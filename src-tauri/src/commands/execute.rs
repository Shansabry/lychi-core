use tauri::State;

use lychi_core::action_registry::{ActionResult, CompletionItem};
use lychi_core::error::LychiError;

use crate::state::AppState;

#[tauri::command]
pub async fn execute_command(
    input: String,
    confirmed: Option<bool>,
    state: State<'_, AppState>,
) -> Result<ActionResult, LychiError> {
    // Record in history
    {
        let mut history = state.history.write().await;
        history.push(&input);
    }

    // Run through executor pipeline: resolve → validate → execute
    let executor = state.executor.read().await;
    executor.run(&input, confirmed.unwrap_or(false)).await
}

#[tauri::command]
pub async fn get_completions(
    input: String,
    state: State<'_, AppState>,
) -> Result<Vec<CompletionItem>, LychiError> {
    let executor = state.executor.read().await;
    Ok(executor.completions(&input).await)
}
