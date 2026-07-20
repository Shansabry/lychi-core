use serde::Serialize;
use tauri::State;

use lychi_core::error::LychiError;
use lychi_core::notes::store::NotesStore;
use lychi_core::notes::{NoteItem, TodoItem};

use crate::state::AppState;

#[derive(Serialize, specta::Type)]
pub struct AllNotes {
    pub notes: Vec<NoteItem>,
    pub todos: Vec<TodoItem>,
}

#[tauri::command]
#[specta::specta]
pub async fn get_all_notes(state: State<'_, AppState>) -> Result<AllNotes, LychiError> {
    let store = NotesStore::new();
    Ok(AllNotes {
        notes: store.get_notes(&state.db)?,
        todos: store.get_todos(&state.db)?,
    })
}

// ---- Notes ----

#[tauri::command]
#[specta::specta]
pub async fn get_notes(state: State<'_, AppState>) -> Result<Vec<NoteItem>, LychiError> {
    let store = NotesStore::new();
    store.get_notes(&state.db)
}

#[tauri::command]
#[specta::specta]
pub async fn add_note(text: String, state: State<'_, AppState>) -> Result<NoteItem, LychiError> {
    let store = NotesStore::new();
    store.add_note(&state.db, &text)
}

#[tauri::command]
#[specta::specta]
pub async fn update_note(
    id: String,
    text: String,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    let store = NotesStore::new();
    store.update_note(&state.db, &id, &text)
}

#[tauri::command]
#[specta::specta]
pub async fn delete_note(id: String, state: State<'_, AppState>) -> Result<(), LychiError> {
    let store = NotesStore::new();
    store.delete_note(&state.db, &id)
}

// ---- Todos ----

#[tauri::command]
#[specta::specta]
pub async fn get_todos(state: State<'_, AppState>) -> Result<Vec<TodoItem>, LychiError> {
    let store = NotesStore::new();
    store.get_todos(&state.db)
}

#[tauri::command]
#[specta::specta]
pub async fn add_todo(text: String, state: State<'_, AppState>) -> Result<TodoItem, LychiError> {
    let store = NotesStore::new();
    store.add_todo(&state.db, &text)
}

#[tauri::command]
#[specta::specta]
pub async fn toggle_todo(id: String, state: State<'_, AppState>) -> Result<(), LychiError> {
    let store = NotesStore::new();
    store.toggle_todo(&state.db, &id)
}

#[tauri::command]
#[specta::specta]
pub async fn delete_todo(id: String, state: State<'_, AppState>) -> Result<(), LychiError> {
    let store = NotesStore::new();
    store.delete_todo(&state.db, &id)
}
