use tauri::State;

use lychi_core::command::{CommandResult, CompletionItem};
use lychi_core::error::LychiError;

use crate::state::AppState;

#[tauri::command]
pub async fn execute_command(
    input: String,
    state: State<'_, AppState>,
) -> Result<CommandResult, LychiError> {
    // Record in history
    {
        let mut history = state.history.write().await;
        history.push(&input);
    }

    // Route and dispatch
    let registry = state.registry.read().await;
    registry.execute_routed(&input).await
}

#[tauri::command]
pub async fn get_completions(
    input: String,
    state: State<'_, AppState>,
) -> Result<Vec<CompletionItem>, LychiError> {
    let registry = state.registry.read().await;
    Ok(registry.completions_routed(&input).await)
}
