use std::os::unix::process::CommandExt;
use std::process::Stdio;

use tauri::{AppHandle, Emitter, Manager, State};

use lychi_core::action_registry::{ActionResult, CompletionItem};
use lychi_core::db::frecency;
use lychi_core::error::LychiError;

use crate::state::AppState;

/// Action IDs that mutate panel data (notes, todos, reminders).
const PANEL_MUTATION_ACTIONS: &[&str] = &["note", "todo", "reminder"];

#[tauri::command]
pub async fn execute_command(
    input: String,
    confirmed: Option<bool>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ActionResult, LychiError> {
    // Record in history + frecency for fuzzy search ranking
    state.history.push(&state.db, &input)?;
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
        drop(executor_r);
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

    // Set terminal routing mode from config (read before executor lock)
    {
        let config = state.config.read().await;
        lychi_core::action_registry::handlers::shell_exec::set_terminal_routing(Some(
            config.commands.terminal_routing.clone(),
        ));
    }

    // Run through executor pipeline: resolve → validate → execute
    let executor = state.executor.read().await;
    let privacy = state.config.read().await.privacy.clone();
    let mut exec = executor
        .run(&input, confirmed.unwrap_or(false), &privacy)
        .await?;

    // Notify frontend when notes/todos/reminders are mutated by a handler
    if exec.result.success && PANEL_MUTATION_ACTIONS.contains(&exec.action_id.as_str()) {
        let _ = app.emit("lychi://notes-changed", ());
    }

    // Launch desktop app. Primary path: GIO DesktopAppInfo + GDK AppLaunchContext,
    // which handles D-Bus activation, Terminal=true, and Wayland activation tokens.
    // Fallback: gtk-launch, which is what KDE Plasma uses and handles edge cases
    // that GIO misses on KDE/Wayland (session env, DBus activation for JetBrains, etc.).
    if let Some(ref desktop_path) = exec.result.launch_desktop {
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
                        match std::process::Command::new("gtk-launch")
                            .arg(&id)
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .process_group(0)
                            .status()
                        {
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
                exec.result.success = false;
                exec.result.output = None;
                exec.result.error = Some(format!(
                    "Failed to launch app: GIO: {gio_err}; gtk-launch also failed"
                ));
            }
        }
    }

    // Smart-open: focus the running window if the app was already open.
    if let Some(ref wm_class) = exec.result.focus_app {
        use lychi_core::action_registry::handlers::app_control;
        if let Err(e) = app_control::focus_by_class(wm_class) {
            tracing::warn!("[open] focus_by_class({wm_class}) failed: {e}");
        }
    }

    Ok(exec.result)
}

#[tauri::command]
pub async fn get_completions(
    input: String,
    state: State<'_, AppState>,
) -> Result<Vec<CompletionItem>, LychiError> {
    let executor = state.executor.read().await;
    let results = executor.completions(&input).await;
    Ok(results)
}
