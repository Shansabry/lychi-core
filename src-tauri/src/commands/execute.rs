use std::os::unix::process::CommandExt;
use std::process::Stdio;

use tauri::{AppHandle, Emitter, Manager, State};

use lychi_core::action_registry::{CommandInfo, CommandResultDto, CompletionItem};
use lychi_core::db::frecency;
use lychi_core::error::LychiError;

use crate::state::AppState;

/// Action IDs that mutate panel data (notes, todos, reminders).
const PANEL_MUTATION_ACTIONS: &[&str] = &["note", "todo", "reminder"];

/// Execute a command.
///
/// `query` is what the user had TYPED when they chose this command, when it
/// differs from `input`. Supplied only when a suggestion was SELECTED, so the
/// launcher can learn `query → chosen command` (see `frecency::record_latch`).
/// `None` for a directly-typed command — a query that is its own answer teaches
/// nothing.
#[tauri::command]
#[specta::specta]
pub async fn execute_command(
    input: String,
    confirmed: Option<bool>,
    run_inline: Option<bool>,
    query: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<CommandResultDto, LychiError> {
    // Frecency for fuzzy-search ranking is recorded up front — it reflects what
    // the user typed/accepted, independent of the command's exit status.
    // History (which drives suggestions/ghost autocomplete) is recorded LATER,
    // only if the command succeeds, so failed commands (e.g. "run run htop" →
    // "command not found") never get suggested back to the user.
    let trimmed = input.trim();
    if !trimmed.is_empty() {
        let _ = frecency::record(&state.db, &format!("history:{trimmed}"));
    }

    // Record workspace-scoped frecency (command memory per project)
    if !trimmed.is_empty() {
        let executor_r = state.executor.read().await;
        if let Some(ref ctx) = executor_r.context {
            let project_root = ctx
                .project
                .as_ref()
                .map(|p| p.root.as_str())
                .or(ctx.cwd.as_deref());
            if let Some(root) = project_root {
                let _ = frecency::record_workspace(&state.db, root, trimmed);
            }
        }
        // Suggestion learning loop: executing a command we just suggested
        // counts as acceptance — boosts it (record_suggestion) AND ticks the
        // acceptance side of the CTR store (record_acceptance) so the panel
        // self-tunes toward what the user actually picks.
        if let Some(context_key) = executor_r.suggestion_acceptance(trimmed) {
            tracing::debug!("[suggest] acceptance: '{trimmed}' in {context_key}");
            let _ = frecency::record_suggestion(&state.db, &context_key, trimmed);
            let _ = frecency::record_acceptance(&state.db, &context_key, trimmed);
        }
        drop(executor_r);

        // Query→command latching. Deliberately OUTSIDE the block above, which
        // needs both a matching context and a previously-shown suggestion: a
        // latch is keyed by the query itself, so it must also be learned when
        // there is no project or focused app — the case where per-context
        // learning records nothing at all.
        if let Some(q) = query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            tracing::debug!("[latch] '{q}' → '{trimmed}'");
            let _ = frecency::record_latch(&state.db, q, trimmed);
        }

        // Learn the user's fallback preference: running an `ask …`/`web …` on a
        // free-text query is choosing that escape hatch. Frecency then orders the
        // two fallback rows so the preferred one leads next time.
        let lower = trimmed.to_lowercase();
        if let Some(action) = ["ask", "web"]
            .into_iter()
            .find(|a| lower.starts_with(&format!("{a} ")))
        {
            let _ = frecency::record_fallback_choice(&state.db, action);
        }
    }

    // If context is soft-stale, kick off a background re-gather before routing.
    // This covers "Lychi was open, user came back, immediately hit Enter" — no summon fires.
    // The fresh context arrives async; this execution uses the current (stale) context but
    // subsequent completions and the next command will see fresh state.
    {
        let executor_r = state.executor.read().await;
        let is_stale = executor_r
            .context
            .as_ref()
            .is_some_and(|ctx| ctx.is_soft_stale());
        drop(executor_r);

        if is_stale {
            lychi_core::context::metrics::inc_stale_refresh_triggered();
            tracing::debug!("[execute_command] context is stale — triggering background re-gather");
            let _ = app.emit("lychi://context-stale", ());
            let ctx_handle = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let ctx = lychi_core::context::gather(None);
                let state = ctx_handle.state::<crate::state::AppState>();
                if let Ok(mut executor) = state.executor.try_write() {
                    executor.context = Some(ctx.clone());
                }
                let _ = ctx_handle.emit("lychi://context-ready", &ctx);
            });
        }
    }

    // Build the per-run inputs threaded into the executor: terminal + routing
    // mode come from config; `inline` (e.g. Shift+Enter) forces the next `run`
    // command to capture output inline instead of opening a terminal. The
    // executor prefers the context-detected terminal and falls back to
    // `inputs.terminal`.
    let inputs = {
        let config = state.config.read().await;
        lychi_core::executor::RunInputs {
            terminal: Some(config.commands.terminal.clone()),
            terminal_routing: config.commands.terminal_routing.clone(),
            inline: run_inline.unwrap_or(false),
            // No live sink on the direct-execute path — its captured output goes
            // to the result panel, not a streaming chat block.
            sink: None,
        }
    };

    // Snapshot config FIRST and release it, then take the executor — the two
    // guards must never overlap. See `AppState`'s lock-discipline note.
    let privacy = state.config_snapshot(|c| c.privacy.clone()).await;

    // Run through executor pipeline: resolve → validate → execute.
    //
    // SNAPSHOT, then release the lock — never hold the guard across the run.
    // The guard used to live for the whole handler execution, and the reactors
    // take blocking_write on this fair RwLock: one slow handler plus one
    // settings save queued EVERY later read (completions per keystroke) behind
    // the writer — the launcher accepted keystrokes and showed nothing. The
    // clone is cheap (Arc'd handlers) and shares the gate/tracker/router state,
    // so semantics are identical; see the note on `Executor`'s Clone impl.
    let executor = state.executor.read().await.clone();
    let exec = executor
        .run(&input, confirmed.unwrap_or(false), &privacy, &inputs)
        .await?;

    // G1: if this returned a pending confirmation, store the EXACT assessed intent
    // so `confirm_execution` runs it verbatim (no re-resolve). Any prior pending is
    // replaced (a new prompt supersedes an unanswered one).
    if let Some(intent) = exec.pending_intent.clone() {
        let risk = exec
            .envelope
            .risk_level
            .unwrap_or(lychi_core::action_registry::RiskLevel::High);
        *state.pending_execution.write().await =
            Some(crate::state::PendingExecution::new(intent, risk));
    }

    finalize_exec(&app, state.inner(), &input, exec).await
}

