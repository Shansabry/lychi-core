//! The agent loop itself — `Coordinator`, its event/outcome types, and the
//! stop-condition trait.
//!
//! Shape: one turn = one model response. The loop streams `provider.chat`,
//! forwards prose deltas live, accumulates the tool calls, then (if the turn
//! requested tools) executes them and re-enters — until the model answers with
//! no tool calls, hits the step cap, or pauses for approval.
//!
//! Stream + outcome split: the loop runs on a spawned task that emits
//! `AgentEvent`s into an mpsc channel (the UI drains it) and resolves a oneshot
//! carrying the final `Outcome`. The UI renders the stream; app logic awaits the
//! outcome to decide whether to prompt for approval and call `resume`.

use std::sync::Arc;

use futures_util::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::error::LychiError;
use crate::providers::{
    AiProvider, CancellationToken, EventStream as ProviderStream, StreamEvent, ToolCall, ToolDef,
};

use super::session::{ApprovalDecision, ApprovalRequest, PendingApproval, Session};
use super::tool_executor::{ResumeToken, ToolExecutor, ToolOutcome};

/// A unified, higher-level event the coordinator emits as it runs. The UI
/// subscribes to THIS (never the provider's raw stream): it multiplexes live
/// prose, tool lifecycle, and results into one stream.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    /// A model turn began (`step` is 0-based).
    TurnStarted { step: usize },
    /// A chunk of assistant prose.
    TextDelta(String),
    /// A chunk of extended-thinking text.
    ReasoningDelta(String),
    /// A tool call is about to run.
    ToolCallStarted {
        call_id: String,
        name: String,
        args: String,
    },
    /// A tool finished. `output` is what was fed back to the model; `artifact`
    /// is an optional rich result (QR/weather/image) the UI renders inline.
    ToolCallCompleted {
        call_id: String,
        output: String,
        artifact: Option<super::ToolArtifact>,
    },
    /// A tool errored (fed back to the model, not fatal).
    ToolCallFailed { call_id: String, error: String },
    /// A tool needs user approval — the loop is suspending.
    AwaitingApproval(ApprovalRequest),
    /// The final assistant answer text (turn ended with no tool calls).
    /// `truncated` = the model hit its token cap mid-answer (cut off, not done).
    Final { text: String, truncated: bool },
    /// Token usage for a completed turn (when the provider reports it). Emitted
    /// per turn; the UI accumulates across a multi-turn conversation.
    Usage {
        input_tokens: u32,
        output_tokens: u32,
    },
    /// The loop stopped on the step cap.
    Stopped { reason: String },
    /// An infrastructure error aborted the loop.
    Error(String),
}

/// The terminal result of a `run`/`resume` — what the caller inspects to decide
/// the next step (prompt for approval, persist the session, or finish).
pub enum Outcome {
    /// The model produced a final answer. Carries the full session (for history).
    Done { session: Session },
    /// A destructive tool needs approval. The caller shows a prompt, then calls
    /// `Coordinator::resume(session, decision, …)`.
    AwaitingApproval {
        request: ApprovalRequest,
        session: Session,
    },
    /// The step cap was hit.
    Stopped { reason: String, session: Session },
    /// An infrastructure error (provider down, cancel, etc.).
    ///
    /// Carries the session whenever the loop still had one: the messages up to
    /// the failed turn — including partial prose already streamed to the user —
    /// are valid context, and the caller must be able to persist and re-stash
    /// them. This variant used to carry only the error, and the caller cleared
    /// the stashed session while the conversation id lived on; the next
    /// follow-up then started an EMPTY session under the same id and its
    /// upsert replaced the stored transcript — one wifi blip mid-answer both
    /// gave the model amnesia and destroyed the recall copy. `None` only when
    /// the loop task itself was lost and there is genuinely nothing to save.
    Error {
        error: LychiError,
        session: Option<Session>,
    },
}

/// The coordinator's event stream — a boxed, `Send + 'static` stream of
/// `AgentEvent`s. Boxed (like the provider's `EventStream`) so callers depend
/// only on `futures`, not on the concrete channel-stream type.
pub type AgentEventStream = futures_util::stream::BoxStream<'static, AgentEvent>;

/// A handle to await the loop's terminal `Outcome`. Returned alongside the event
/// stream; the two are consumed independently (drain the stream to render, await
/// this to branch on the result).
pub struct OutcomeHandle(oneshot::Receiver<Outcome>);

