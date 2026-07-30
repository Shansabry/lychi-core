//! The agent-chat command: drives the coordinator brick against the real
//! `Executor`, streaming `AgentEvent`s to the frontend as `lychi://agent-event`.
//!
//! This is the Tauri adapter layer. The coordinator (in lychi-core) is
//! UI/Tauri-agnostic; here we (1) adapt the real `Executor` to the coordinator's
//! `ToolExecutor` trait, (2) build the tool catalog from the registry, (3) drive
//! the loop and forward its event stream, and (4) hold the suspended `Session`
//! so an approval can resume it.
//!
//! Phase 2 of the AI rewrite — the tool-calling agent. Reuses the whole
//! permission stack: `executor.run(confirmed=false)` returns `needs_confirmation`
//! for destructive tools (the Rules Engine gate), which the adapter maps to the
//! coordinator's `NeedsApproval`; approval runs `run_confirmed` on the exact
//! assessed action.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use futures_util::StreamExt;
use lychi_core::coordinator::{
    AgentEvent, ApprovalDecision, Coordinator, Outcome, ResumeToken, Session, ToolArtifact,
    ToolExecutor, ToolOutcome,
};
use lychi_core::error::LychiError;
use lychi_core::executor::{Executor, RunInputs};
use lychi_core::providers::{CancellationToken, ImageSource, ToolDef};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::RwLock;

use crate::state::AppState;

// ── ToolExecutor adapter over the real Executor ──────────────────────────────

/// Adapts Lychi's `Executor` (Rules Engine, side effects, per-run context) to the
/// coordinator's `ToolExecutor` seam. The coordinator stays pure; all the
/// execution-context wiring lives here.
struct ExecutorAdapter {
    executor: Arc<RwLock<Executor>>,
    privacy: lychi_core::config::PrivacyConfig,
}

/// Render an `ActionResult` into the plain text the model sees as a tool result.
fn result_text(res: &lychi_core::action_registry::ActionResult) -> String {
    use lychi_core::action_registry::Output;
    if let Some(err) = &res.error {
        return err.clone();
    }
    match &res.output {
        Output::Text { body, .. } => body.clone(),
        Output::Navigate { url, .. } => format!("Opened: {url}"),
        Output::LaunchDesktop { path } => format!("Launched: {path}"),
        Output::FocusApp { wm_class } => format!("Focused: {wm_class}"),
        Output::None => {
            if res.success {
                "Done.".to_string()
            } else {
                "Failed.".to_string()
            }
        }
    }
}

/// Split a tool result into (model-facing summary text, optional rich artifact).
/// Rich outputs (a QR SVG, a weather card) are NOT dumped into the model's
/// context — the model gets a short summary; the raw payload rides an artifact
/// the UI renders inline. Plain text/status results have no artifact.
fn result_summary_and_artifact(
    res: &lychi_core::action_registry::ActionResult,
) -> (String, Option<ToolArtifact>) {
    use lychi_core::action_registry::{Output, OutputType};
    if res.error.is_some() {
        return (result_text(res), None);
    }
    if let Output::Text { body, kind } = &res.output {
        match kind {
            OutputType::Svg => {
                return (
                    "Generated the requested graphic (shown to the user).".to_string(),
                    Some(ToolArtifact {
                        kind: "svg".into(),
                        content: body.clone(),
                    }),
                );
            }
            OutputType::Weather => {
                // The model gets the JSON (small, useful for it to summarize);
                // the UI also renders it as a rich card.
                return (
                    body.clone(),
                    Some(ToolArtifact {
                        kind: "weather".into(),
                        content: body.clone(),
                    }),
                );
            }
            _ => {}
        }
    }
    (result_text(res), None)
}

#[async_trait]
impl ToolExecutor for ExecutorAdapter {
    async fn execute(&self, name: &str, args: &str) -> Result<ToolOutcome, LychiError> {
        let input = if args.is_empty() {
            name.to_string()
        } else {
            format!("{name} {args}")
        };
        let exec = self.executor.read().await;
        let res = exec
            .run(&input, false, &self.privacy, &RunInputs::default())
            .await?;

        // Destructive → the Rules Engine flagged it; pause for approval. Carry the
        // exact assessed intent in the resume token so approval runs THAT action.
        if let Some(reason) = res.envelope.needs_confirmation {
            let intent = res.pending_intent.ok_or_else(|| {
                LychiError::Ai("needs_confirmation without a pending intent".into())
            })?;
            let token = ResumeToken(serde_json::json!({
                "action_id": intent.action_id,
                "args": intent.args,
            }));
            return Ok(ToolOutcome::NeedsApproval {
                reason,
                resume: token,
            });
        }

        let (output, artifact) = result_summary_and_artifact(&res.result);
        Ok(ToolOutcome::Ran {
            output,
            is_error: !res.result.success,
            artifact,
        })
    }