/// Shared post-execution tail: notify panel mutations, flatten to the wire DTO,
/// perform app-launch / window-focus platform side-effects, and record history.
/// Used by both `execute_command` and `confirm_execution` so the confirm path
/// gets identical launch/navigate/focus handling without duplicating it.
async fn finalize_exec(
    app: &AppHandle,
    state: &AppState,
    input: &str,
    exec: lychi_core::executor::ExecuteResult,
) -> Result<CommandResultDto, LychiError> {
    // Notify frontend when notes/todos/reminders are mutated by a handler
    if exec.result.success && PANEL_MUTATION_ACTIONS.contains(&exec.action_id.as_str()) {
        let _ = app.emit("lychi://notes-changed", ());
    }

    // Flatten the handler result + executor envelope into the wire DTO. The
    // launch/navigate logic below reads the flat fields (launch_desktop,
    // focus_app, …) off this DTO.
    let mut dto = CommandResultDto::build(exec.result, exec.envelope);

    // Launch desktop app. Primary path: GIO DesktopAppInfo + GDK AppLaunchContext,
    // which handles D-Bus activation, Terminal=true, and Wayland activation tokens.
    // Fallback: gtk-launch, which is what KDE Plasma uses and handles edge cases
    // that GIO misses on KDE/Wayland (session env, DBus activation for JetBrains, etc.).
    if let Some(ref desktop_path) = dto.launch_desktop {
        let path = desktop_path.clone();
        let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();

        // Step 1: try GIO on the GLib main thread (required for GDK context).
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        glib::MainContext::default().invoke(move || {
            use gio::prelude::*;
            let result = (|| {
                let app_info = gio::DesktopAppInfo::from_filename(&path)
                    .ok_or_else(|| format!("failed to load .desktop: {path}"))?;
                let context: Option<gio::AppLaunchContext> = gdk::Display::default()
                    .and_then(|d| d.app_launch_context())
                    .map(|c| c.into());
                if context.is_none() {
                    tracing::warn!(
                        "[open] no GDK display context (wayland={is_wayland}) — \
                         Wayland activation token unavailable for {path}"
                    );
                }
                // Strip Lychi's AppImage env from the launched app's environment,
                // so it loads the SYSTEM GTK/libs — not the bundled ones in our
                // FUSE mount, which crashed GTK apps intermittently. GIO launches
                // with the context's env, so apply the same overrides here as we
                // do for subprocess spawns (see crate::spawn_env). Needs a context
                // to carry them; build one if the display didn't provide it.
                let context = context.or_else(|| Some(gio::AppLaunchContext::new()));
                if let Some(ctx) = &context {
                    use gio::prelude::AppLaunchContextExt;
                    for (key, value) in
                        lychi_core::spawn_env::desktop_env_overrides(|k| std::env::var(k).ok())
                    {
                        match value {
                            Some(v) => ctx.setenv(&key, &v),
                            None => ctx.unsetenv(&key),
                        }
                    }
                }
                app_info
                    .launch(&[], context.as_ref())
                    .map_err(|e| format!("GIO launch failed: {e}"))
            })();
            let _ = tx.send(result);
        });
        let gio_result = rx.await.unwrap_or(Err("GIO channel dropped".into()));

        // Step 2: if GIO failed, try gtk-launch (KDE Plasma's own mechanism).
        if let Err(gio_err) = gio_result {
            tracing::warn!(
                "[open] {gio_err} — trying gtk-launch fallback \
                 (wayland={is_wayland}, path={desktop_path})"
            );

            let path_obj = std::path::Path::new(desktop_path.as_str());
            let file_name = path_obj
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let file_stem = path_obj
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            // Try file_name first (e.g. "android-studio.desktop"), then stem.
            // Both are valid XDG desktop IDs accepted by gtk-launch.
            // gtk-launch calls are synchronous subprocess waits — run on a blocking thread
            // with a 3s timeout to guard against hung .desktop launchers.
            let gtk_task = tauri::async_runtime::spawn_blocking(move || {
                [file_name, file_stem]
                    .into_iter()
                    .filter(|id| !id.is_empty())
                    .find_map(|id| {
                        let mut cmd = std::process::Command::new("gtk-launch");
                        cmd.arg(&id)
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .process_group(0);
                        // Strip Lychi's AppImage env so the launched app loads the
                        // system GTK/libs, not ours (see crate::spawn_env).
                        lychi_core::spawn_env::sanitize_command(&mut cmd);
                        match cmd.status() {
                            Ok(s) if s.success() => {
                                tracing::info!("[open] gtk-launch {id} succeeded");
                                Some(true)
                            }
                            Ok(s) => {
                                tracing::warn!("[open] gtk-launch {id} exited {:?}", s.code());
                                None
                            }
                            Err(e) => {
                                tracing::error!("[open] gtk-launch {id} spawn error: {e}");
                                None
                            }
                        }
                    })
                    .is_some()
            });
            let fallback_ok =
                match tokio::time::timeout(std::time::Duration::from_secs(3), gtk_task).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(e)) => {
                        tracing::error!("[open] gtk-launch task panicked: {e}");
                        false
                    }
                    Err(_) => {
                        tracing::error!("[open] gtk-launch timed out after 3s");
                        false
                    }
                };

            if !fallback_ok {
                dto.success = false;
                dto.output = None;
                dto.error = Some(format!(
                    "Failed to launch app: GIO: {gio_err}; gtk-launch also failed"
                ));
            }
        }
    }

    // Smart-open: focus the running window if the app was already open.
    if let Some(ref wm_class) = dto.focus_app {
        use lychi_core::action_registry::handlers::app_control;
        if let Err(e) = app_control::focus_by_class(wm_class) {
            tracing::warn!("[open] focus_by_class({wm_class}) failed: {e}");
        }
    }

    // Record history only on success — never suggest a command that failed.
    // (A terminal/app launch reports success once spawned, which is correct:
    // the launch worked even if the program later exits non-zero.) Confirmation
    // prompts returned early above, so they're recorded on the confirmed re-run.
    if dto.success {
        let _ = state.history.push(&state.db, input);
    }

    Ok(dto)
}