impl OutcomeHandle {
    /// Await the outcome. Returns an `Error` outcome if the loop task vanished.
    pub async fn wait(self) -> Outcome {
        self.0.await.unwrap_or_else(|_| Outcome::Error {
            error: LychiError::Ai("agent loop task dropped".into()),
            session: None,
        })
    }
}

/// Decides when to stop looping. A one-line seam so "stop after N steps" can
/// later become "stop when tool X ran" without touching the loop.
pub trait StopCondition: Send + Sync {
    /// `step` is the 0-based turn about to run. Return true to stop before it.
    fn should_stop(&self, session: &Session, step: usize) -> bool;
}

/// The default: stop after a fixed number of model turns. A low cap for a
/// launcher (tasks rarely need more than a few tool round-trips) — bounded
/// latency + runaway safety.
pub struct MaxSteps(pub usize);
impl StopCondition for MaxSteps {
    fn should_stop(&self, _session: &Session, step: usize) -> bool {
        step >= self.0
    }
}

/// The agent coordinator. Generic over the `ToolExecutor` so the whole loop is
/// unit-testable with a mock. Cheap to construct per run.
pub struct Coordinator<E: ToolExecutor + 'static> {
    provider: Arc<dyn AiProvider>,
    executor: Arc<E>,
    tools: Vec<ToolDef>,
    stop: Arc<dyn StopCondition>,
}

impl<E: ToolExecutor + 'static> Coordinator<E> {
    pub fn new(provider: Arc<dyn AiProvider>, executor: Arc<E>, tools: Vec<ToolDef>) -> Self {
        Self {
            provider,
            executor,
            tools,
            stop: Arc::new(MaxSteps(12)),
        }
    }

    /// Override the stop condition (default `MaxSteps(12)`).
    pub fn with_stop(mut self, stop: Arc<dyn StopCondition>) -> Self {
        self.stop = stop;
        self
    }

    /// Start the loop on `session`. Returns the `AgentEvent` stream (drain to
    /// render) and an `OutcomeHandle` (await to branch). The loop runs on a
    /// spawned task; dropping the stream + handle, or cancelling `cancel`, stops
    /// it.
    pub fn run(
        &self,
        session: Session,
        cancel: CancellationToken,
    ) -> (AgentEventStream, OutcomeHandle) {
        self.spawn_loop(session, None, cancel)
    }

    /// Resume after the user decided on a pending approval. Applies the decision
    /// (run the approved tool / feed a rejection back), then continues the loop.
    pub fn resume(
        &self,
        session: Session,
        decision: ApprovalDecision,
        cancel: CancellationToken,
    ) -> (AgentEventStream, OutcomeHandle) {
        self.spawn_loop(session, Some(decision), cancel)
    }

    fn spawn_loop(
        &self,
        session: Session,
        decision: Option<ApprovalDecision>,
        cancel: CancellationToken,
    ) -> (AgentEventStream, OutcomeHandle) {
        // UNBOUNDED on purpose: `emit` must NEVER block the loop's reading of the
        // provider's HTTP stream. With a bounded channel, a slow/hidden webview
        // consumer fills the buffer, `emit().await` stalls between stream reads,
        // the unread HTTP/2 body triggers a flow-control stall, and the server
        // resets the stream (RST_STREAM CANCEL) — truncating the answer. Draining
        // the network at full speed into an unbounded buffer avoids that; the
        // buffer is naturally bounded by one response's worth of deltas.
        let (ev_tx, ev_rx) = mpsc::unbounded_channel::<AgentEvent>();
        let (out_tx, out_rx) = oneshot::channel::<Outcome>();

        let provider = self.provider.clone();
        let executor = self.executor.clone();
        let tools = self.tools.clone();
        let stop = self.stop.clone();

        tokio::spawn(async move {
            let ctx = LoopCtx {
                provider,
                executor,
                tools,
                stop,
                ev_tx,
                cancel,
            };
            let outcome = ctx.drive(session, decision).await;
            let _ = out_tx.send(outcome);
        });

        (
            UnboundedReceiverStream::new(ev_rx).boxed(),
            OutcomeHandle(out_rx),
        )
    }
}