    async fn run_approved(&self, resume: ResumeToken) -> Result<String, LychiError> {
        // Reconstruct the assessed intent from the token and run it confirmed —
        // the exact action the Rules Engine gated, not a re-resolution.
        let action_id = resume.0["action_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let args = resume.0["args"].as_str().unwrap_or_default().to_string();
        let intent = lychi_core::intent::ResolvedIntent {
            action_id,
            args,
            routing: lychi_core::intent::RoutingMethod::Ai,
        };
        let exec = self.executor.read().await;
        let res = exec
            .run_confirmed(intent, &self.privacy, &RunInputs::default())
            .await?;
        Ok(result_text(&res.result))
    }
}

// ── The command ──────────────────────────────────────────────────────────────

/// A coordinator `AgentEvent` on the wire. Flattened so the frontend gets one
/// event type with a `kind` discriminant + the fields for that kind.
#[derive(Clone, Serialize, specta::Type)]
pub struct AgentEventDto {
    #[serde(rename = "gen")]
    pub generation: u64,
    /// One of: turn_started | text | reasoning | tool_started | tool_completed |
    /// tool_failed | awaiting_approval | final | stopped | error.
    pub kind: String,
    /// Text payload (text/reasoning delta, final text, stop/error reason).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_args: Option<String>,
    /// Approval prompt reason (kind = awaiting_approval).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub step: Option<u32>,
    /// The final answer was cut off at the token cap (kind = final).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Token usage (kind = usage).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u32>,
    /// A rich tool artifact for inline render (kind = tool_completed):
    /// artifact_kind = "svg" | "weather" | …, artifact_content = the raw payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_content: Option<String>,
}

impl AgentEventDto {
    fn from(ev: AgentEvent, generation: u64) -> Self {
        let mut d = AgentEventDto {
            generation,
            kind: String::new(),
            text: None,
            call_id: None,
            tool_name: None,
            tool_args: None,
            reason: None,
            step: None,
            truncated: false,
            input_tokens: None,
            output_tokens: None,
            artifact_kind: None,
            artifact_content: None,
        };
        match ev {
            AgentEvent::TurnStarted { step } => {
                d.kind = "turn_started".into();
                d.step = Some(step as u32);
            }
            AgentEvent::TextDelta(t) => {
                d.kind = "text".into();
                d.text = Some(t);
            }
            AgentEvent::ReasoningDelta(t) => {
                d.kind = "reasoning".into();
                d.text = Some(t);
            }
            AgentEvent::ToolCallStarted {
                call_id,
                name,
                args,
            } => {
                d.kind = "tool_started".into();
                d.call_id = Some(call_id);
                d.tool_name = Some(name);
                d.tool_args = Some(args);
            }
            AgentEvent::ToolCallCompleted {
                call_id,
                output,
                artifact,
            } => {
                d.kind = "tool_completed".into();
                d.call_id = Some(call_id);
                d.text = Some(output);
                if let Some(a) = artifact {
                    d.artifact_kind = Some(a.kind);
                    d.artifact_content = Some(a.content);
                }
            }
            AgentEvent::ToolCallFailed { call_id, error } => {
                d.kind = "tool_failed".into();
                d.call_id = Some(call_id);
                d.text = Some(error);
            }
            AgentEvent::AwaitingApproval(req) => {
                d.kind = "awaiting_approval".into();
                d.call_id = Some(req.call_id);
                d.tool_name = Some(req.tool_name);
                d.tool_args = Some(req.args);
                d.reason = Some(req.reason);
            }
            AgentEvent::Final { text, truncated } => {
                d.kind = "final".into();
                d.text = Some(text);
                d.truncated = truncated;
            }
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
            } => {
                d.kind = "usage".into();
                d.input_tokens = Some(input_tokens);
                d.output_tokens = Some(output_tokens);
            }
            AgentEvent::Stopped { reason } => {
                d.kind = "stopped".into();
                d.text = Some(reason);
            }
            AgentEvent::Error(e) => {
                d.kind = "error".into();
                d.text = Some(e);
            }
        }
        d
    }
}