/// Confirm and run the action currently awaiting confirmation (G1).
///
/// Executes the EXACT resolved intent captured when the pipeline first returned
/// `needs_confirmation` — it does NOT re-resolve the raw input, so the action
/// can't shift between assessment and execution (closes the confirmation TOCTOU
/// gap). Policy is still re-checked inside `run_confirmed`, so a deny/consent
/// change since the prompt is honored. A missing or expired pending is rejected.
#[tauri::command]
#[specta::specta]
pub async fn confirm_execution(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<CommandResultDto, LychiError> {
    // Take (and clear) the pending action — a confirmation is single-use.
    let pending = state.pending_execution.write().await.take();
    let Some(pending) = pending else {
        return Err(LychiError::ExecutionFailed(
            "Nothing to confirm — the request expired or was already handled.".into(),
        ));
    };
    if pending.is_expired() {
        return Err(LychiError::ExecutionFailed(
            "Confirmation expired — please run the command again.".into(),
        ));
    }

    let inputs = {
        let config = state.config.read().await;
        lychi_core::executor::RunInputs {
            terminal: Some(config.commands.terminal.clone()),
            terminal_routing: config.commands.terminal_routing.clone(),
            inline: false,
            sink: None,
        }
    };

    // Reconstruct the human-facing input string only for history/logging; the
    // stored intent is what actually executes.
    let input_for_history = if pending.intent.args.is_empty() {
        pending.intent.action_id.clone()
    } else {
        format!("{} {}", pending.intent.action_id, pending.intent.args)
    };

    // Audit trail: record what the user reviewed vs what will run.
    tracing::info!(
        action = %pending.intent.action_id,
        assessed_risk = ?pending.risk,
        "[confirm] executing confirmed action (risk re-checked with fresh context before run)"
    );

    // Config first, released before the executor guard is taken.
    let privacy = state.config_snapshot(|c| c.privacy.clone()).await;
    let executor = state.executor.read().await;
    let exec = executor
        .run_confirmed(pending.intent.clone(), &privacy, &inputs)
        .await?;
    drop(executor);

    // Busy-reinsert (#1↔#10): if execution was rejected because an exclusive
    // action was already running, NO execution happened — so put the pending
    // confirmation back (with its ORIGINAL expiry) instead of consuming it. The
    // user can retry the confirm without reconstructing the destructive command.
    // A newer prompt that arrived meanwhile is not clobbered.
    if exec.busy {
        let mut slot = state.pending_execution.write().await;
        if slot.is_none() {
            *slot = Some(pending);
        }
    }

    finalize_exec(&app, state.inner(), &input_for_history, exec).await
}

#[tauri::command]
#[specta::specta]
pub async fn get_completions(
    input: String,
    state: State<'_, AppState>,
) -> Result<Vec<CompletionItem>, LychiError> {
    let suggestions_cfg = state.config.read().await.suggestions.clone();
    let executor = state.executor.read().await;
    let results = executor.completions(&input, &suggestions_cfg).await;
    Ok(results)
}

/// Classify a raw input string into a typed routing decision — the SINGLE source
/// of truth for "what does Enter do?". The frontend actuates the result verbatim
/// (run a command, open a panel, go to the agent/fork card, fill a correction),
/// never re-deriving command-vs-AI from its own keyword list. Local + instant
/// (reuses the pattern router, prefix index, presets, and typo suggester).
#[tauri::command]
#[specta::specta]
pub async fn classify_input(
    input: String,
    state: State<'_, AppState>,
) -> Result<lychi_core::intent::classify::RouteDecision, LychiError> {
    let executor = state.executor.read().await;
    Ok(executor.classify(&input))
}

/// A "Did you mean: X?" correction for a near-miss input (e.g. an app-name typo
/// "spoti" → "open Spotify"), or `None`. Called on Enter for a single unknown
/// word BEFORE falling to the AI, so a fat-fingered app name is corrected
/// instead of sent to the model. Local + instant (in-memory fuzzy match).
/// Returns the corrected command string (the `description` of the suggestion).
#[tauri::command]
#[specta::specta]
pub async fn suggest_correction(
    input: String,
    state: State<'_, AppState>,
) -> Result<Option<String>, LychiError> {
    let executor = state.executor.read().await;
    // `suggest` only ever corrects a MISSPELLED command/app, so the string it
    // returns is always safe to fill into the input — it never rewrites a
    // correctly-typed question into a command.
    Ok(
        lychi_core::intent::typo_suggest::suggest(&input, &executor.registry)
            .and_then(|item| item.description),
    )
}

/// Dynamic command catalog for the Guide — generated from the live action
/// registry, so it never goes stale as handlers are added/removed.
#[tauri::command]
#[specta::specta]
pub async fn get_command_catalog(
    state: State<'_, AppState>,
) -> Result<Vec<CommandInfo>, LychiError> {
    let executor = state.executor.read().await;
    Ok(executor.registry.command_catalog())
}

/// Dynamic trigger list for the Guide — structural sigils + shorthand
/// colon-triggers, the latter described by their live handler (centralised with
/// the command catalog so they never drift).
#[tauri::command]
#[specta::specta]
pub async fn get_trigger_catalog(
    state: State<'_, AppState>,
) -> Result<Vec<CommandInfo>, LychiError> {
    let executor = state.executor.read().await;
    Ok(executor.registry.trigger_catalog())
}

/// Resolve a row action into the command it stands for, then run it.
///
/// Row actions are declarative — a row carries `{id, label, target}`, never a
/// command string. This is where that pays off: resolution happens **here**,
/// against the handler that produced the row, so the frontend can only ask for
/// verbs a handler declared and targets it can validate. Shipping commands on
/// the action instead would make the frontend a command source, and the rules
/// engine would then see something indistinguishable from typed input.
///
/// The resolved string goes through `execute_command` like anything the user
/// types, so risk assessment, confirmation and the denylist all still apply —
/// one execution path, not a privileged side door.
#[tauri::command]
#[specta::specta]
pub async fn run_row_action(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    handler: String,
    id: String,
    target: String,
) -> Result<CommandResultDto, LychiError> {
    let command = match handler.as_str() {
        "services" => lychi_core::action_registry::handlers::services::resolve_action(&id, &target),
        "packages" => lychi_core::action_registry::handlers::packages::resolve_action(&id, &target),
        "ssh" => lychi_core::action_registry::handlers::ssh::resolve_action(&id, &target),
        "snippets" => lychi_core::action_registry::handlers::snippets::resolve_action(&id, &target),
        "notes" => lychi_core::action_registry::handlers::notes::resolve_note_action(&id, &target),
        "todos" => lychi_core::action_registry::handlers::notes::resolve_todo_action(&id, &target),
        // Unknown producers are refused rather than guessed at: a handler that
        // has not opted in cannot have its rows actioned.
        other => Err(format!("Handler '{other}' does not support row actions")),
    }
    .map_err(LychiError::ExecutionFailed)?;

    // `target`/`command` can carry row content (note text, snippet bodies) —
    // metadata at info, content at debug (the file log ships with bug reports).
    tracing::info!("[row-action] {handler}/{id}");
    tracing::debug!("[row-action] target={target} → {command}");
    execute_command(command, None, None, None, app, state).await
}
