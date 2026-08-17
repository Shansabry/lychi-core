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
    /// A chunk of live output from a still-running tool (e.g. a shell command's
    /// stdout/stderr, streamed line-by-line). Purely additive UI sugar — the
    /// final, complete output still arrives in `ToolCallCompleted`; these let the
    /// user watch the work happen instead of waiting for the end. Not every tool
    /// streams; those that don't simply never emit this.
    ToolOutputDelta { call_id: String, chunk: String },
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
        /// How many input tokens were prompt-cache hits (the two-tier caching,
        /// made visible). 0 when the provider doesn't report it.
        cached_input_tokens: u32,
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
    /// The FULL tool catalog. The model-facing shortlist is re-selected from it
    /// each turn (see the turn loop); this field stays the complete set so
    /// re-selection and `is_mutating` always see every tool.
    tools: Vec<ToolDef>,
    stop: Arc<dyn StopCondition>,
}

impl<E: ToolExecutor + 'static> Coordinator<E> {
    pub fn new(provider: Arc<dyn AiProvider>, executor: Arc<E>, mut tools: Vec<ToolDef>) -> Self {
        // A tool-bearing agent always gets the discovery pseudo-tool: shortlist
        // filtering is fail-safe only if the model can search the full catalog
        // when nothing visible fits. Answered inline by the loop, never the
        // executor. A tool-less chat gets no tools at all, discovery included.
        if !tools.is_empty()
            && !tools
                .iter()
                .any(|t| t.name == crate::coordinator::relevance::FIND_TOOL)
        {
            tools.push(crate::coordinator::relevance::find_tool_def());
        }
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
        // One free retry each for the two provider-flake classes (see below):
        // a fully-empty turn, and a mid-stream rejection before any prose.
        let mut retried_empty_turn = false;
        let mut retried_flaky_turn = false;
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

            // Send only the tools this conversation has plausibly needed:
            // re-selected from the CURRENT conversation each turn (a later step
            // reaches tools its own context now implies), but append-only across
            // the conversation — once sent, a schema stays visible, so history
            // never references a tool the model can no longer see and the
            // request prefix only grows. Fail-safe (core + find_tool always
            // present, full catalog on vague/broad queries), and the executor
            // still runs any tool by name. See `relevance`.
            let step_tools = crate::coordinator::select_tools_sticky(
                &session.messages,
                &self.tools,
                &mut session.sent_tools,
            );

            // Stream one model turn, forwarding prose + collecting tool calls.
            let turn = match self.consume_turn(&session.messages, &step_tools).await {
                Ok(t) => t,
                Err((e, partial)) => {
                    // A MID-STREAM provider rejection with nothing on screen yet
                    // is a re-rollable flake, not a verdict — gpt-oss on Groq
                    // sometimes leaks its harmony channel marker into a tool
                    // name ("web_tools<|channel|>commentary"), which the
                    // provider's validator rejects. The same request typically
                    // succeeds on the next roll. Only when the wire tagged it as
                    // an in-band provider error (pre-stream failures like auth
                    // or rate limits already have their own handling), and only
                    // when no prose streamed (a retry must not double-print).
                    if partial.is_empty()
                        && !retried_flaky_turn
                        && e.to_string().contains("provider reported an error")
                    {
                        retried_flaky_turn = true;
                        tracing::warn!("[agent] mid-stream provider error — retrying once: {e}");
                        continue;
                    }
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
            // Repair recognizably-mangled tool names BEFORE the batch runs and
            // BEFORE the assistant turn enters history — the stored call must
            // teach the model the correct shape, not echo the mangling back.
            let calls: Vec<ToolCall> = calls
                .into_iter()
                .map(|c| normalize_tool_call(c, &self.tools))
                .collect();

            // A COMPLETELY empty turn (no prose, no calls, not a token-cap cut)
            // is a provider flake, not an answer — Groq has returned 0-token
            // streams right after a tool result, and treating that as the final
            // answer ended the conversation in silence with the user staring at
            // a finished tool call. Retry once; then fail honestly.
            if text.trim().is_empty() && calls.is_empty() && !truncated {
                if !retried_empty_turn {
                    retried_empty_turn = true;
                    tracing::warn!("[agent] provider returned an empty turn — retrying once");
                    continue;
                }
                let error = LychiError::Ai(
                    "The AI returned an empty response twice — try again in a moment.".into(),
                );
                self.emit(AgentEvent::Error(error.to_string()));
                return Outcome::Error {
                    error,
                    session: Some(session),
                };
            }
            session.push_assistant(text.clone(), calls.clone());

            // No tool calls → final answer, done.
            if calls.is_empty() {
                self.emit(AgentEvent::Final { text, truncated });
                return Outcome::Done { session };
            }

            // Filter the batch the model emitted in ONE turn, in order:
            //   1. Exact dedup — same tool name + same args. Fast models
            //      sometimes request an identical tool twice; running a side
            //      effect twice is never wanted and doubles token cost.
            //   2. Mutating-tool hold — at most ONE state-mutating tool
            //      (`run`, `zip`, `service`, …) runs per turn. A model that
            //      hedges by emitting several VARIANTS of one destructive
            //      operation at once (three ways to resize the same photos —
            //      convert vs magick, differing only by flags, so exact dedup
            //      can't catch them) would otherwise run all of them: wasted
            //      tokens → rate limits, and a corrupted result. The
            //      "don't parallelize non-idempotent tools" principle.
            // Read-only tools stay fully parallel (two file searches, two
            // definitions in one turn are fine). Both are per-turn only — a
            // later turn legitimately re-calling a tool is untouched. Every
            // held call is answered with a tool_result so the provider contract
            // holds (each tool_use id gets a result).
            let calls = self.filter_batch(&mut session, calls);

            // Discovery calls are answered inline — find_tool is the
            // coordinator's own pseudo-tool (the executor has never heard of
            // it): search the FULL catalog, answer with the matches, and widen
            // the session's sent set so the matched schemas are callable on the
            // very next step.
            let calls: Vec<ToolCall> = calls
                .into_iter()
                .filter_map(|call| {
                    if call.name == crate::coordinator::relevance::FIND_TOOL {
                        self.answer_find_tool(&mut session, call);
                        None
                    } else {
                        Some(call)
                    }
                })
                .collect();

            // Execute the batch IN FULL before suspending. The provider
            // contract is unforgiving here: every tool_use id in the assistant
            // turn must have a matching tool_result in the conversation before
            // the next request, or Anthropic and OpenAI both reject the whole
            // conversation (400) — permanently, because the broken turn is
            // baked into the history. An earlier version returned on the FIRST
            // NeedsApproval, dropping every sibling call after it: never run,
            // never queued, never answered. One approval in a parallel-tool
            // turn then wedged the conversation for good.
            //
            // So: safe siblings run now; approval-needing ones queue in
            // session.pending (surfaced to the user one at a time, in order);
            // and an infra error mid-batch synthesizes error results for
            // everything not yet answered rather than leaving danglers.
            // Read-only siblings run CONCURRENTLY (join_all: results come back
            // in call order); the single mutating call (filter_batch caps the
            // batch at one) runs after them, alone. Ordering rationale: same-turn
            // calls are independent by tool-calling convention, so reads observing
            // pre-mutation state is correct; and if the mutating call suspends on
            // approval, every read has already answered its tool_use id.
            let (mutating, reads): (Vec<ToolCall>, Vec<ToolCall>) =
                calls.into_iter().partition(|c| self.is_mutating(c));

            let read_results =
                futures_util::future::join_all(reads.iter().map(|c| self.execute_tool_call(c)))
                    .await;

            let mut first_request: Option<ApprovalRequest> = None;
            let mut batch_error: Option<LychiError> = None;
            for (call, result) in reads.into_iter().zip(read_results) {
                // Fold EVERY completed result even after one errored — these
                // tools already ran, and their tool_use ids must be answered.
                match self.fold_outcome(&mut session, call, result) {
                    Ok(None) => {}
                    Ok(Some(request)) => {
                        if first_request.is_none() {
                            first_request = Some(request);
                        }
                    }
                    Err(error) => {
                        if batch_error.is_none() {
                            batch_error = Some(error);
                        }
                    }
                }
            }

            if batch_error.is_none() {
                for call in mutating {
                    match self.run_one_tool(&mut session, call).await {
                        Ok(None) => {}
                        Ok(Some(request)) => {
                            if first_request.is_none() {
                                first_request = Some(request);
                            }
                        }
                        Err(error) => {
                            batch_error = Some(error);
                            break;
                        }
                    }
                }
            } else {
                // A read errored: the mutating call must not run on a failed
                // premise. Answer its tool_use id so nothing dangles.
                for c in mutating {
                    session.push_tool_result(
                        &c.id,
                        "not run: an earlier tool call in this batch failed".into(),
                        true,
                    );
                }
            }

            if let Some(error) = batch_error {
                // The failing call already got its error tool_result in
                // fold_outcome. Answer any approvals queued in this batch (an
                // Error outcome never reaches apply_decision, so their pending
                // entries would otherwise never produce a result).
                let queued: Vec<String> = session.pending.drain(..).map(|p| p.call.id).collect();
                for id in queued {
                    session.push_tool_result(
                        &id,
                        "not run: the batch failed before approval could be requested".into(),
                        true,
                    );
                }
                self.emit(AgentEvent::Error(error.to_string()));
                return Outcome::Error {
                    error,
                    session: Some(session),
                };
            }
            if let Some(request) = first_request {
                self.emit(AgentEvent::AwaitingApproval(request.clone()));
                return Outcome::AwaitingApproval {
                    request,
                    session: session.clone(),
                };
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
        tools: &[ToolDef],
    ) -> Result<TurnResult, (LychiError, String)> {
        let mut stream: ProviderStream = self.provider.chat(messages, tools, self.cancel.clone());
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
                        // Visible confirmation the prompt cache is (or isn't)
                        // hitting — the whole point of the stable-catalog design.
                        let pct = if u.input_tokens > 0 {
                            (u.cached_input_tokens as f32 / u.input_tokens as f32 * 100.0) as u32
                        } else {
                            0
                        };
                        tracing::info!(
                            input_tokens = u.input_tokens,
                            output_tokens = u.output_tokens,
                            cached_input_tokens = u.cached_input_tokens,
                            cache_hit_pct = pct,
                            "[agent] turn token usage"
                        );
                        self.emit(AgentEvent::Usage {
                            input_tokens: u.input_tokens,
                            output_tokens: u.output_tokens,
                            cached_input_tokens: u.cached_input_tokens,
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

    /// Execute one tool call. Appends the result to `session` and returns
    /// `Ok(None)` to continue, `Ok(Some(request))` when the call was QUEUED
    /// for approval (the caller decides when to surface it — siblings in the
    /// same batch still run first), or `Err` on an infra failure (this call's
    /// error tool_result is already appended so it cannot dangle; the caller
    /// answers the rest of the batch).
    async fn run_one_tool(
        &self,
        session: &mut Session,
        call: ToolCall,
    ) -> Result<Option<ApprovalRequest>, LychiError> {
        let result = self.execute_tool_call(&call).await;
        self.fold_outcome(session, call, result)
    }

    /// The execution half of a tool call: emit the start event, wire the live
    /// output stream, run the executor, and return the raw outcome — WITHOUT
    /// touching the session. Session-free on purpose: read-only siblings in one
    /// batch run through this concurrently ([`join_all`] in the main loop), and
    /// their outcomes are folded into the session afterwards, in call order, by
    /// [`Self::fold_outcome`].
    async fn execute_tool_call(&self, call: &ToolCall) -> Result<ToolOutcome, LychiError> {
        self.emit(AgentEvent::ToolCallStarted {
            call_id: call.id.clone(),
            name: call.name.clone(),
            args: call.args.clone(),
        });

        // Wire a live-output channel: a streaming tool (a captured shell command)
        // pushes each output line into `out_tx`; a forwarder task turns those into
        // `ToolOutputDelta` events tagged with this call's id, so the UI streams
        // the output as it happens. The task ends when the tool drops its sender
        // (the channel closes) — i.e. when the command finishes. Non-streaming
        // tools never push, so the task simply ends at once. The FINAL output
        // still comes back in the `Ran` outcome, unchanged. Concurrent calls
        // interleave safely: every delta is tagged with its call id.
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let forward = {
            let ev_tx = self.ev_tx.clone();
            let call_id = call.id.clone();
            tokio::spawn(async move {
                while let Some(chunk) = out_rx.recv().await {
                    if ev_tx
                        .send(AgentEvent::ToolOutputDelta {
                            call_id: call_id.clone(),
                            chunk,
                        })
                        .is_err()
                    {
                        break; // UI dropped the stream — stop forwarding.
                    }
                }
            })
        };

        let result = self
            .executor
            .execute(&call.name, &call.args, Some(out_tx))
            .await;
        // The tool has returned, so its sender is dropped and the forwarder will
        // drain any last buffered chunks and end. Await it so all deltas are
        // emitted BEFORE the completion event (ordering the UI relies on).
        let _ = forward.await;
        result
    }

    /// The bookkeeping half of a tool call: apply an outcome from
    /// [`Self::execute_tool_call`] to the session (tool_result / pending
    /// approval) and emit the completion event.
    fn fold_outcome(
        &self,
        session: &mut Session,
        call: ToolCall,
        result: Result<ToolOutcome, LychiError>,
    ) -> Result<Option<ApprovalRequest>, LychiError> {
        match result {
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
                Ok(None)
            }
            Ok(ToolOutcome::NeedsApproval { reason, resume }) => {
                let request = ApprovalRequest {
                    call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    args: call.args.clone(),
                    reason: reason.clone(),
                };
                // Record the pending call in the session so resume can run it.
                // No AwaitingApproval event here — the caller emits it for one
                // request at a time, after the whole batch has been handled.
                session.pending.push(PendingApproval {
                    call,
                    reason,
                    resume,
                });
                Ok(Some(request))
            }
            Err(e) => {
                // Answer THIS call before surfacing the failure — a tool_use
                // without a tool_result poisons every later provider request.
                session.push_tool_result(&call.id, e.to_string(), true);
                self.emit(AgentEvent::ToolCallFailed {
                    call_id: call.id,
                    error: e.to_string(),
                });
                Err(e)
            }
        }
    }

    /// Apply the user's decision to the OLDEST pending approval (FIFO — the
    /// order the model asked in). Returns `Some(Outcome)` if another queued
    /// approval must be surfaced next or an error occurred; `None` if every
    /// pending call is answered and the loop should continue to the model.
    async fn apply_decision(
        &self,
        session: &mut Session,
        decision: ApprovalDecision,
    ) -> Option<Outcome> {
        if session.pending.is_empty() {
            return None; // nothing pending — nothing to do
        }
        let pending = session.pending.remove(0);
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
                    }
                    Err(e) => {
                        // A tool-logic failure feeds back; an infra failure aborts.
                        session.push_tool_result(&call_id, e.to_string(), true);
                        self.emit(AgentEvent::ToolCallFailed {
                            call_id,
                            error: e.to_string(),
                        });
                    }
                }
            }
            ApprovalDecision::Reject { message } => {
                let msg = message.unwrap_or_else(|| {
                    // Deny-and-continue: the refusal is FEEDBACK, not a wall.
                    // Naming the follow-ups stops the model from retrying the
                    // same call or ending the run with an apology.
                    "The user declined this action. Do not retry it — take a \
                     different approach, or briefly say what you would have done \
                     and ask how they'd like to proceed."
                        .to_string()
                });
                session.push_tool_result(&call_id, msg.clone(), true);
                self.emit(AgentEvent::ToolCallFailed {
                    call_id,
                    error: msg,
                });
            }
        }

        // More approvals queued from the same batch: surface the next one
        // instead of re-entering the model — the transcript is incomplete
        // until every pending call has a result, and the model must not be
        // called with tool_use ids that have none.
        if let Some(next) = session.pending.first() {
            let request = ApprovalRequest {
                call_id: next.call.id.clone(),
                tool_name: next.call.name.clone(),
                args: next.call.args.clone(),
                reason: next.reason.clone(),
            };
            self.emit(AgentEvent::AwaitingApproval(request.clone()));
            return Some(Outcome::AwaitingApproval {
                request,
                session: session.clone(),
            });
        }
        None
    }

    /// Whether a specific call is state-mutating. For a standalone tool that
    /// is its `ToolDef.mutates`; for a GROUP tool (non-empty
    /// `mutating_actions`) it is per action — `personal_data` holds both
    /// `note_add` (mutating) and `note_read` (not), so judging the tool as a
    /// whole would serialize harmless reads. A group call whose args don't
    /// parse or name no known action counts as non-mutating: dispatch rejects
    /// it before anything runs, so it cannot mutate. Unknown tool names
    /// default to non-mutating — the executor will reject them anyway, so it
    /// is never worth blocking a sibling for.
    fn is_mutating(&self, call: &ToolCall) -> bool {
        let Some(def) = self.tools.iter().find(|t| t.name == call.name) else {
            return false;
        };
        if def.mutating_actions.is_empty() {
            return def.mutates;
        }
        serde_json::from_str::<serde_json::Value>(call.args.trim())
            .ok()
            .and_then(|v| {
                v.get("action")
                    .and_then(|a| a.as_str())
                    .map(|a| def.mutating_actions.iter().any(|m| m == a))
            })
            .unwrap_or(false)
    }

    /// Filter a single turn's tool batch before execution: exact-duplicate
    /// dedup, then a one-mutating-tool-per-turn hold.
    ///
    /// Returns the calls to actually execute. Every dropped call is answered
    /// with a tool_result (and its UI lifecycle events emitted, so the panel
    /// shows it resolved rather than pending) so the provider contract holds —
    /// each tool_use id gets a result. Per-batch only: a tool the model calls
    /// again in a LATER turn is unaffected.
    fn filter_batch(&self, session: &mut Session, calls: Vec<ToolCall>) -> Vec<ToolCall> {
        let mut seen: Vec<(String, String)> = Vec::new();
        let mut ran_mutating = false;
        let mut kept = Vec::with_capacity(calls.len());
        for call in calls {
            let key = (call.name.clone(), call.args.clone());
            if seen.contains(&key) {
                // A repeat of a call already accepted this turn. Don't run the
                // side effect again — answer its id and show it resolved.
                self.answer_dropped(
                    session,
                    call,
                    "Skipped: an identical tool call was already made in this turn.",
                );
                continue;
            }
            seen.push(key);

            if self.is_mutating(&call) {
                if ran_mutating {
                    // A second state-mutating tool in the same turn. The model
                    // is almost certainly hedging (variants of one operation);
                    // run only the first and tell it to sequence the rest.
                    self.answer_dropped(
                        session,
                        call,
                        "Held: a state-changing command already ran in this turn. Issue \
                         only one file/system-modifying command at a time — wait for its \
                         result, then run the next. If you meant to try alternatives, pick \
                         one.",
                    );
                    continue;
                }
                ran_mutating = true;
            }
            kept.push(call);
        }
        kept
    }

    /// Answer a `find_tool` call inline: rank the query against the FULL
    /// catalog, reply with the matches, and add them to the session's sent set
    /// so their schemas ride the next request. Accepts the schema'd JSON
    /// (`{"query": …}`) or a bare string. A no-match answer says so explicitly
    /// — silence would read as "the capability does not exist".
    fn answer_find_tool(&self, session: &mut Session, call: ToolCall) {
        self.emit(AgentEvent::ToolCallStarted {
            call_id: call.id.clone(),
            name: call.name.clone(),
            args: call.args.clone(),
        });
        let raw = call.args.trim();
        let query = serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(String::from))
            .unwrap_or_else(|| raw.to_string());

        let matches = crate::coordinator::relevance::search_catalog(&query, &self.tools);
        let output = if matches.is_empty() {
            format!(
                "No tools match \"{query}\". The capability may not exist; answer from \
                 knowledge or tell the user it is not available."
            )
        } else {
            let mut out = String::from("Matching tools (callable from your next step):\n");
            for t in &matches {
                out.push_str(&format!("- `{}` — {}\n", t.name, t.description));
            }
            out.trim_end().to_string()
        };
        for t in matches {
            if !session.sent_tools.iter().any(|n| n == &t.name) {
                session.sent_tools.push(t.name.clone());
            }
        }
        session.push_tool_result(&call.id, output.clone(), false);
        self.emit(AgentEvent::ToolCallCompleted {
            call_id: call.id,
            output,
            artifact: None,
        });
    }

    /// Answer a call the batch filter dropped: emit its start/complete UI events
    /// and push a non-error tool_result carrying `note`, so the model sees why it
    /// was not run and the provider still gets a result for the tool_use id.
    fn answer_dropped(&self, session: &mut Session, call: ToolCall, note: &str) {
        self.emit(AgentEvent::ToolCallStarted {
            call_id: call.id.clone(),
            name: call.name.clone(),
            args: call.args.clone(),
        });
        session.push_tool_result(&call.id, note.to_string(), false);
        self.emit(AgentEvent::ToolCallCompleted {
            call_id: call.id,
            output: note.to_string(),
            artifact: None,
        });
    }
}