/// Build a `Coordinator` over the real executor + the live tool catalog.
///
/// `with_tools = false` → the "quick-AI" fork card: no tool catalog, so the model
/// just answers (a short prose reply, no acting). The full agent chat passes
/// `true` and gets every handler as a callable tool.
async fn build_coordinator(
    state: &AppState,
    with_tools: bool,
) -> Result<(Coordinator<ExecutorAdapter>, ()), LychiError> {
    let provider = state
        .ai_provider
        .read()
        .await
        .clone()
        .ok_or_else(|| LychiError::Ai("AI is not configured".to_string()))?;
    let privacy = state.config.read().await.privacy.clone();

    // Tool catalog from the live registry — every handler is a callable tool.
    // Empty for quick-AI (answer only, no acting).
    let tools: Vec<ToolDef> = if with_tools {
        let exec = state.executor.read().await;
        exec.registry
            .command_catalog()
            .into_iter()
            .map(|c| ToolDef {
                name: c.id,
                description: c.description,
            })
            .collect()
    } else {
        Vec::new()
    };

    let adapter = Arc::new(ExecutorAdapter {
        executor: state.executor.clone(),
        privacy,
    });
    Ok((Coordinator::new(provider, adapter, tools), ()))
}

/// Drive a coordinator run/resume: forward every `AgentEvent` as
/// `lychi://agent-event`, and on completion stash the session (for a pending
/// approval) or clear it.
#[allow(clippy::too_many_arguments)]
fn drive(
    app: AppHandle,
    state_generation: Arc<std::sync::atomic::AtomicU64>,
    generation: u64,
    pending_session: Arc<RwLock<Option<Session>>>,
    db: Arc<redb::Database>,
    conversation_id: Arc<RwLock<Option<String>>>,
    mut stream: lychi_core::coordinator::AgentEventStream,
    handle: lychi_core::coordinator::OutcomeHandle,
) {
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = stream.next().await {
            if state_generation.load(Ordering::Relaxed) != generation {
                break; // superseded
            }
            let _ = app.emit("lychi://agent-event", AgentEventDto::from(ev, generation));
        }
        // Stash the session so the NEXT turn can continue it: on `Done` it holds
        // the full history (for a follow-up), on `AwaitingApproval` the paused
        // state (for `agent_approve`). Only an error/stop clears it.
        let outcome = handle.wait().await;
        if state_generation.load(Ordering::Relaxed) == generation {
            match outcome {
                Outcome::AwaitingApproval { session, .. } | Outcome::Done { session } => {
                    // A completed (or waiting) turn — persist it to history so it
                    // can be recalled later, then stash for the next turn.
                    persist_conversation(&db, &conversation_id, &session).await;
                    *pending_session.write().await = Some(session);
                }
                Outcome::Stopped { session, .. } => {
                    persist_conversation(&db, &conversation_id, &session).await;
                    *pending_session.write().await = Some(session);
                }
                Outcome::Error(_) => {
                    *pending_session.write().await = None;
                }
            }
        }
    });
}

/// Upsert the conversation into history (best-effort; a persist failure never
/// breaks the chat). Runs off the async runtime since redb is blocking.
async fn persist_conversation(
    db: &Arc<redb::Database>,
    conversation_id: &Arc<RwLock<Option<String>>>,
    session: &Session,
) {
    let Some(id) = conversation_id.read().await.clone() else {
        return;
    };
    let db = db.clone();
    let messages = session.messages.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        let store = lychi_core::ai_history::store::AiHistoryStore::new();
        if let Err(e) = store.upsert(&db, &id, &messages) {
            tracing::warn!("[history] failed to persist conversation: {e}");
        }
    })
    .await;
}

