use tauri::State;

use lychi_core::error::LychiError;
use lychi_core::notes::{NoteItem, TodoItem};

use crate::state::AppState;

// ---- Notes ----

#[tauri::command]
pub async fn get_notes(state: State<'_, AppState>) -> Result<Vec<NoteItem>, LychiError> {
    let notes = state.notes.read().await;
    Ok(notes.get_notes().to_vec())
}

#[tauri::command]
pub async fn add_note(text: String, state: State<'_, AppState>) -> Result<NoteItem, LychiError> {
    let mut notes = state.notes.write().await;
    notes.add_note(&text)
}

#[tauri::command]
pub async fn update_note(
    id: String,
    text: String,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    let mut notes = state.notes.write().await;
    notes.update_note(&id, &text)
}

#[tauri::command]
pub async fn delete_note(id: String, state: State<'_, AppState>) -> Result<(), LychiError> {
    let mut notes = state.notes.write().await;
    notes.delete_note(&id)
}

// ---- Todos ----

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