struct TurnResult {
    text: String,
    tool_calls: Vec<ToolCall>,
    /// The model hit its `max_tokens` cap — the answer is cut off, not complete.
    truncated: bool,
}

/// Repair a tool call whose NAME the model mangled, when the intent is still
/// recognizable. gpt-oss (via Groq, with server validation disabled) emits two
/// known manglings: a namespaced `group.action` ("web_tools.fetch") and a
/// leaked harmony marker ("web_tools<|channel|>commentary"). Both carry a real
/// base tool; the dotted form also names the action, which folds into the JSON
/// args. Anything unrecognizable passes through untouched — the executor's
/// unknown-tool error is feedback the model can correct from, and inventing a
/// call the model didn't make is worse than failing one it did.
fn normalize_tool_call(mut call: ToolCall, tools: &[ToolDef]) -> ToolCall {
    let known = |name: &str| tools.iter().any(|t| t.name == name);
    if known(&call.name) {
        return call;
    }
    // Cut trailing junk (the harmony marker starts at '<').
    let cleaned: String = call
        .name
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .collect();
    let (base, action) = match cleaned.split_once('.') {
        Some((b, a)) => (b.to_string(), Some(a.to_string())),
        None => (cleaned, None),
    };
    if !known(&base) {
        return call;
    }
    tracing::warn!(from = %call.name, to = %base, "[agent] normalized mangled tool name");
    call.name = base;
    if let Some(action) = action.filter(|a| !a.is_empty()) {
        // Fold the dotted action into the JSON args the group dispatcher reads.
        let mut map = serde_json::from_str::<serde_json::Value>(call.args.trim())
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        map.entry("action".to_string())
            .or_insert(serde_json::Value::String(action));
        call.args = serde_json::Value::Object(map).to_string();
    }
    call
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::tool_executor::ToolOutputChannel;
    use async_trait::async_trait;
    use futures_util::stream;
    use std::sync::Mutex;

    // ── Mock provider ────────────────────────────────────────────────────────
    // Scripts a sequence of turns. Each turn is a Vec<StreamEvent> the provider
    // replays. `chat` pops the next scripted turn — so the loop drives real
    // multi-turn behavior with zero network / real model.
    struct MockProvider {
        turns: Mutex<std::collections::VecDeque<Vec<StreamEvent>>>,
        /// Turn indexes (0-based) that ERROR mid-stream instead of playing
        /// their events — the wire-level "provider reported an error" path.
        error_turns: Mutex<std::collections::VecDeque<usize>>,
        calls: Mutex<usize>,
        /// Records the messages passed on each `chat` call (to assert the loop
        /// fed tool results back).
        seen: Mutex<Vec<usize>>,
    }
    impl MockProvider {
        fn new(turns: Vec<Vec<StreamEvent>>) -> Arc<Self> {
            Arc::new(Self {
                turns: Mutex::new(turns.into()),
                error_turns: Mutex::new(Default::default()),
                calls: Mutex::new(0),
                seen: Mutex::new(Vec::new()),
            })
        }
        /// `new`, plus: the given 0-based call indexes fail mid-stream with the
        /// wire's in-band provider-error shape (before yielding any event).
        fn with_stream_errors(turns: Vec<Vec<StreamEvent>>, errors: &[usize]) -> Arc<Self> {
            let p = Self::new(turns);
            *p.error_turns.lock().unwrap() = errors.iter().copied().collect();
            p
        }
    }
    #[async_trait]
    impl AiProvider for MockProvider {
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
            let call_idx = {
                let mut c = self.calls.lock().unwrap();
                let i = *c;
                *c += 1;
                i
            };
            if self.error_turns.lock().unwrap().contains(&call_idx) {
                return stream::iter(vec![Err(LychiError::Ai(
                    "The AI provider reported an error: tool call validation failed".into(),
                ))])
                .boxed();
            }
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
    /// One turn requesting SEVERAL tools — the parallel-call shape Claude
    /// routinely emits, and the one every approval test missed.
    fn call_tools(calls: &[(&str, &str, &str)]) -> Vec<StreamEvent> {
        let mut events: Vec<StreamEvent> = calls
            .iter()
            .map(|(id, name, args)| StreamEvent::ToolCallComplete {
                id: (*id).into(),
                name: (*name).into(),
                args: (*args).into(),
            })
            .collect();
        events.push(StreamEvent::Done {
            stop_reason: crate::providers::StopReason::ToolUse,
            usage: None,
        });
        events
    }
    /// The call ids that have a tool_result in the session, in message order.
    fn result_ids(session: &Session) -> Vec<String> {
        session
            .messages
            .iter()
            .filter_map(|m| m.tool_call_id.clone())
            .collect()
    }

    // ── Mock executor ────────────────────────────────────────────────────────
    // Maps tool name → a scripted ToolOutcome. `run_approved` echoes a fixed
    // string so approve-resume is observable.
    struct MockExecutor {
        outcomes: std::collections::HashMap<String, ToolOutcome>,
        approved_output: String,
        approved_calls: Mutex<usize>,
        execute_calls: Mutex<usize>,
    }
    impl MockExecutor {
        fn new() -> Self {
            Self {
                outcomes: Default::default(),
                approved_output: "APPROVED-RAN".into(),
                approved_calls: Mutex::new(0),
                execute_calls: Mutex::new(0),
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
        async fn execute(
            &self,
            name: &str,
            _args: &str,
            output_ch: Option<ToolOutputChannel>,
        ) -> Result<ToolOutcome, LychiError> {
            *self.execute_calls.lock().unwrap() += 1;
            match self.outcomes.get(name) {
                Some(ToolOutcome::Ran {
                    output, is_error, ..
                }) => {
                    // Stream the output as two chunks when a channel is provided,
                    // so a test can assert live deltas arrive before completion.
                    if let Some(ch) = output_ch
                        && !output.is_empty()
                    {
                        let _ = ch.send(output.clone());
                    }
                    Ok(ToolOutcome::Ran {
                        output: output.clone(),
                        is_error: *is_error,
                        artifact: None,
                    })
                }
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
                mutates: false,
                mutating_actions: Vec::new(),
                input_schema: None,
            }],
        )
    }

    /// A coordinator whose catalog has a single named tool with a chosen
    /// `mutates` flag — for tests that exercise one specific tool.
    fn coordinator_with_tool(
        provider: Arc<MockProvider>,
        executor: Arc<MockExecutor>,
        name: &str,
        mutates: bool,
    ) -> Coordinator<MockExecutor> {
        Coordinator::new(
            provider,
            executor,
            vec![ToolDef {
                name: name.into(),
                description: name.into(),
                mutates,
                mutating_actions: Vec::new(),
                input_schema: None,
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
    async fn find_tool_answers_inline_and_widens_the_sent_set() {
        // Turn 1: the model searches for a capability its shortlist lacked;
        // turn 2: it answers. The executor must never see the pseudo-tool.
        let provider = MockProvider::new(vec![
            call_tool("c1", "find_tool", r#"{"query":"screenshot of my window"}"#),
            answer("done"),
        ]);
        let exec = Arc::new(MockExecutor::new());
        let tools = vec![
            ToolDef {
                name: "weather".into(),
                description: "get weather".into(),
                mutates: false,
                mutating_actions: Vec::new(),
                input_schema: None,
            },
            ToolDef {
                name: "screenshot".into(),
                description: "capture the screen or a window".into(),
                mutates: false,
                mutating_actions: Vec::new(),
                input_schema: None,
            },
        ];
        let coord = Coordinator::new(provider, exec.clone(), tools);
        let (stream, handle) = coord.run(
            Session::new("sys", "grab my screen"),
            CancellationToken::new(),
        );
        let events = drain(stream).await;
        let out = tool_completed_output(&events).expect("find_tool answered");
        assert!(
            out.contains("`screenshot`"),
            "answers with the match: {out}"
        );
        assert_eq!(
            *exec.execute_calls.lock().unwrap(),
            0,
            "the executor never sees find_tool"
        );
        match handle.wait().await {
            Outcome::Done { session } => {
                assert!(
                    session.sent_tools.iter().any(|n| n == "screenshot"),
                    "the found tool joins the sent set: {:?}",
                    session.sent_tools
                );
                assert_eq!(result_ids(&session), vec!["c1".to_string()]);
            }
            _ => panic!("expected Done"),
        }
    }

    #[tokio::test]
    async fn a_tool_bearing_coordinator_always_offers_find_tool() {
        let provider = MockProvider::new(vec![answer("hi")]);
        let exec = Arc::new(MockExecutor::new());
        let coord = coordinator(provider, exec);
        assert!(coord.tools.iter().any(|t| t.name == "find_tool"));

        // A tool-less chat gets no discovery tool either.
        let provider = MockProvider::new(vec![answer("hi")]);
        let exec = Arc::new(MockExecutor::new());
        let coord = Coordinator::new(provider, exec, Vec::new());
        assert!(coord.tools.is_empty());
    }

    #[test]
    fn mangled_tool_names_normalize_to_their_intent() {
        let tools = vec![ToolDef {
            name: "web_tools".into(),
            description: "web".into(),
            mutates: false,
            mutating_actions: Vec::new(),
            input_schema: None,
        }];
        let call = |name: &str, args: &str| ToolCall {
            id: "c".into(),
            name: name.into(),
            args: args.into(),
        };

        // Dotted group.action → base tool, action folded into the args.
        let n = normalize_tool_call(call("web_tools.fetch", r#"{"url":"https://x"}"#), &tools);
        assert_eq!(n.name, "web_tools");
        let v: serde_json::Value = serde_json::from_str(&n.args).unwrap();
        assert_eq!(v["action"], "fetch");
        assert_eq!(v["url"], "https://x");

        // Harmony marker junk → base tool, args untouched.
        let n = normalize_tool_call(
            call(
                "web_tools<|channel|>commentary",
                r#"{"action":"search","query":"q"}"#,
            ),
            &tools,
        );
        assert_eq!(n.name, "web_tools");
        assert_eq!(n.args, r#"{"action":"search","query":"q"}"#);

        // An existing action never gets clobbered by the dotted one.
        let n = normalize_tool_call(
            call("web_tools.fetch", r#"{"action":"search","query":"q"}"#),
            &tools,
        );
        let v: serde_json::Value = serde_json::from_str(&n.args).unwrap();
        assert_eq!(v["action"], "search");

        // Unrecognizable names pass through for the executor's error feedback.
        let n = normalize_tool_call(call("mystery.thing", "{}"), &tools);
        assert_eq!(n.name, "mystery.thing");

        // A correct name is untouched (no re-serialization churn).
        let n = normalize_tool_call(call("web_tools", "flat args"), &tools);
        assert_eq!(n.args, "flat args");
    }

    #[tokio::test]
    async fn a_mid_stream_provider_error_is_retried_once() {
        // Call 0 dies mid-stream (the gpt-oss channel-marker flake); the loop
        // must re-roll and the second call answers.
        let provider = MockProvider::with_stream_errors(vec![answer("recovered")], &[0]);
        let exec = Arc::new(MockExecutor::new());
        let (stream, handle) =
            coordinator(provider, exec).run(Session::new("sys", "hi"), CancellationToken::new());
        let events = drain(stream).await;
        assert_eq!(final_text(&events), Some("recovered"));
        assert!(matches!(handle.wait().await, Outcome::Done { .. }));

        // Two in a row → surfaced as an error, no infinite roll.
        let provider = MockProvider::with_stream_errors(vec![answer("never")], &[0, 1]);
        let exec = Arc::new(MockExecutor::new());
        let (stream, handle) =
            coordinator(provider, exec).run(Session::new("sys", "hi"), CancellationToken::new());
        let events = drain(stream).await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Error(m) if m.contains("provider reported"))),
        );
        assert!(matches!(handle.wait().await, Outcome::Error { .. }));
    }

    #[tokio::test]
    async fn an_empty_turn_is_retried_once_then_errors() {
        // First turn: the provider streams NOTHING (the Groq 0-token flake).
        // The loop must retry; a good second turn answers normally.
        let provider = MockProvider::new(vec![answer(""), answer("recovered")]);
        let exec = Arc::new(MockExecutor::new());
        let (stream, handle) =
            coordinator(provider, exec).run(Session::new("sys", "hi"), CancellationToken::new());
        let events = drain(stream).await;
        assert_eq!(final_text(&events), Some("recovered"));
        match handle.wait().await {
            Outcome::Done { session } => {
                // The empty attempt must not linger in history.
                assert_eq!(session.messages.len(), 3, "system, user, one assistant");
            }
            _ => panic!("expected Done"),
        }

        // Two empty turns in a row → an honest error, not silence.
        let provider = MockProvider::new(vec![answer(""), answer("")]);
        let exec = Arc::new(MockExecutor::new());
        let (stream, handle) =
            coordinator(provider, exec).run(Session::new("sys", "hi"), CancellationToken::new());
        let events = drain(stream).await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Error(msg) if msg.contains("empty"))),
            "an error event must surface"
        );
        assert!(matches!(handle.wait().await, Outcome::Error { .. }));
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
    async fn a_tools_output_streams_as_deltas_before_completion() {
        // A streaming tool (the mock pushes its output into the live channel)
        // must produce ToolOutputDelta events tagged with the call id, and they
        // must arrive BEFORE the ToolCallCompleted — the ordering the UI relies
        // on to fill the block live and then finalise it.
        let provider = MockProvider::new(vec![call_tool("t1", "run", "echo hi"), answer("Done.")]);
        let exec = Arc::new(MockExecutor::new().ran("run", "hi\n", false));
        let (stream, handle) = coordinator_with_tool(provider, exec, "run", false)
            .run(Session::new("sys", "run it"), CancellationToken::new());
        let events = drain(stream).await;

        // A delta for this call arrived, carrying the streamed output.
        let delta_idx = events.iter().position(|e| {
            matches!(e, AgentEvent::ToolOutputDelta { call_id, chunk }
                if call_id == "t1" && chunk == "hi\n")
        });
        assert!(delta_idx.is_some(), "a ToolOutputDelta must be emitted");

        // The completion for the same call came AFTER the delta.
        let done_idx = events.iter().position(
            |e| matches!(e, AgentEvent::ToolCallCompleted { call_id, .. } if call_id == "t1"),
        );
        assert!(done_idx.is_some(), "the tool still completes");
        assert!(
            delta_idx.unwrap() < done_idx.unwrap(),
            "deltas must stream before the completion event"
        );

        assert!(matches!(handle.wait().await, Outcome::Done { .. }));
    }

    #[tokio::test]
    async fn duplicate_tool_calls_in_one_turn_run_once() {
        // The model emits the SAME call (name+args) twice in one turn — the
        // groq-llama double-screenshot case. The side effect must run once; both
        // ids must still get a tool_result (contract), and the model's next turn
        // must see two results (one real, one "skipped") so no tool_use dangles.
        let provider = MockProvider::new(vec![
            call_tools(&[("t1", "weather", "London"), ("t2", "weather", "London")]),
            answer("Done."),
        ]);
        let exec = Arc::new(MockExecutor::new().ran("weather", "18C sunny", false));
        let e = exec.clone();
        let (stream, handle) = coordinator(provider, exec)
            .run(Session::new("sys", "weather?"), CancellationToken::new());
        let events = drain(stream).await;

        // The executor ran EXACTLY once despite two identical calls.
        assert_eq!(
            *e.execute_calls.lock().unwrap(),
            1,
            "an identical duplicate call must not re-execute the tool"
        );
        // Both ids answered → no dangling tool_use.
        assert!(has_text(&events, "Done."));
        match handle.wait().await {
            Outcome::Done { session } => {
                let ids = result_ids(&session);
                assert!(ids.contains(&"t1".to_string()) && ids.contains(&"t2".to_string()));
                // One real result, one "skipped" — but both present.
                let skipped = session
                    .messages
                    .iter()
                    .any(|m| m.content_text().contains("Skipped"));
                assert!(skipped, "the duplicate got a skipped tool_result");
            }
            _ => panic!("expected Done"),
        }
    }

    #[tokio::test]
    async fn only_one_mutating_tool_runs_per_turn() {
        // The hedge case: the model emits THREE variants of one destructive
        // operation in a single turn (resize the same photos three ways — the
        // args differ, so exact dedup can't catch them). Only the first
        // mutating call must run; the other two are held with a note, and all
        // three ids still get a tool_result so no tool_use dangles.
        let provider = MockProvider::new(vec![
            call_tools(&[
                ("t1", "run", "magick a -resize 50%"),
                ("t2", "run", "convert a -resize 50%"),
                ("t3", "run", "magick a resize 50%"),
            ]),
            answer("Resized."),
        ]);
        let exec = Arc::new(MockExecutor::new().ran("run", "ok", false));
        let e = exec.clone();
        let coord = Coordinator::new(
            provider,
            exec,
            vec![ToolDef {
                name: "run".into(),
                description: "shell".into(),
                mutates: true,
                mutating_actions: Vec::new(),
                input_schema: None,
            }],
        );
        let (stream, handle) = coord.run(
            Session::new("sys", "resize my photos"),
            CancellationToken::new(),
        );
        let events = drain(stream).await;

        // Exactly one of the three mutating calls actually executed.
        assert_eq!(
            *e.execute_calls.lock().unwrap(),
            1,
            "a second/third mutating tool in one turn must be held, not run"
        );
        assert!(has_text(&events, "Resized."));
        match handle.wait().await {
            Outcome::Done { session } => {
                let ids = result_ids(&session);
                // All three ids answered — provider contract holds.
                for id in ["t1", "t2", "t3"] {
                    assert!(ids.contains(&id.to_string()), "id {id} must have a result");
                }
                let held = session
                    .messages
                    .iter()
                    .filter(|m| m.content_text().contains("Held:"))
                    .count();
                assert_eq!(held, 2, "the two extra mutating calls were held");
            }
            _ => panic!("expected Done"),
        }
    }

    #[tokio::test]
    async fn read_only_tools_stay_parallel_in_one_turn() {
        // The hold is scoped to MUTATING tools. Two different read-only calls in
        // one turn (two weather lookups) must both run — Lychi stays smart.
        let provider = MockProvider::new(vec![
            call_tools(&[("t1", "weather", "London"), ("t2", "weather", "Paris")]),
            answer("Done."),
        ]);
        let exec = Arc::new(MockExecutor::new().ran("weather", "sunny", false));
        let e = exec.clone();
        // Default `coordinator` helper registers weather with mutates:false.
        let (stream, handle) = coordinator(provider, exec)
            .run(Session::new("sys", "weather?"), CancellationToken::new());
        drain(stream).await;
        assert_eq!(
            *e.execute_calls.lock().unwrap(),
            2,
            "two distinct read-only calls in one turn must both run"
        );
        assert!(matches!(handle.wait().await, Outcome::Done { .. }));
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
                mutates: false,
                mutating_actions: Vec::new(),
                input_schema: None,
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
                mutates: false,
                mutating_actions: Vec::new(),
                input_schema: None,
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
                mutates: false,
                mutating_actions: Vec::new(),
                input_schema: None,
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
                mutates: false,
                mutating_actions: Vec::new(),
                input_schema: None,
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
                mutates: false,
                mutating_actions: Vec::new(),
                input_schema: None,
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

    /// THE WEDGE (ROUTE-3): a parallel-tool turn where one call needs approval
    /// must not drop its siblings. The old loop returned on the first
    /// NeedsApproval; the sibling was never run and never answered, and the
    /// next provider request carried a tool_use id with no tool_result — a
    /// permanent 400 on both Anthropic and OpenAI. Every prior approval test
    /// used single-call turns, the one shape that can't show this.
    #[tokio::test]
    async fn an_approval_does_not_drop_its_sibling_calls() {
        let provider = MockProvider::new(vec![call_tools(&[
            ("t1", "delete", "all"),
            ("t2", "weather", "London"),
        ])]);
        let exec = Arc::new(
            MockExecutor::new()
                .needs_approval("delete", "confirm?")
                .ran("weather", "18C sunny", false),
        );
        let coord = Coordinator::new(
            provider,
            exec.clone(),
            vec![
                ToolDef {
                    name: "delete".into(),
                    description: "delete".into(),
                    mutates: false,
                    mutating_actions: Vec::new(),
                    input_schema: None,
                },
                ToolDef {
                    name: "weather".into(),
                    description: "get weather".into(),
                    mutates: false,
                    mutating_actions: Vec::new(),
                    input_schema: None,
                },
            ],
        );
        let (s1, h1) = coord.run(Session::new("sys", "go"), CancellationToken::new());
        drain(s1).await;
        let session = match h1.wait().await {
            Outcome::AwaitingApproval { request, session } => {
                assert_eq!(request.call_id, "t1", "the approval surfaces first");
                assert_eq!(
                    result_ids(&session),
                    vec!["t2"],
                    "the safe sibling ran BEFORE suspension"
                );
                assert_eq!(session.pending.len(), 1);
                session
            }
            _ => panic!("expected AwaitingApproval"),
        };

        // Approve → the queued call runs, and the model gets a COMPLETE
        // transcript: a result for every tool_use id of the turn.
        let provider2 = MockProvider::new(vec![answer("done")]);
        let coord2 = Coordinator::new(
            provider2,
            exec.clone(),
            vec![ToolDef {
                name: "delete".into(),
                description: "delete".into(),
                mutates: false,
                mutating_actions: Vec::new(),
                input_schema: None,
            }],
        );
        let (s2, h2) = coord2.resume(session, ApprovalDecision::Approve, CancellationToken::new());
        drain(s2).await;
        match h2.wait().await {
            Outcome::Done { session } => {
                let mut ids = result_ids(&session);
                ids.sort();
                assert_eq!(ids, vec!["t1", "t2"], "no tool_use id left dangling");
                assert!(session.pending.is_empty());
            }
            _ => panic!("expected Done"),
        }
    }

    /// Two approval-needing calls in one turn surface SEQUENTIALLY — deciding
    /// the first re-suspends on the second instead of calling the model with
    /// an unanswered tool_use.
    #[tokio::test]
    async fn queued_approvals_surface_one_at_a_time_in_order() {
        let provider = MockProvider::new(vec![call_tools(&[
            ("t1", "delete", "all"),
            ("t2", "format", "disk"),
        ])]);
        let exec = Arc::new(
            MockExecutor::new()
                .needs_approval("delete", "delete?")
                .needs_approval("format", "format?"),
        );
        let tools = vec![
            ToolDef {
                name: "delete".into(),
                description: "delete".into(),
                mutates: false,
                mutating_actions: Vec::new(),
                input_schema: None,
            },
            ToolDef {
                name: "format".into(),
                description: "format".into(),
                mutates: false,
                mutating_actions: Vec::new(),
                input_schema: None,
            },
        ];
        let coord = Coordinator::new(provider, exec.clone(), tools.clone());
        let (s1, h1) = coord.run(Session::new("sys", "go"), CancellationToken::new());
        drain(s1).await;
        let session = match h1.wait().await {
            Outcome::AwaitingApproval { request, session } => {
                assert_eq!(request.call_id, "t1");
                assert_eq!(session.pending.len(), 2, "both calls queued");
                session
            }
            _ => panic!("expected AwaitingApproval"),
        };

        // Deciding the first surfaces the SECOND — not a model turn.
        let coord2 = Coordinator::new(MockProvider::new(vec![]), exec.clone(), tools.clone());
        let (s2, h2) = coord2.resume(session, ApprovalDecision::Approve, CancellationToken::new());
        drain(s2).await;
        let session = match h2.wait().await {
            Outcome::AwaitingApproval { request, session } => {
                assert_eq!(request.call_id, "t2", "FIFO: the model's order");
                assert_eq!(session.pending.len(), 1);
                assert_eq!(result_ids(&session), vec!["t1"]);
                session
            }
            _ => panic!("expected the second approval"),
        };

        // Rejecting the second completes the transcript and reaches the model.
        let coord3 = Coordinator::new(
            MockProvider::new(vec![answer("understood")]),
            exec.clone(),
            tools,
        );
        let (s3, h3) = coord3.resume(
            session,
            ApprovalDecision::Reject { message: None },
            CancellationToken::new(),
        );
        drain(s3).await;
        match h3.wait().await {
            Outcome::Done { session } => {
                let mut ids = result_ids(&session);
                ids.sort();
                assert_eq!(ids, vec!["t1", "t2"]);
                assert_eq!(*exec.approved_calls.lock().unwrap(), 1, "only t1 ran");
            }
            _ => panic!("expected Done"),
        }
    }

    /// An infra error mid-batch answers EVERY call of the turn — the failing
    /// one, unrun siblings, and any approval already queued — so the persisted
    /// session stays continuable instead of poisoning every later request.
    #[tokio::test]
    async fn a_mid_batch_error_leaves_no_dangling_tool_calls() {
        let provider = MockProvider::new(vec![call_tools(&[
            ("t1", "delete", "all"),     // queues for approval
            ("t2", "unknown_tool", "x"), // infra error (not in the mock's map)
            ("t3", "weather", "London"), // never reached
        ])]);
        let exec = Arc::new(
            MockExecutor::new()
                .needs_approval("delete", "confirm?")
                .ran("weather", "18C", false),
        );
        let coord = Coordinator::new(
            provider,
            exec,
            vec![ToolDef {
                name: "delete".into(),
                description: "delete".into(),
                mutates: false,
                mutating_actions: Vec::new(),
                input_schema: None,
            }],
        );
        let (stream, handle) = coord.run(Session::new("sys", "go"), CancellationToken::new());
        drain(stream).await;
        match handle.wait().await {
            Outcome::Error { session, .. } => {
                let session = session.expect("error keeps the session");
                let mut ids = result_ids(&session);
                ids.sort();
                assert_eq!(
                    ids,
                    vec!["t1", "t2", "t3"],
                    "every tool_use id answered despite the failure"
                );
                assert!(
                    session.pending.is_empty(),
                    "queued approvals resolved — nothing left to wedge a resume"
                );
            }
            _ => panic!("expected Error"),
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