/// Owned context the spawned loop task runs against (everything is `'static`).
struct LoopCtx<E: ToolExecutor + 'static> {
    provider: Arc<dyn AiProvider>,
    executor: Arc<E>,
    tools: Vec<ToolDef>,
    stop: Arc<dyn StopCondition>,
    ev_tx: mpsc::UnboundedSender<AgentEvent>,
    cancel: CancellationToken,
}

impl<E: ToolExecutor + 'static> LoopCtx<E> {
    fn emit(&self, ev: AgentEvent) {
        // Non-blocking (unbounded) — never stalls the HTTP read. Err = the UI
        // dropped the stream; harmless, the loop still finishes for persistence.
        let _ = self.ev_tx.send(ev);
    }

    /// The core loop. `resume_decision` applies a pending approval first (on a
    /// `resume` call), then it runs model turns until done / stopped / suspended.
    async fn drive(
        &self,
        mut session: Session,
        resume_decision: Option<ApprovalDecision>,
    ) -> Outcome {
        // ── Apply a resume decision (if this is a resume) ────────────────────
        if let Some(decision) = resume_decision
            && let Some(outcome) = self.apply_decision(&mut session, decision).await
        {
            return outcome; // a nested approval or error surfaced
        }
        // else: results appended, fall through to continue the loop

        // ── The turn loop ────────────────────────────────────────────────────
        let mut step = 0usize;
        loop {
            if self.cancel.is_cancelled() {
                // Esc ends the TURN, not the conversation — the session (with
                // whatever partial answer the previous consume_turn kept) goes
                // back to the caller for persist + re-stash.
                return Outcome::Error {
                    error: LychiError::Ai("cancelled".into()),
                    session: Some(session),
                };
            }
            if self.stop.should_stop(&session, step) {
                let reason = format!("reached step limit ({step})");
                self.emit(AgentEvent::Stopped {
                    reason: reason.clone(),
                });
                return Outcome::Stopped { reason, session };
            }
            self.emit(AgentEvent::TurnStarted { step });

            // Stream one model turn, forwarding prose + collecting tool calls.
            let turn = match self.consume_turn(&session.messages).await {
                Ok(t) => t,
                Err((e, partial)) => {
                    // The prose that already streamed is on the user's screen —
                    // it is context the model must keep. Push it before
                    // surfacing the error, or the follow-up answers as if the
                    // interrupted reply never happened.
                    if !partial.is_empty() {
                        session.push_assistant(partial, Vec::new());
                    }
                    self.emit(AgentEvent::Error(e.to_string()));
                    return Outcome::Error {
                        error: e,
                        session: Some(session),
                    };
                }
            };
            let (text, calls, truncated) = (turn.text, turn.tool_calls, turn.truncated);
            session.push_assistant(text.clone(), calls.clone());

            // No tool calls → final answer, done.
            if calls.is_empty() {
                self.emit(AgentEvent::Final { text, truncated });
                return Outcome::Done { session };
            }

            // Execute each requested tool; suspend on the first that needs approval.
            for call in calls {
                if let Some(outcome) = self.run_one_tool(&mut session, call).await {
                    return outcome;
                }
            }
            step += 1;
            // Loop re-enters with the tool results now in session.messages.
        }
    }

    /// Stream one provider turn: forward `TextDelta`/`ReasoningDelta`, accumulate
    /// tool calls, and return them once the stream ends.
    ///
    /// On a stream error the text accumulated BEFORE it comes back alongside
    /// the error — that prose already streamed to the user, and the caller
    /// preserves it in the session rather than pretending it never happened.
    async fn consume_turn(
        &self,
        messages: &[crate::providers::ChatMessage],
    ) -> Result<TurnResult, (LychiError, String)> {
        let mut stream: ProviderStream =
            self.provider
                .chat(messages, &self.tools, self.cancel.clone());
        let mut text = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut truncated = false;

        while let Some(item) = stream.next().await {
            if self.cancel.is_cancelled() {
                break;
            }
            let event = match item {
                Ok(ev) => ev,
                Err(e) => return Err((e, text)),
            };
            match event {
                StreamEvent::MessageStart { .. } => {}
                StreamEvent::TextDelta(d) => {
                    text.push_str(&d);
                    self.emit(AgentEvent::TextDelta(d));
                }
                StreamEvent::ReasoningDelta(d) => {
                    self.emit(AgentEvent::ReasoningDelta(d));
                }
                // The provider already assembles complete calls; the delta
                // variants are for live "typing args…" UI, which we skip here.
                StreamEvent::ToolCallStart { .. } | StreamEvent::ToolCallArgsDelta { .. } => {}
                StreamEvent::ToolCallComplete { id, name, args } => {
                    calls.push(ToolCall { id, name, args });
                }
                StreamEvent::Done { stop_reason, usage } => {
                    // ToolUse vs EndTurn is implied by calls.is_empty(); MaxTokens
                    // means the answer was cut off at the token cap.
                    truncated = matches!(stop_reason, crate::providers::StopReason::MaxTokens);
                    if let Some(u) = usage {
                        self.emit(AgentEvent::Usage {
                            input_tokens: u.input_tokens,
                            output_tokens: u.output_tokens,
                        });
                    }
                    break;
                }
            }
        }
        Ok(TurnResult {
            text,
            tool_calls: calls,
            truncated,
        })
    }

    /// Execute one tool call. Appends the result to `session` and returns `None`
    /// to continue, or `Some(Outcome)` to suspend (approval) / abort (infra err).
    async fn run_one_tool(&self, session: &mut Session, call: ToolCall) -> Option<Outcome> {
        self.emit(AgentEvent::ToolCallStarted {
            call_id: call.id.clone(),
            name: call.name.clone(),
            args: call.args.clone(),
        });

        match self.executor.execute(&call.name, &call.args).await {
            Ok(ToolOutcome::Ran {
                output,
                is_error,
                artifact,
            }) => {
                // Only the text `output` goes into the model's context; the rich
                // `artifact` (if any) rides the event to the UI for inline render.
                session.push_tool_result(&call.id, output.clone(), is_error);
                if is_error {
                    self.emit(AgentEvent::ToolCallFailed {
                        call_id: call.id,
                        error: output,
                    });
                } else {
                    self.emit(AgentEvent::ToolCallCompleted {
                        call_id: call.id,
                        output,
                        artifact,
                    });
                }
                None
            }
            Ok(ToolOutcome::NeedsApproval { reason, resume }) => {
                let request = ApprovalRequest {
                    call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    args: call.args.clone(),
                    reason: reason.clone(),
                };
                // Record the pending call in the session so resume can run it.
                session.pending.push(PendingApproval {
                    call,
                    reason,
                    resume,
                });
                self.emit(AgentEvent::AwaitingApproval(request.clone()));
                Some(Outcome::AwaitingApproval {
                    request,
                    session: session.clone(),
                })
            }
            Err(e) => {
                self.emit(AgentEvent::Error(e.to_string()));
                Some(Outcome::Error {
                    error: e,
                    session: Some(session.clone()),
                })
            }
        }
    }

    /// Apply the user's decision to the (single) pending approval. Returns
    /// `Some(Outcome)` if it surfaced another approval or an error; `None` if the
    /// result was appended and the loop should continue.
    async fn apply_decision(
        &self,
        session: &mut Session,
        decision: ApprovalDecision,
    ) -> Option<Outcome> {
        let Some(pending) = session.pending.pop() else {
            return None; // nothing pending — nothing to do
        };
        let call_id = pending.call.id.clone();
        match decision {
            ApprovalDecision::Approve | ApprovalDecision::ApproveWithEdit { .. } => {
                // (Edit path would re-assess with new args; for now approve runs
                // the exact assessed action via the resume token.)
                let resume: ResumeToken = pending.resume;
                match self.executor.run_approved(resume).await {
                    Ok(output) => {
                        session.push_tool_result(&call_id, output.clone(), false);
                        // Approval-resume runs run_approved which returns only text;
                        // no artifact on this path (a rich artifact would need
                        // run_approved to return one — not needed for current tools).
                        self.emit(AgentEvent::ToolCallCompleted {
                            call_id,
                            output,
                            artifact: None,
                        });
                        None
                    }
                    Err(e) => {
                        // A tool-logic failure feeds back; an infra failure aborts.
                        session.push_tool_result(&call_id, e.to_string(), true);
                        self.emit(AgentEvent::ToolCallFailed {
                            call_id,
                            error: e.to_string(),
                        });
                        None
                    }
                }
            }
            ApprovalDecision::Reject { message } => {
                let msg = message.unwrap_or_else(|| "User declined this action.".to_string());
                session.push_tool_result(&call_id, msg.clone(), true);
                self.emit(AgentEvent::ToolCallFailed {
                    call_id,
                    error: msg,
                });
                None
            }
        }
    }
}

