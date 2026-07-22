use tauri::State;

use lychi_core::context::EnvironmentContext;
use lychi_core::error::LychiError;

use crate::state::AppState;

#[tauri::command]
#[specta::specta]
pub async fn get_context(
    state: State<'_, AppState>,
) -> Result<Option<EnvironmentContext>, LychiError> {
    let executor = state.executor.read().await;
    Ok(executor.context.clone())
}

/// Read the PRIMARY selection — the text the user has highlighted in the focused
/// window (without copying it). Returns `None` if nothing is selected. Used to
/// auto-fill AI commands: `summarize` with no typed text acts on the selection.
/// Runs `wl-paste`/`xclip` in a blocking task (process spawn).
#[tauri::command]
#[specta::specta]
pub async fn read_selection() -> Result<Option<String>, LychiError> {
    let is_wayland = lychi_core::context::is_wayland();
    tokio::task::spawn_blocking(move || {
        lychi_core::clipboard::selection::read_primary_selection(is_wayland)
    })
    .await
    .map_err(|e| LychiError::ExecutionFailed(format!("selection read task failed: {e}")))
}
