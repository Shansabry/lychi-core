use tauri::{AppHandle, Emitter, State};

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

    // Run through executor pipeline: resolve → validate → execute
    let executor = state.executor.read().await;
    let privacy = state.config.read().await.privacy.clone();
    let exec = executor
        .run(&input, confirmed.unwrap_or(false), &privacy)
        .await?;

    // Notify frontend when notes/todos/reminders are mutated by a handler
    if exec.result.success && PANEL_MUTATION_ACTIONS.contains(&exec.action_id.as_str()) {
        let _ = app.emit("lychi://notes-changed", ());
    }

    // Launch desktop app via GIO DesktopAppInfo with GDK AppLaunchContext.
    // This properly handles working directories, D-Bus activation, quoted Exec
    // args, Terminal=true, and Wayland activation tokens.
    if let Some(ref desktop_path) = exec.result.launch_desktop {
        let path = desktop_path.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        glib::MainContext::default().invoke(move || {
            use gio::prelude::*;
            let result = (|| {
                let app_info = gio::DesktopAppInfo::from_filename(&path)
                    .ok_or_else(|| format!("Failed to load desktop file: {path}"))?;
                let context: Option<gio::AppLaunchContext> = gdk::Display::default()
                    .and_then(|d| d.app_launch_context())
                    .map(|c| c.into());
                app_info
                    .launch(&[], context.as_ref())
                    .map_err(|e| format!("launch() failed: {e}"))
            })();
            if let Err(e) = result {
                tracing::error!("Failed to launch desktop app: {e}");
            }
            let _ = tx.send(());
        });
        // Wait for GLib to complete the launch before returning
        let _ = rx.await;
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
