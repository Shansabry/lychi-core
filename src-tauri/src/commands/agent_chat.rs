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
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::RwLock;

use crate::state::AppState;

// ── ToolExecutor adapter over the real Executor ──────────────────────────────

/// Adapts Lychi's `Executor` (Rules Engine, side effects, per-run context) to the
/// coordinator's `ToolExecutor` seam. The coordinator stays pure; all the
/// execution-context wiring lives here.
struct ExecutorAdapter {
    executor: Arc<RwLock<Executor>>,
    privacy: lychi_core::config::PrivacyConfig,
    /// For panel-refresh events: an agent tool that mutates notes/todos/
    /// reminders/snippets must notify the frontend exactly like a typed
    /// command does (`finalize_exec`), or the panel shows stale data until
    /// the next summon.
    app: AppHandle,
    /// Whether the ACTIVE model may be shown images (capability-learned; only
    /// a known rejection turns this off). Gates feeding a screenshot back to
    /// the model — a text-only model just gets the saved path in the text.
    vision_ok: bool,
}

impl ExecutorAdapter {
    /// Hide the launcher before a screenshot tool runs — the chat card sits in
    /// the middle of the screen and would occlude exactly what the user asked
    /// about ("analyze this chart"). Routed through the FE's `request-hide`
    /// (the ONE sanctioned hide primitive: blank-then-hide paints a clean
    /// frame first — a raw `.hide()` re-introduces the re-summon flash), with
    /// the same settle budget as the hotkey path's watchdog. The agent-busy
    /// guard held by `drive` keeps the focus loss from being read as a
    /// dismissal.
    async fn hide_for_capture(&self) {
        let _ = self.app.emit("lychi://request-hide", ());
        tokio::time::sleep(std::time::Duration::from_millis(220)).await;
    }

    /// Re-summon after the capture so the user watches the analysis arrive.
    /// The chat conversation survives the hide/show (the resume machinery
    /// preserves an active run across a re-summon).
    async fn reshow_after_capture(&self) {
        if let Some(w) = self.app.get_webview_window("main") {
            let _ =
                tauri::async_runtime::spawn_blocking(move || crate::window::show_window(&w)).await;
        }
    }

    /// Mirror `finalize_exec`'s panel notification for the agent path.
    fn notify_panel_mutation(&self, action_id: &str, success: bool) {
        if success && crate::commands::execute::PANEL_MUTATION_ACTIONS.contains(&action_id) {
            let _ = self.app.emit("lychi://notes-changed", ());
        }
    }
}

/// Upper bound on the tool-result text fed BACK to the model, per call.
///
/// This is deliberately far tighter than the shell handler's 256KB *display*
/// cap: that cap is about not jamming the WebView, whereas this is about not
/// burning the context window (and the user's tokens) on output the model does
/// not need. A chatty command — `zip` printing one `adding: …` line per file,
/// a long `ls -l`, a verbose build — can emit tens of thousands of tokens of
/// noise; the model only needs enough to see the shape of the result and any
/// error. ~8KB ≈ ~2k tokens, plenty for that. See `truncate_for_model`.
const MODEL_OUTPUT_MAX_BYTES: usize = 8 * 1024;

/// Truncate long tool output to `MODEL_OUTPUT_MAX_BYTES`, keeping the HEAD and
/// the TAIL (the two ends carry the signal — what a command started doing and
/// how it finished, including a trailing error) with a marker in between saying
/// how much was dropped. Short output is returned unchanged.
fn truncate_for_model(text: &str) -> String {
    if text.len() <= MODEL_OUTPUT_MAX_BYTES {
        return text.to_string();
    }
    // Split the budget between head and tail. Use char_indices so we never cut a
    // multi-byte char in half.
    let half = MODEL_OUTPUT_MAX_BYTES / 2;
    let head_end = text
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= half)
        .last()
        .unwrap_or(0);
    let tail_start = text
        .char_indices()
        .map(|(i, _)| i)
        .rev()
        .take_while(|&i| text.len() - i <= half)
        .last()
        .unwrap_or(text.len());
    let omitted = tail_start.saturating_sub(head_end);
    format!(
        "{}\n\n… [{} bytes of output omitted] …\n\n{}",
        &text[..head_end],
        omitted,
        &text[tail_start..]
    )
}

