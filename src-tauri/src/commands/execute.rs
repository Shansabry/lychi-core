use tauri::{AppHandle, Emitter, State};

use lychi_core::action_registry::{ActionResult, CompletionItem};
use lychi_core::error::LychiError;

use crate::state::AppState;

#[tauri::command]
pub async fn execute_command(
    input: String,
    confirmed: Option<bool>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ActionResult, LychiError> {
    // Record in history
    {
        let mut history = state.history.write().await;
        history.push(&input);
    }

    // Run through executor pipeline: resolve → validate → execute
    let executor = state.executor.read().await;
    let privacy = state.config.read().await.privacy.clone();
    let result = executor
        .run(&input, confirmed.unwrap_or(false), &privacy)
        .await?;

    // Notify frontend when notes/todos are mutated by a handler
    if result.success && is_notes_mutation(&input) {
        let _ = app.emit("lychi://notes-changed", ());
    }

    Ok(result)
}

#[tauri::command]
pub async fn get_completions(
    input: String,
    state: State<'_, AppState>,
) -> Result<Vec<CompletionItem>, LychiError> {
    let executor = state.executor.read().await;
    Ok(executor.completions(&input).await)
}

/// Check if the input is a notes/todo write operation (not a read).
fn is_notes_mutation(input: &str) -> bool {
    let lower = input.trim().to_lowercase();
    let is_note_write = lower.starts_with("note ") && !lower.starts_with("note read");
    let is_todo_write = lower.starts_with("todo ")
        && !lower.starts_with("todo list")
        && !lower.starts_with("todo ls")
        && !lower.starts_with("todo summary");
    is_note_write || is_todo_write
}