struct TurnResult {
    text: String,
    tool_calls: Vec<ToolCall>,
    /// The model hit its `max_tokens` cap — the answer is cut off, not complete.
    truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{AiResponse, AiRoute};
    use async_trait::async_trait;
    use futures_util::stream;
    use std::sync::Mutex;

    // ── Mock provider ────────────────────────────────────────────────────────
    // Scripts a sequence of turns. Each turn is a Vec<StreamEvent> the provider
    // replays. `chat` pops the next scripted turn — so the loop drives real
    // multi-turn behavior with zero network / real model.
    struct MockProvider {
        turns: Mutex<std::collections::VecDeque<Vec<StreamEvent>>>,
        /// Records the messages passed on each `chat` call (to assert the loop
        /// fed tool results back).
        seen: Mutex<Vec<usize>>,
    }
    impl MockProvider {
        fn new(turns: Vec<Vec<StreamEvent>>) -> Arc<Self> {
            Arc::new(Self {
                turns: Mutex::new(turns.into()),
                seen: Mutex::new(Vec::new()),
            })
        }
    }
    #[async_trait]
    impl AiProvider for MockProvider {
        async fn route_intent(&self, _: &str, _: &[&str]) -> Result<AiRoute, LychiError> {
            unreachable!()
        }
        async fn route_or_plan(
            &self,
            _: &str,
            _: &[&str],
            _: Option<&str>,
        ) -> Result<AiResponse, LychiError> {
            unreachable!()
        }
        async fn health_check(&self) -> bool {
            true
        }
        fn name(&self) -> &str {
            "mock"
        }
        fn chat(
            &self,
            messages: &[crate::providers::ChatMessage],
            _tools: &[ToolDef],
            _cancel: CancellationToken,
        ) -> ProviderStream {
            self.seen.lock().unwrap().push(messages.len());
            let events = self.turns.lock().unwrap().pop_front().unwrap_or_else(|| {
                vec![
                    StreamEvent::TextDelta("(no more scripted turns)".into()),
                    StreamEvent::Done {
                        stop_reason: crate::providers::StopReason::EndTurn,
                        usage: None,
                    },
                ]
            });
            stream::iter(events.into_iter().map(Ok)).boxed()
        }
    }