/// Render an `ActionResult` into the plain text the model sees as a tool result.
fn result_text(res: &lychi_core::action_registry::ActionResult) -> String {
    use lychi_core::action_registry::Output;
    if let Some(err) = &res.error {
        return truncate_for_model(err);
    }
    match &res.output {
        Output::Text { body, .. } => truncate_for_model(body),
        // The model reads tool results as text, so structured rows are
        // flattened HERE rather than by the handler. That is the point of the
        // split: the handler emits data once, and each consumer renders it for
        // its own medium — a card for the user, plain lines for the model.
        Output::Rows { sections } => {
            let mut out = String::new();
            for section in sections {
                if let Some(title) = &section.title {
                    out.push_str(&format!("{title}:\n"));
                }
                for row in &section.rows {
                    out.push_str(&row.title);
                    if let Some(b) = &row.badge {
                        out.push_str(&format!(" [{}]", b.text));
                    }
                    if let Some(s) = &row.subtitle {
                        out.push_str(&format!(" — {s}"));
                    }
                    out.push('\n');
                }
            }
            if out.is_empty() {
                "No results.".to_string()
            } else {
                truncate_for_model(out.trim_end())
            }
        }
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

/// Vision-encode a file a tool just saved (a screenshot), for the model AND
/// the UI. Returns `(image for the model, artifact for the tool step)` — both
/// `None` unless the result succeeded, tagged a file, and the current model is
/// not KNOWN to reject images (Unknown attaches and lets capability learning
/// record a rejection). Blocking image work runs off the async runtime.
async fn encode_tool_capture(
    res: &lychi_core::executor::ExecuteResult,
    vision_ok: bool,
) -> (Option<ImageSource>, Option<ToolArtifact>) {
    let Some(path) = res.result.saved_file.clone().filter(|_| res.result.success) else {
        return (None, None);
    };
    if !vision_ok {
        return (None, None);
    }
    let encoded = tauri::async_runtime::spawn_blocking(move || {
        lychi_core::files::image_ops::encode_image_for_vision(std::path::Path::new(&path))
    })
    .await;
    match encoded {
        Ok(Ok((media_type, data))) => {
            let artifact = ToolArtifact {
                kind: "image".into(),
                content: format!("data:{media_type};base64,{data}"),
            };
            (Some(ImageSource { media_type, data }), Some(artifact))
        }
        other => {
            tracing::warn!("[agent] could not vision-encode tool capture: {other:?}");
            (None, None)
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

/// Decide how an agent `run` should surface its output, and return the command
/// with any control sentinel stripped.
///
/// Default is **inline capture**: the model must be able to read what it ran,
/// and an agent command dumped into an external terminal is both invisible to
/// the model and a focus thief that dismisses the launcher (the multi-step bug).
/// The model opts into a real terminal — for `ssh`, an editor, a REPL, a
/// long-running foreground process the user wants to watch — with the typed
/// `terminal: true` field of `run`'s input schema, or the flat `--terminal`
/// sentinel (`shell_exec::TERMINAL_PREFIX`) it normalizes to.
/// Returns the executor input string, the `RunInputs`, and whether this
/// dispatch will open an EXTERNAL terminal window (`opens_terminal`).
///
/// `opens_terminal` is an explicit signal, NOT inferred from `RunInputs.inline`:
/// every non-`run` tool leaves `inline` at its `false` default but spawns no
/// terminal, so inferring from `inline` would raise the focus-theft guard for
/// `web`, `open`, `system`, etc. Only a `run` carrying the terminal sentinel
/// actually opens a window, and only that returns `true`.
fn agent_run_inputs(name: &str, args: &str) -> (String, RunInputs, bool) {
    // Only `run` has the inline/terminal duality; every other tool ignores
    // RunInputs.inline, so the default is fine and the sentinel never applies.
    if name != "run" {
        let input = if args.is_empty() {
            name.to_string()
        } else {
            format!("{name} {args}")
        };
        return (input, RunInputs::default(), false);
    }

    // Flatten a schema-typed `{"command":..,"terminal":..}` to the sentinel
    // form FIRST (a plain string passes through unchanged). This must happen
    // here, before the executor: the Rules Engine validates `run`'s args
    // verbatim (`rules/mod.rs` routes them straight into ShellRules), so risk
    // assessment has to read the exact flat command that will execute —
    // JSON-wrapped args would defeat its structural checks. The sentinel split
    // is the handler's own splitter, so adapter and handler cannot disagree.
    let flat = lychi_core::action_registry::handlers::shell_exec::run_args_to_flat(args);
    let (forced_terminal, cmd) =
        lychi_core::action_registry::handlers::shell_exec::split_terminal_sentinel(&flat);
    // The single "does this need a terminal?" rule lives in the `run` handler
    // and applies to the typed command and the agent alike. We consult the SAME
    // function here only so the agent's own `inline` flag (and thus whether a
    // streaming sink is wired) matches what the handler will actually do — an
    // `ssh` should report inline=false so no chat-stream sink is set up for
    // output that will go to a terminal. This is not a second decider: it calls
    // the one decider. `--terminal` remains a hard override.
    let want_terminal =
        forced_terminal || lychi_core::action_registry::handlers::shell_exec::needs_terminal(cmd);

    (
        format!("{name} {cmd}"),
        RunInputs {
            inline: !want_terminal,
            // An agent terminal launch opens a FRESH window rather than routing
            // into the user's currently-focused terminal: hijacking a terminal
            // the user is working in to run the agent's command is surprising,
            // and routing is a user-summoned convenience, not an agent one.
            terminal_routing: "off".to_string(),
            ..RunInputs::default()
        },
        want_terminal,
    )
}

#[async_trait]
impl ToolExecutor for ExecutorAdapter {
    async fn execute(
        &self,
        name: &str,
        args: &str,
        output: Option<lychi_core::coordinator::ToolOutputChannel>,
    ) -> Result<ToolOutcome, LychiError> {
        // GROUP-tool dispatch first: a call like `personal_data
        // {"action":"note_add",…}` resolves to its member handler + the flat
        // args that handler's parser AND the Rules Engine both read — resolved
        // here, before the executor, so risk assessment can never see
        // different args than execution (the same invariant `run`'s flatten
        // upholds below). Anything not a group tool falls through unchanged,
        // so handler ids remain directly callable (stale hints, recalled
        // conversations).
        {
            use lychi_core::action_registry::registry::GroupDispatch;
            let exec = self.executor.read().await.clone();
            match exec.registry.resolve_group_call(name, args) {
                GroupDispatch::NotAGroup => {}
                GroupDispatch::Invalid(msg) => {
                    // A malformed group call never reaches the executor; the
                    // message is the model-facing error result so it can fix
                    // the call.
                    return Ok(ToolOutcome::Ran {
                        output: msg,
                        is_error: true,
                        artifact: None,
                        image: None,
                    });
                }
                GroupDispatch::Resolved {
                    handler_id,
                    flat_args,
                } => {
                    // Pre-resolved: run the member handler directly instead of
                    // re-parsing a synthesized command line (which would be a
                    // second resolver). Group members are never `run`, so the
                    // inline/terminal duality and the streaming sink don't
                    // apply — RunInputs defaults are correct.
                    let intent = lychi_core::intent::ResolvedIntent {
                        action_id: handler_id,
                        args: flat_args,
                        routing: lychi_core::intent::RoutingMethod::Ai,
                    };
                    let run_inputs = RunInputs::default();
                    let capturing = intent.action_id == "screenshot";
                    if capturing {
                        self.hide_for_capture().await;
                    }
                    let res = exec
                        .run_resolved(intent.clone(), &self.privacy, &run_inputs)
                        .await;
                    if capturing {
                        self.reshow_after_capture().await;
                    }
                    let res = res?;
                    self.notify_panel_mutation(&intent.action_id, res.result.success);
                    if let Some(reason) = res.envelope.needs_confirmation {
                        let pending = res.pending_intent.unwrap_or(intent);
                        let token = ResumeToken(serde_json::json!({
                            "action_id": pending.action_id,
                            "args": pending.args,
                            "inline": run_inputs.inline,
                            "terminal_routing": run_inputs.terminal_routing,
                            // When this confirmation IS a consent prompt, carry
                            // the typed feature key so "Always allow" grants the
                            // CONSENT (the thing the prompt asks about) — an
                            // action grant can never bypass the consent gate,
                            // so without this the prompt re-asks forever.
                            "consent_feature": res.envelope.consent_feature,
                        }));
                        return Ok(ToolOutcome::NeedsApproval {
                            reason,
                            resume: token,
                        });
                    }
                    let (image, capture_artifact) = encode_tool_capture(&res, self.vision_ok).await;
                    let (output, artifact) = result_summary_and_artifact(&res.result);
                    return Ok(ToolOutcome::Ran {
                        output,
                        is_error: !res.result.success,
                        artifact: capture_artifact.or(artifact),
                        image,
                    });
                }
            }
        }

        let (input, mut run_inputs, _opens_terminal) = agent_run_inputs(name, args);
        // Bridge the coordinator's live-output channel into the executor's
        // `OutputSink` on RunInputs, so a captured `run` streams each line into
        // the chat as it happens. Only for INLINE runs — a `--terminal` command's
        // output goes to the external terminal, not the chat. The sink (and thus
        // the sender) is dropped when this method returns, which closes the
        // channel and lets the coordinator's forwarder task finish.
        if let Some(ch) = output
            && run_inputs.inline
        {
            run_inputs.sink = Some(lychi_core::action_registry::OutputSink::new(ch));
        }
        // The launcher's dismiss guard (the `agent_busy` flag) is owned by
        // `drive` for the whole run's lifetime, so an external terminal, a
        // `pkexec` polkit dialog, a file picker — any focus thief a tool
        // triggers — is covered without the adapter tracking spawns.
        //
        // Snapshot-then-release, same as execute_command: an agent tool can be
        // a slow shell command, and holding the guard across it queued every
        // launcher keystroke behind the next config save's blocking_write.
        let exec = self.executor.read().await.clone();
        let capturing = name == "screenshot";
        if capturing {
            self.hide_for_capture().await;
        }
        let res = exec.run(&input, false, &self.privacy, &run_inputs).await;
        if capturing {
            self.reshow_after_capture().await;
        }
        let res = res?;

        // Destructive → the Rules Engine flagged it; pause for approval. Carry the
        // exact assessed intent in the resume token so approval runs THAT action.
        if let Some(reason) = res.envelope.needs_confirmation {
            let intent = res.pending_intent.ok_or_else(|| {
                LychiError::Ai("needs_confirmation without a pending intent".into())
            })?;
            let token = ResumeToken(serde_json::json!({
                "action_id": intent.action_id,
                "args": intent.args,
                // Preserve the output-surface decision across the approval: an
                // approved inline `run` must stay inline, not silently revert to
                // an external terminal (the default) when resumed.
                "inline": run_inputs.inline,
                "terminal_routing": run_inputs.terminal_routing,
                // See the group-dispatch token: lets "Always allow" grant a
                // consent prompt's feature instead of an inert action grant.
                "consent_feature": res.envelope.consent_feature,
            }));
            return Ok(ToolOutcome::NeedsApproval {
                reason,
                resume: token,
            });
        }

        // Standalone tools are handler ids (the group path returned above).
        self.notify_panel_mutation(name, res.result.success);
        let (image, capture_artifact) = encode_tool_capture(&res, self.vision_ok).await;
        let (output, artifact) = result_summary_and_artifact(&res.result);
        Ok(ToolOutcome::Ran {
            output,
            is_error: !res.result.success,
            artifact: capture_artifact.or(artifact),
            image,
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
        // Restore the output-surface decision the token captured at assess time.
        // Default to inline when the field is absent (an older token, or a
        // non-`run` action that never wrote it): inline is the inert default —
        // it spawns no terminal and only `run` reads the field anyway, so an
        // absent value can never spuriously open a terminal window.
        let run_inputs = RunInputs {
            inline: resume.0["inline"].as_bool().unwrap_or(true),
            terminal_routing: resume.0["terminal_routing"]
                .as_str()
                .unwrap_or("off")
                .to_string(),
            ..RunInputs::default()
        };
        // Snapshot-then-release (see `execute` above) — approved actions are
        // the destructive ones, i.e. exactly the slow-shell candidates.
        let exec = self.executor.read().await.clone();
        let action_id = intent.action_id.clone();
        let res = exec
            .run_confirmed(intent, &self.privacy, &run_inputs)
            .await?;
        self.notify_panel_mutation(&action_id, res.result.success);
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
    /// tool_output_delta | tool_failed | awaiting_approval | final | stopped | error.
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
    /// How many input tokens were prompt-cache hits (kind = usage). Lets the UI
    /// show the caching working; 0/None when the provider doesn't report it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u32>,
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
            cached_input_tokens: None,
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
            AgentEvent::ToolOutputDelta { call_id, chunk } => {
                d.kind = "tool_output_delta".into();
                d.call_id = Some(call_id);
                d.text = Some(chunk);
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
                cached_input_tokens,
            } => {
                d.kind = "usage".into();
                d.input_tokens = Some(input_tokens);
                d.output_tokens = Some(output_tokens);
                d.cached_input_tokens = Some(cached_input_tokens);
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
/// Build the agent loop and the capability-manifest text for its system prompt.
///
/// The returned `String` is the generated manifest (tools + AI commands) to fold
/// into the session's system message. Present whenever `with_tools` is set — that
/// covers BOTH the full agent and the quick-AI fork card, since both can call
/// tools and so both benefit from knowing the menu. Empty only for a preset run
/// (`with_tools = false`): a text transform calls nothing, so it needs no catalog.
///
/// Generated from the FULL registry catalog + presets even though the callable
/// tool SCHEMAS are filtered per query — the model should KNOW every capability
/// exists; only the schemas cost real tokens.
async fn build_coordinator(
    state: &AppState,
    app: &AppHandle,
    with_tools: bool,
) -> Result<(Coordinator<ExecutorAdapter>, String), LychiError> {
    let provider = state
        .ai_provider
        .read()
        .await
        .clone()
        .ok_or_else(|| LychiError::Ai("AI is not configured".to_string()))?;
    let privacy = state.config.read().await.privacy.clone();

    // Tool catalog from the live registry — every handler is a callable tool.
    // Empty for quick-AI (answer only, no acting). The FULL catalog is handed to
    // the coordinator, which re-selects the model-facing shortlist from the
    // evolving conversation each turn (no pre-filter here — a frozen opening-
    // query set would strand a tool a later step needs).
    // The manifest describes every tool + AI command in prose (cheap awareness),
    // built from the FULL catalog. Empty when tools are off. Presets read from
    // the same DB the store uses.
    let mut manifest = String::new();

    let tools: Vec<ToolDef> = if with_tools {
        // The MODEL-facing projection: grammared handlers folded into their
        // group tools (compound actions, merged operand schemas, per-action
        // mutation lists), everything else standalone with its usage() folded
        // into the description. Small (~10 group tools once migration lands)
        // and byte-stable across turns — which is what makes the provider's
        // prompt cache actually hit. Group calls are resolved back to member
        // handlers by the ExecutorAdapter, so the executor and Rules Engine
        // only ever see handler ids + flat args.
        let catalog = state.executor.read().await.registry.model_catalog();
        let presets = lychi_core::ai_presets::store::AiPresetsStore::new()
            .get_presets(&state.db)
            .unwrap_or_default();
        // No prose manifest: the tool schemas carry the knowledge. The presets
        // list is still surfaced (it is small and not a tool schema).
        manifest = lychi_core::coordinator::build_presets_note(&presets);

        catalog
            .into_iter()
            .map(|m| ToolDef {
                name: m.name,
                description: m.description,
                mutates: m.mutates,
                mutating_actions: m.mutating_actions,
                input_schema: m.input_schema,
            })
            .collect()
    } else {
        Vec::new()
    };

    // Whether the active model may see images: only a capability-learned
    // rejection disables it (Unknown tries and learns). Snapshot here, per
    // coordinator build — a model switch rebuilds on the next turn anyway.
    let vision_ok = {
        let ai = state.config_snapshot(|c| c.ai.clone()).await;
        !matches!(
            lychi_core::providers::capability::get_vision(&ai.provider, &ai.model),
            lychi_core::providers::capability::Vision::Unsupported
        )
    };
    let adapter = Arc::new(ExecutorAdapter {
        executor: state.executor.clone(),
        privacy,
        app: app.clone(),
        vision_ok,
    });
    Ok((Coordinator::new(provider, adapter, tools), manifest))
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
    conversation_id: Arc<RwLock<Option<String>>>,
    agent_busy: Arc<std::sync::atomic::AtomicBool>,
    mut stream: lychi_core::coordinator::AgentEventStream,
    handle: lychi_core::coordinator::OutcomeHandle,
) {
    // The agent run is now in flight: hold the launcher's dismiss-on-blur for its
    // whole lifetime, so any focus thief a tool triggers (a spawned terminal, a
    // pkexec/polkit password dialog for a package install, a file picker) can't
    // dismiss the chat out from under the user. Lowered when the run reaches a
    // final outcome below. Escape still dismisses deliberately.
    agent_busy.store(true, Ordering::SeqCst);
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
        // Lower the busy guard when the run REACHES A FINAL state
        // (Done/Stopped/Error) or is superseded — no more of this run's tool
        // calls can fire, so the launcher may self-dismiss on blur again. An
        // `AwaitingApproval` is NOT final (the run continues once the user
        // decides, and a focus thief may still be up), so it deliberately keeps
        // the guard raised. A superseded generation (match skipped) still clears
        // via this default, so the flag can never stay stuck for the session.
        let is_awaiting = matches!(outcome, Outcome::AwaitingApproval { .. });
        if !is_awaiting {
            agent_busy.store(false, Ordering::SeqCst);
        }
        if state_generation.load(Ordering::Relaxed) == generation {
            match outcome {
                Outcome::AwaitingApproval { session, .. } => {
                    // Paused awaiting the user's approval — the run is still in
                    // flight, so the busy guard stays raised and the chat (with
                    // its approval prompt) can't be dismissed by focus loss.
                    persist_conversation(&conversation_id, &session).await;
                    *pending_session.write().await = Some(session);
                }
                Outcome::Done { session } => {
                    // A completed turn — persist it to history so it can be
                    // recalled later, then stash for the next turn.
                    persist_conversation(&conversation_id, &session).await;
                    *pending_session.write().await = Some(session);
                }
                Outcome::Stopped { session, .. } => {
                    persist_conversation(&conversation_id, &session).await;
                    *pending_session.write().await = Some(session);
                }
                Outcome::Error { session, .. } => {
                    // An infrastructure error ends the TURN, not the
                    // conversation: the messages up to the failure (including
                    // partial prose the user already read) are valid context.
                    // This arm used to clear the slot without persisting while
                    // the conversation id survived — the next follow-up then
                    // upserted an empty session OVER the stored transcript.
                    match session {
                        Some(session) => {
                            persist_conversation(&conversation_id, &session).await;
                            *pending_session.write().await = Some(session);
                        }
                        // The loop task itself was lost — nothing to save.
                        None => *pending_session.write().await = None,
                    }
                }
            }
        }
    });
}

/// Upsert the conversation into history (best-effort; a persist failure never
/// breaks the chat). Runs off the async runtime since file I/O is blocking.
async fn persist_conversation(conversation_id: &Arc<RwLock<Option<String>>>, session: &Session) {
    let Some(id) = conversation_id.read().await.clone() else {
        return;
    };
    let messages = session.messages.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        let store = lychi_core::ai_history::store::AiHistoryStore::new();
        if let Err(e) = store.upsert(&id, &messages) {
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

/// Everything the caller supplies to start a turn.
///
/// Grouped into a struct because the flat argument list had grown to seven
/// caller-supplied values, four of them `bool`/`u64`/`String` — the shape where
/// a transposed pair of arguments still compiles and fails at runtime. Naming
/// them at the call site removes that class of mistake, and specta turns this
/// into a TypeScript object so the frontend gains the same protection.
#[derive(serde::Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentChatStart {
    pub system: String,
    pub user: String,
    /// Start a new conversation rather than continuing the current one.
    pub fresh: bool,
    /// Expose the tool set to the model (plain chat when false).
    pub with_tools: bool,
    /// Monotonic turn id; a stale stream's tokens are dropped by the frontend.
    pub generation: u64,
    #[serde(default)]
    pub images: Vec<String>,
    /// How the turn renders when that differs from its content (a preset that
    /// folded a large payload into a chip). Persisted with the message so a
    /// RECALLED conversation renders identically, keeping the sender the only
    /// decider of that split. Never sent to a provider.
    #[serde(default)]
    pub display: Option<lychi_core::providers::MessageDisplay>,
}

#[tauri::command]
#[specta::specta]
pub async fn agent_chat_start(
    params: AgentChatStart,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    let AgentChatStart {
        system,
        user,
        fresh,
        with_tools,
        generation,
        images,
        display,
    } = params;
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
    // Ambient context rides the LATEST user turn as a trailing `<context>`
    // block — never the system prompt, which must stay byte-stable across turns
    // (provider prompt caching + idempotent manifest splice). Full-agent turns
    // only: a preset is a pure text transform and gets no ambient state. The
    // block enters the WIRE content; the UI keeps showing what the user typed —
    // when the caller supplied no display split, one is synthesized so a
    // recalled conversation renders the typed text plus a "Context" chip
    // instead of the raw block.
    let (user, display) = if with_tools {
        let ctx = state.executor.read().await.context.clone();
        let block = lychi_core::context::agent_context_block(ctx.as_ref());
        let display = display.or_else(|| {
            Some(lychi_core::providers::MessageDisplay {
                instruction: user.clone(),
                label: "Context".to_string(),
                body: block.clone(),
            })
        });
        (format!("{user}\n\n{block}"), display)
    } else {
        (user, display)
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
            None => {
                // No stashed session to continue (superseded mid-turn, or a
                // first message sent as a follow-up). Whatever the current id's
                // history row holds is a transcript this empty session never
                // saw — reusing the id would make the next upsert REPLACE it.
                // New session ⇒ new id, unconditionally.
                *state.agent_conversation_id.write().await = Some(lychi_core::db::new_id());
                Session::new_with_images(system, user, images)
            }
        }
    };

    let mut session = session;
    session.set_last_user_display(display);

    let (coord, manifest) = match build_coordinator(&state, &app, with_tools).await {
        Ok(c) => c,
        Err(e) => {
            // Failed before the loop ever ran (e.g. AI disabled
            // mid-conversation). Put the taken session back — a build error
            // must not amputate the conversation it never touched. (The
            // un-answered user message stays in it; both wire dialects accept
            // consecutive user turns on retry.)
            *state.agent_session.write().await = Some(session);
            return Err(e);
        }
    };
    // Fold the capability manifest into the system prompt so the model knows its
    // tools + AI commands. `splice_manifest` is idempotent (it replaces any prior
    // manifest), so a follow-up turn on a continued session doesn't stack copies;
    // an empty manifest (tools off) leaves the prompt as-is.
    {
        let base = session.system_prompt();
        session.set_system(lychi_core::coordinator::splice_manifest(&base, &manifest));
    }
    let (stream, handle) = coord.run(session, cancel);
    drive(
        app,
        state.ai_generation.clone(),
        generation,
        state.agent_session.clone(),
        state.agent_conversation_id.clone(),
        state.agent_busy.clone(),
        stream,
        handle,
    );
    Ok(())
}

/// Persist an "Always allow" grant for the pending approval, so future
/// invocations of the same action run without asking. `run` commands become a
/// prefix regex in `shell_policy.allow` (the shell's own decider); every other
/// handler becomes an `approved_actions` entry — "handler verb" when the
/// grammar is verbed, the bare handler id otherwise. Saved through the same
/// lock-then-emit discipline as `save_commands_config`, so the CommandsReactor
/// rebuilds the Rules Engine and the grant is live for the very next call.
async fn persist_always_allow(
    state: &AppState,
    action_id: &str,
    args: &str,
    consent_feature: Option<&str>,
) -> Result<(), LychiError> {
    use lychi_core::events::{ConfigSection, DomainEvent};

    // A CONSENT prompt's "Always allow" grants the consent itself — that is
    // what the prompt asked ("Allow web access and remember?"), and an action
    // grant would be inert against it (consent is checked before grants, by
    // design). Saved before the resume rebuilds the coordinator, so even the
    // rest of THIS run sees the grant.
    if let Some(feature) = consent_feature {
        let mut config = state.config.write().await;
        if lychi_core::rules::grant_consent_key(&mut config.privacy, feature) {
            tracing::info!(%feature, "[agent] always-allow consent grant");
            config.save(&lychi_core::paths::config_file())?;
            return Ok(());
        }
        tracing::warn!(%feature, "[agent] unknown consent feature on approval token");
    }

    let entry = if action_id == "run" {
        let (_, cmd) =
            lychi_core::action_registry::handlers::shell_exec::split_terminal_sentinel(args);
        Some(lychi_core::rules::shell::allow_pattern_for(cmd))
    } else {
        None
    };

    // Verbed grammar → grant this verb only; free-form → the whole handler.
    let action_entry = if action_id == "run" {
        None
    } else {
        let executor = state.executor.read().await;
        let verbed = executor
            .registry
            .get(action_id)
            .and_then(|h| h.grammar())
            .is_some_and(|g| !g.is_free_form());
        let first = args.split_whitespace().next().unwrap_or("");
        Some(if verbed && !first.is_empty() {
            format!("{action_id} {first}")
        } else {
            action_id.to_string()
        })
    };

    {
        let mut config = state.config.write().await;
        match (entry, action_entry) {
            (Some(pattern), _) => {
                if !config.commands.shell_policy.allow.contains(&pattern) {
                    tracing::info!(%pattern, "[agent] always-allow shell grant");
                    config.commands.shell_policy.allow.push(pattern);
                }
            }
            (None, Some(entry)) => {
                if !config.commands.approved_actions.contains(&entry) {
                    tracing::info!(%entry, "[agent] always-allow action grant");
                    config.commands.approved_actions.push(entry);
                }
            }
            (None, None) => {}
        }
        config.save(&lychi_core::paths::config_file())?;
    }
    // Release the write lock BEFORE emitting — the reactors take these locks
    // with blocking_* (same discipline as save_commands_config).
    state
        .event_bus
        .emit_from_async(DomainEvent::ConfigChanged {
            section: ConfigSection::Commands,
        })
        .await;
    Ok(())
}

/// Resolve a pending approval and resume the agent loop. `decision` is
/// "approve", "always" (approve + persist an always-allow grant), or "reject"
/// (deny-and-continue: the refusal feeds back to the model as a tool result).
#[tauri::command]
#[specta::specta]
pub async fn agent_approve(
    decision: String,
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

    if decision == "always" {
        // The resume token carries the EXACT assessed action — grant that,
        // not a re-parse of the display strings.
        if let Some(pending) = session.pending.first() {
            let action_id = pending.resume.0["action_id"].as_str().unwrap_or_default();
            let args = pending.resume.0["args"].as_str().unwrap_or_default();
            let consent = pending.resume.0["consent_feature"].as_str();
            if !action_id.is_empty()
                && let Err(e) = persist_always_allow(&state, action_id, args, consent).await
            {
                // The grant failing must not strand the approval — approve
                // this run anyway and tell the log.
                tracing::warn!("[agent] always-allow grant failed: {e}");
            }
        }
    }

    state.ai_generation.store(generation, Ordering::Relaxed);
    let cancel = CancellationToken::new();
    *state.ai_cancel.write().await = Some(cancel.clone());

    // Approval resumes the full agent (a tool was pending), so tools stay on.
    // Empty query → full catalog: mid-task we must not drop a tool the ongoing
    // plan may still call. The manifest is ignored on resume — the continued
    // session already carries it in its system prompt from the initial turn.
    let (coord, _manifest) = match build_coordinator(&state, &app, true).await {
        Ok(c) => c,
        Err(e) => {
            // Restore the taken session: the approval is still pending and the
            // user can retry once the build error (e.g. AI disabled) is fixed.
            *state.agent_session.write().await = Some(session);
            return Err(e);
        }
    };
    let decision = if decision == "reject" {
        ApprovalDecision::Reject { message: None }
    } else {
        ApprovalDecision::Approve
    };
    let (stream, handle) = coord.resume(session, decision, cancel);
    drive(
        app,
        state.ai_generation.clone(),
        generation,
        state.agent_session.clone(),
        state.agent_conversation_id.clone(),
        state.agent_busy.clone(),
        stream,
        handle,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_tool_output_is_passed_through_unchanged() {
        let s = "adding: a.jpg\nadding: b.jpg\ndone";
        assert_eq!(truncate_for_model(s), s);
    }

    #[test]
    fn long_tool_output_is_truncated_head_and_tail() {
        // Simulate a chatty `zip` — one line per file, far over the budget.
        let body: String = (0..5000)
            .map(|i| format!("  adding: file{i}.jpg\n"))
            .collect();
        assert!(
            body.len() > MODEL_OUTPUT_MAX_BYTES,
            "test setup: body too small"
        );
        let out = truncate_for_model(&body);
        assert!(
            out.len() < body.len(),
            "truncated output must be smaller than the original"
        );
        assert!(
            out.len() <= MODEL_OUTPUT_MAX_BYTES + 128,
            "truncated output must be near the budget, was {}",
            out.len()
        );
        // Head and tail are preserved so the model sees the shape + any trailing
        // error, with a marker in between.
        assert!(out.starts_with("  adding: file0.jpg"), "head kept");
        assert!(out.trim_end().ends_with(".jpg"), "tail kept");
        assert!(out.contains("bytes of output omitted"), "marker present");
    }

    #[test]
    fn a_plain_run_is_inline_by_default() {
        let (input, inputs, opens_terminal) = agent_run_inputs("run", "zip -r out.zip src");
        assert_eq!(input, "run zip -r out.zip src");
        assert!(
            inputs.inline,
            "the agent must capture output, not open a terminal"
        );
        assert!(!opens_terminal, "a plain run opens no window");
    }

    #[test]
    fn an_interactive_command_auto_routes_to_a_terminal_without_the_prefix() {
        // `ssh` needs a real TTY, so it must open a terminal even though the
        // model did NOT prefix `--terminal` — the auto TTY-detection (shared with
        // the typed `run` command) decides. inline=false → no chat-stream sink.
        let (input, inputs, opens_terminal) = agent_run_inputs("run", "ssh nimbus");
        assert_eq!(input, "run ssh nimbus");
        assert!(
            !inputs.inline,
            "an interactive command is not captured inline"
        );
        assert!(opens_terminal, "ssh auto-opens a terminal");
    }

    #[test]
    fn the_terminal_sentinel_opens_a_terminal_and_is_stripped() {
        let (input, inputs, opens_terminal) = agent_run_inputs("run", "--terminal ssh nimbus");
        assert_eq!(
            input, "run ssh nimbus",
            "the sentinel must not reach the shell"
        );
        assert!(
            !inputs.inline,
            "an explicit --terminal must open a real terminal"
        );
        assert!(
            opens_terminal,
            "the guard must know this dispatch opens a window"
        );
        assert_eq!(
            inputs.terminal_routing, "off",
            "an agent terminal is a fresh window, never a hijack of the user's"
        );
    }

    #[test]
    fn a_bare_terminal_word_in_a_command_is_not_the_sentinel() {
        // A mid-command "--terminal" must NOT be treated as the control prefix —
        // only a leading, whitespace- or end-delimited token counts.
        let (input, inputs, opens_terminal) = agent_run_inputs("run", "echo --terminal is a flag");
        assert_eq!(input, "run echo --terminal is a flag");
        assert!(inputs.inline);
        assert!(!opens_terminal);

        // "--terminals" (no delimiter after the word) is a different token.
        let (_, inputs, opens_terminal) = agent_run_inputs("run", "--terminals foo");
        assert!(
            inputs.inline,
            "--terminals is a different token, not the sentinel"
        );
        assert!(!opens_terminal);
    }

    #[test]
    fn a_non_run_tool_never_opens_a_terminal_despite_inline_false() {
        // The critical guard case: every non-`run` tool leaves inline at its
        // `false` default but spawns NO terminal, so `opens_terminal` must be
        // false — inferring the focus-theft guard from `inline` here would raise
        // it for web/open/system and wrongly suppress a real click-away dismiss.
        let (input, inputs, opens_terminal) = agent_run_inputs("web", "rust async");
        assert_eq!(input, "web rust async");
        assert!(
            !inputs.inline,
            "non-run tools carry the inline=false default"
        );
        assert!(
            !opens_terminal,
            "a non-run tool opens no terminal regardless of the inline default"
        );

        // Even when the text happens to start with the sentinel word — only
        // `run` interprets it; every other tool passes it through untouched.
        let (input, _, opens_terminal) = agent_run_inputs("web", "--terminal something");
        assert_eq!(input, "web --terminal something");
        assert!(!opens_terminal);
    }

    #[test]
    fn a_schema_typed_run_is_flattened_before_the_executor() {
        // A schema-constrained model sends `{"command":..,"terminal":..}`. The
        // adapter must flatten it HERE, before the executor, so the Rules
        // Engine's shell validation reads the exact command that runs — raw
        // JSON must never reach ShellRules.
        let (input, inputs, opens_terminal) = agent_run_inputs("run", r#"{"command":"ls -la"}"#);
        assert_eq!(input, "run ls -la", "JSON must not reach the executor");
        assert!(inputs.inline);
        assert!(!opens_terminal);

        // The typed terminal boolean behaves exactly like the flat sentinel.
        let (input, inputs, opens_terminal) =
            agent_run_inputs("run", r#"{"command":"ssh nimbus","terminal":true}"#);
        assert_eq!(input, "run ssh nimbus");
        assert!(!inputs.inline);
        assert!(opens_terminal);
    }

    #[test]
    fn an_empty_run_falls_through_to_the_usage_guard() {
        // A bare `--terminal` with no command leaves an empty command; the
        // handler's own "Usage: run …" guard reports it — we don't second-guess.
        // It still counts as a terminal dispatch (the sentinel was present).
        let (input, inputs, opens_terminal) = agent_run_inputs("run", "--terminal");
        assert_eq!(input, "run ");
        assert!(!inputs.inline);
        assert!(opens_terminal);
    }
}
