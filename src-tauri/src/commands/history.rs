use tauri::State;

use lychi_core::error::LychiError;

use crate::state::AppState;

#[tauri::command]
#[specta::specta]
pub async fn get_history(state: State<'_, AppState>) -> Result<Vec<String>, LychiError> {
    state.history.entries(&state.db)
}

#[tauri::command]
#[specta::specta]
pub async fn clear_history(state: State<'_, AppState>) -> Result<(), LychiError> {
    state.history.clear(&state.db)
}