/// Start (or continue) an agent chat. `fresh` = start a new conversation; else
/// append `user` as a follow-up to the stashed session (the running conversation
/// — with prior tool results + history as context). `with_tools = false` is the
/// quick-AI fork card (answer only, no acting). Streams `lychi://agent-event`s;
/// may end in an approval the frontend resolves via `agent_approve`.
/// Encode a list of image file paths into vision `ImageSource`s off the async
/// runtime (decode/resize/base64 is CPU-bound). A path that fails to encode is
/// skipped with a warning rather than failing the whole turn.
async fn encode_images(paths: Vec<String>) -> Vec<ImageSource> {
    if paths.is_empty() {
        return Vec::new();
    }
    tauri::async_runtime::spawn_blocking(move || {
        paths
            .into_iter()
            .filter_map(|p| {
                match lychi_core::files::image_ops::encode_image_for_vision(std::path::Path::new(
                    &p,
                )) {
                    Ok((media_type, data)) => Some(ImageSource { media_type, data }),
                    Err(e) => {
                        tracing::warn!("[agent] skipping image {p}: {e}");
                        None
                    }
                }
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
#[specta::specta]
// `display` carries how the turn renders when that differs from its content (a
// preset that folded a large payload into a chip). It is persisted with the
// message so a RECALLED conversation renders identically, keeping the sender the
// only decider of that split. Never sent to a provider.
pub async fn agent_chat_start(
    system: String,
    user: String,
    fresh: bool,
    with_tools: bool,
    generation: u64,
    images: Vec<String>,
    display: Option<lychi_core::providers::MessageDisplay>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    // Entry marker for the AI path. Lengths and counts only — never prompt text,
    // which would put user content in the log file. Earns its place because
    // "the AI never responded" is otherwise indistinguishable from "the request
    // never arrived": a stale completion index once made Enter run a leftover
    // file row instead of starting a turn, and this line is what separated the
    // two cases.
    tracing::debug!(
        generation,
        with_tools,
        fresh,
        images = images.len(),
        user_len = user.len(),
        "[agent] chat turn requested"
    );
    state.ai_generation.store(generation, Ordering::Relaxed);
    let images = encode_images(images).await;
    // Inline any `@`-referenced documents (pdf/docx/…) as extracted text so the
    // model sees content, not a path. Off-runtime: extraction reads + parses files.
    // On any failure, fall back to the user's original text (never drop it).
    // Both `@` expansions run in the same blocking hop: documents become inlined
    // text, and `@clipboard` / `@selection` pull in ambient context. Context is
    // resolved first so a doc ref inside pasted text still expands afterwards.
    let user = {
        let raw = user.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let is_wayland = lychi_core::context::is_wayland();
            let with_context = lychi_core::files::text_extract::expand_context_refs(&raw, &|src| {
                lychi_core::clipboard::read_context_source(src, is_wayland)
            });
            lychi_core::files::text_extract::expand_doc_refs(&with_context)
        })
        .await
        .unwrap_or(user)
    };
    let cancel = CancellationToken::new();
    {
        let mut slot = state.ai_cancel.write().await;
        if let Some(prev) = slot.replace(cancel.clone()) {
            prev.cancel();
        }
    }

    // Continue the running conversation (follow-up) or start fresh. A fresh start
    // mints a new conversation id (for history persistence); a follow-up reuses
    // the existing one so it upserts the same history row.
    let session = if fresh {
        state.agent_session.write().await.take(); // drop any prior conversation
        *state.agent_conversation_id.write().await = Some(lychi_core::db::new_id());
        Session::new_with_images(system, user, images)
    } else {
        {
            let mut id = state.agent_conversation_id.write().await;
            if id.is_none() {
                *id = Some(lychi_core::db::new_id());
            }
        }
        match state.agent_session.write().await.take() {
            Some(mut s) => {
                // Keep the caller's system prompt authoritative on continue. This
                // is what escalation relies on: a quick-AI session (terse, no
                // tools) is promoted to the full agent prompt while retaining the
                // answer it already produced.
                s.set_system(system);
                s.push_user_with_images(user, images);
                s
            }
            None => Session::new_with_images(system, user, images),
        }
    };

    let mut session = session;
    session.set_last_user_display(display);

    let (coord, ()) = build_coordinator(&state, with_tools).await?;
    let (stream, handle) = coord.run(session, cancel);
    drive(
        app,
        state.ai_generation.clone(),
        generation,
        state.agent_session.clone(),
        state.db.clone(),
        state.agent_conversation_id.clone(),
        stream,
        handle,
    );
    Ok(())
}

/// Resolve a pending approval and resume the agent loop.
#[tauri::command]
#[specta::specta]
pub async fn agent_approve(
    approve: bool,
    generation: u64,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    let session = state
        .agent_session
        .write()
        .await
        .take()
        .ok_or_else(|| LychiError::Ai("no pending agent approval".to_string()))?;

    state.ai_generation.store(generation, Ordering::Relaxed);
    let cancel = CancellationToken::new();
    *state.ai_cancel.write().await = Some(cancel.clone());

    // Approval resumes the full agent (a tool was pending), so tools stay on.
    let (coord, ()) = build_coordinator(&state, true).await?;
    let decision = if approve {
        ApprovalDecision::Approve
    } else {
        ApprovalDecision::Reject { message: None }
    };
    let (stream, handle) = coord.resume(session, decision, cancel);
    drive(
        app,
        state.ai_generation.clone(),
        generation,
        state.agent_session.clone(),
        state.db.clone(),
        state.agent_conversation_id.clone(),
        stream,
        handle,
    );
    Ok(())
}