    // Helpers to script turns.
    fn answer(text: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::TextDelta(text.into()),
            StreamEvent::Done {
                stop_reason: crate::providers::StopReason::EndTurn,
                usage: None,
            },
        ]
    }
    fn call_tool(id: &str, name: &str, args: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ToolCallComplete {
                id: id.into(),
                name: name.into(),
                args: args.into(),
            },
            StreamEvent::Done {
                stop_reason: crate::providers::StopReason::ToolUse,
                usage: None,
            },
        ]
    }

    // ── Mock executor ────────────────────────────────────────────────────────
    // Maps tool name → a scripted ToolOutcome. `run_approved` echoes a fixed
    // string so approve-resume is observable.
    struct MockExecutor {
        outcomes: std::collections::HashMap<String, ToolOutcome>,
        approved_output: String,
        approved_calls: Mutex<usize>,
    }
    impl MockExecutor {
        fn new() -> Self {
            Self {
                outcomes: Default::default(),
                approved_output: "APPROVED-RAN".into(),
                approved_calls: Mutex::new(0),
            }
        }
        fn ran(mut self, name: &str, output: &str, is_error: bool) -> Self {
            self.outcomes.insert(
                name.into(),
                ToolOutcome::Ran {
                    output: output.into(),
                    is_error,
                    artifact: None,
                },
            );
            self
        }
        fn needs_approval(mut self, name: &str, reason: &str) -> Self {
            self.outcomes.insert(
                name.into(),
                ToolOutcome::NeedsApproval {
                    reason: reason.into(),
                    resume: ResumeToken(serde_json::json!({"tool": name})),
                },
            );
            self
        }
    }
    #[async_trait]
    impl ToolExecutor for MockExecutor {
        async fn execute(&self, name: &str, _args: &str) -> Result<ToolOutcome, LychiError> {
            match self.outcomes.get(name) {
                Some(ToolOutcome::Ran {
                    output, is_error, ..
                }) => Ok(ToolOutcome::Ran {
                    output: output.clone(),
                    is_error: *is_error,
                    artifact: None,
                }),
                Some(ToolOutcome::NeedsApproval { reason, resume }) => {
                    Ok(ToolOutcome::NeedsApproval {
                        reason: reason.clone(),
                        resume: resume.clone(),
                    })
                }
                None => Err(LychiError::Ai(format!("mock: unknown tool {name}"))),
            }
        }
        async fn run_approved(&self, _resume: ResumeToken) -> Result<String, LychiError> {
            *self.approved_calls.lock().unwrap() += 1;
            Ok(self.approved_output.clone())
        }
    }

    fn coordinator(
        provider: Arc<MockProvider>,
        executor: Arc<MockExecutor>,
    ) -> Coordinator<MockExecutor> {
        Coordinator::new(
            provider,
            executor,
            vec![ToolDef {
                name: "weather".into(),
                description: "get weather".into(),
            }],
        )
    }

    // Drain the event stream into a Vec (for assertions).
    async fn drain(mut s: AgentEventStream) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Some(e) = s.next().await {
            out.push(e);
        }
        out
    }

    // Predicate helpers (avoids guard-in-`matches!`, unstable in this edition).
    fn has_text(events: &[AgentEvent], want: &str) -> bool {
        events.iter().any(|e| {
            if let AgentEvent::TextDelta(t) = e {
                t == want
            } else {
                false
            }
        })
    }
    fn final_text(events: &[AgentEvent]) -> Option<&str> {
        events.iter().rev().find_map(|e| match e {
            AgentEvent::Final { text, .. } => Some(text.as_str()),
            _ => None,
        })
    }
    fn has_tool_started(events: &[AgentEvent], name: &str) -> bool {
        events.iter().any(|e| {
            if let AgentEvent::ToolCallStarted { name: n, .. } = e {
                n == name
            } else {
                false
            }
        })
    }
    fn tool_completed_output(events: &[AgentEvent]) -> Option<&str> {
        events.iter().find_map(|e| {
            if let AgentEvent::ToolCallCompleted { output, .. } = e {
                Some(output.as_str())
            } else {
                None
            }
        })
    }
    fn tool_failed_error(events: &[AgentEvent]) -> Option<&str> {
        events.iter().find_map(|e| {
            if let AgentEvent::ToolCallFailed { error, .. } = e {
                Some(error.as_str())
            } else {
                None
            }
        })
    }
    fn awaiting_tool(events: &[AgentEvent]) -> Option<&str> {
        events.iter().find_map(|e| {
            if let AgentEvent::AwaitingApproval(r) = e {
                Some(r.tool_name.as_str())
            } else {
                None
            }
        })
    }

    #[tokio::test]
    async fn plain_answer_no_tools() {
        let provider = MockProvider::new(vec![answer("Hello there")]);
        let exec = Arc::new(MockExecutor::new());
        let (stream, handle) =
            coordinator(provider, exec).run(Session::new("sys", "hi"), CancellationToken::new());
        let events = drain(stream).await;
        // TurnStarted, TextDelta, Final
        assert!(matches!(
            events.first(),
            Some(AgentEvent::TurnStarted { step: 0 })
        ));
        assert!(has_text(&events, "Hello there"));
        assert_eq!(final_text(&events), Some("Hello there"));
        match handle.wait().await {
            Outcome::Done { session } => {
                // system, user, assistant
                assert_eq!(session.messages.len(), 3);
            }
            _ => panic!("expected Done"),
        }
    }

    #[tokio::test]
    async fn tool_call_then_answer_loops_and_feeds_result_back() {
        // Turn 1: call weather. Turn 2: final answer.
        let provider = MockProvider::new(vec![
            call_tool("t1", "weather", "London"),
            answer("It's sunny in London."),
        ]);
        let exec = Arc::new(MockExecutor::new().ran("weather", "18C sunny", false));
        let p = provider.clone();
        let (stream, handle) = coordinator(provider, exec)
            .run(Session::new("sys", "weather?"), CancellationToken::new());
        let events = drain(stream).await;

        assert!(has_tool_started(&events, "weather"));
        assert_eq!(tool_completed_output(&events), Some("18C sunny"));
        assert!(final_text(&events).unwrap().contains("sunny"));

        // The 2nd chat call must have seen MORE messages than the first (the tool
        // result + assistant turn were fed back).
        let seen = p.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "provider called twice (two turns)");
        assert!(
            seen[1] > seen[0],
            "second turn sees the appended tool result"
        );

        match handle.wait().await {
            Outcome::Done { session } => {
                // sys, user, assistant(tool_call), tool_result, assistant(final)
                assert_eq!(session.messages.len(), 5);
            }
            _ => panic!("expected Done"),
        }
    }

    #[tokio::test]
    async fn tool_error_is_fed_back_not_fatal() {
        let provider = MockProvider::new(vec![
            call_tool("t1", "weather", "Nowhere"),
            answer("Sorry, that failed."),
        ]);
        let exec = Arc::new(MockExecutor::new().ran("weather", "network down", true));
        let (stream, handle) = coordinator(provider, exec)
            .run(Session::new("sys", "weather?"), CancellationToken::new());
        let events = drain(stream).await;
        assert_eq!(tool_failed_error(&events), Some("network down"));
        // The loop CONTINUED (didn't abort) — a Final answer arrived.
        assert!(matches!(events.last(), Some(AgentEvent::Final { .. })));
        assert!(matches!(handle.wait().await, Outcome::Done { .. }));
    }

    #[tokio::test]
    async fn destructive_tool_suspends_for_approval() {
        let provider = MockProvider::new(vec![call_tool("t1", "delete", "all")]);
        let exec =
            Arc::new(MockExecutor::new().needs_approval("delete", "This deletes everything"));
        let coord = Coordinator::new(
            provider,
            exec,
            vec![ToolDef {
                name: "delete".into(),
                description: "delete".into(),
            }],
        );
        let (stream, handle) =
            coord.run(Session::new("sys", "delete all"), CancellationToken::new());
        let events = drain(stream).await;
        assert_eq!(awaiting_tool(&events), Some("delete"));
        match handle.wait().await {
            Outcome::AwaitingApproval { request, session } => {
                assert_eq!(request.reason, "This deletes everything");
                assert_eq!(
                    session.pending.len(),
                    1,
                    "the paused call is recorded for resume"
                );
            }
            _ => panic!("expected AwaitingApproval"),
        }
    }

    #[tokio::test]
    async fn approve_resumes_without_rerunning_and_continues() {
        // Set up a suspended session by running to the approval point.
        let provider = MockProvider::new(vec![call_tool("t1", "delete", "all")]);
        let exec = Arc::new(MockExecutor::new().needs_approval("delete", "confirm?"));
        let coord = Coordinator::new(
            provider,
            exec.clone(),
            vec![ToolDef {
                name: "delete".into(),
                description: "delete".into(),
            }],
        );
        let (s1, h1) = coord.run(Session::new("sys", "delete all"), CancellationToken::new());
        drain(s1).await;
        let session = match h1.wait().await {
            Outcome::AwaitingApproval { session, .. } => session,
            _ => panic!("expected suspend"),
        };

        // Resume with Approve — a NEW provider that answers after the approved tool.
        let provider2 = MockProvider::new(vec![answer("Deleted, done.")]);
        let coord2 = Coordinator::new(
            provider2,
            exec.clone(),
            vec![ToolDef {
                name: "delete".into(),
                description: "delete".into(),
            }],
        );
        let (s2, h2) = coord2.resume(session, ApprovalDecision::Approve, CancellationToken::new());
        let events = drain(s2).await;
        assert_eq!(tool_completed_output(&events), Some("APPROVED-RAN"));
        assert!(final_text(&events).unwrap().contains("Deleted"));
        // run_approved was called exactly once (no double-execution).
        assert_eq!(*exec.approved_calls.lock().unwrap(), 1);
        assert!(matches!(h2.wait().await, Outcome::Done { .. }));
    }

    #[tokio::test]
    async fn reject_feeds_denial_back_to_model() {
        let provider = MockProvider::new(vec![call_tool("t1", "delete", "all")]);
        let exec = Arc::new(MockExecutor::new().needs_approval("delete", "confirm?"));
        let coord = Coordinator::new(
            provider,
            exec.clone(),
            vec![ToolDef {
                name: "delete".into(),
                description: "delete".into(),
            }],
        );
        let (s1, h1) = coord.run(Session::new("sys", "delete all"), CancellationToken::new());
        drain(s1).await;
        let session = match h1.wait().await {
            Outcome::AwaitingApproval { session, .. } => session,
            _ => panic!(),
        };

        let provider2 = MockProvider::new(vec![answer("Okay, I won't.")]);
        let coord2 = Coordinator::new(
            provider2,
            exec.clone(),
            vec![ToolDef {
                name: "delete".into(),
                description: "delete".into(),
            }],
        );
        let (s2, h2) = coord2.resume(
            session,
            ApprovalDecision::Reject {
                message: Some("User said no".into()),
            },
            CancellationToken::new(),
        );
        let events = drain(s2).await;
        assert_eq!(tool_failed_error(&events), Some("User said no"));
        // run_approved NOT called (rejected).
        assert_eq!(*exec.approved_calls.lock().unwrap(), 0);
        // A denial tool-result was appended, and the model got another turn.
        match h2.wait().await {
            Outcome::Done { session } => {
                assert!(
                    session
                        .messages
                        .iter()
                        .any(|m| m.is_error && m.content_text() == "User said no")
                );
            }
            _ => panic!("expected Done"),
        }
    }

    #[tokio::test]
    async fn step_cap_stops_a_runaway_loop() {
        // A provider that ALWAYS calls a tool → would loop forever without the cap.
        let turns: Vec<Vec<StreamEvent>> =
            (0..20).map(|_| call_tool("t", "weather", "x")).collect();
        let provider = MockProvider::new(turns);
        let exec = Arc::new(MockExecutor::new().ran("weather", "ok", false));
        let coord = coordinator(provider, exec).with_stop(Arc::new(MaxSteps(3)));
        let (stream, handle) = coord.run(Session::new("sys", "go"), CancellationToken::new());
        let events = drain(stream).await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Stopped { .. }))
        );
        assert!(matches!(handle.wait().await, Outcome::Stopped { .. }));
    }

    #[tokio::test]
    async fn cancellation_aborts_the_loop() {
        let provider = MockProvider::new(vec![call_tool("t", "weather", "x"), answer("done")]);
        let exec = Arc::new(MockExecutor::new().ran("weather", "ok", false));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (stream, handle) = coordinator(provider, exec).run(Session::new("sys", "go"), cancel);
        drain(stream).await;
        // Esc ends the turn, not the conversation: the session must come back.
        match handle.wait().await {
            Outcome::Error { session, .. } => {
                assert!(session.is_some(), "cancel must return the session")
            }
            _ => panic!("expected Error"),
        }
    }

    /// A provider whose stream fails mid-answer, after some prose streamed.
    struct FailingProvider;
    #[async_trait]
    impl AiProvider for FailingProvider {
        async fn route_intent(&self, _: &str, _: &[&str]) -> Result<AiRoute, LychiError> {
            unreachable!()
        }
        async fn route_or_plan(
            &self,
            _: &str,
            _: &[&str],
            _: Option<&str>,
        ) -> Result<AiResponse, LychiError> {
            unreachable!()
        }
        async fn health_check(&self) -> bool {
            true
        }
        fn name(&self) -> &str {
            "failing"
        }
        fn chat(
            &self,
            _: &[crate::providers::ChatMessage],
            _: &[ToolDef],
            _: CancellationToken,
        ) -> ProviderStream {
            futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta("partial answer ".into())),
                Ok(StreamEvent::TextDelta("the user already read".into())),
                Err(LychiError::Ai("stream error: connection reset".into())),
            ])
            .boxed()
        }
    }

    /// AI-1: a mid-stream infrastructure error must hand the session back WITH
    /// the partial prose. The old shape returned only the error; the caller
    /// cleared its stashed session while the conversation id survived, so the
    /// next follow-up upserted an empty session over the stored transcript —
    /// one wifi blip destroyed the conversation twice (context AND recall).
    #[tokio::test]
    async fn a_stream_error_preserves_the_session_and_partial_text() {
        let exec = Arc::new(MockExecutor::new());
        let coord = Coordinator::new(
            Arc::new(FailingProvider) as Arc<dyn AiProvider>,
            exec,
            Vec::new(),
        );
        let (stream, handle) = coord.run(Session::new("sys", "go"), CancellationToken::new());
        let events = drain(stream).await;
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::Error(_))),
            "the error must still surface to the UI"
        );
        match handle.wait().await {
            Outcome::Error { session, .. } => {
                let session = session.expect("session must survive a stream error");
                let last = session.messages.last().expect("messages not empty");
                assert_eq!(
                    last.content_text(),
                    "partial answer the user already read",
                    "the streamed prose must be preserved as an assistant turn"
                );
            }
            _ => panic!("expected Error outcome"),
        }
    }
}
