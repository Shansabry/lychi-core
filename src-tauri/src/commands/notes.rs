use tauri::State;

use lychi_core::error::LychiError;
use lychi_core::notes::TodoItem;

use crate::state::AppState;

#[tauri::command]
pub async fn get_note(state: State<'_, AppState>) -> Result<String, LychiError> {
    let notes = state.notes.read().await;
    Ok(notes.get_note().to_string())
}

#[tauri::command]
pub async fn set_note(text: String, state: State<'_, AppState>) -> Result<(), LychiError> {
    let mut notes = state.notes.write().await;
    notes.set_note(&text)
}

#[tauri::command]
pub async fn get_todos(state: State<'_, AppState>) -> Result<Vec<TodoItem>, LychiError> {
    let notes = state.notes.read().await;
    Ok(notes.get_todos().to_vec())
}

#[tauri::command]
pub async fn add_todo(text: String, state: State<'_, AppState>) -> Result<TodoItem, LychiError> {
    let mut notes = state.notes.write().await;
    notes.add_todo(&text)
}

#[tauri::command]
pub async fn toggle_todo(id: String, state: State<'_, AppState>) -> Result<(), LychiError> {
    let mut notes = state.notes.write().await;
    notes.toggle_todo(&id)
}

#[tauri::command]
pub async fn delete_todo(id: String, state: State<'_, AppState>) -> Result<(), LychiError> {
    let mut notes = state.notes.write().await;
    notes.delete_todo(&id)
}
