use tauri::State;

use lychi_core::error::LychiError;

use crate::state::AppState;

#[tauri::command]
pub async fn get_history(state: State<'_, AppState>) -> Result<Vec<String>, LychiError> {
    let history = state.history.read().await;
    Ok(history.entries().to_vec())
}

#[tauri::command]
pub async fn clear_history(state: State<'_, AppState>) -> Result<(), LychiError> {
    let mut history = state.history.write().await;
    history.clear();
    Ok(())
}
