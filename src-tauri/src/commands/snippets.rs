use tauri::State;

use lychi_core::error::LychiError;
use lychi_core::snippets::SnippetItem;
use lychi_core::snippets::store::SnippetsStore;

use crate::state::AppState;

#[tauri::command]
pub async fn get_snippets(state: State<'_, AppState>) -> Result<Vec<SnippetItem>, LychiError> {
    let store = SnippetsStore::new();
    store.get_snippets(&state.db)
}

#[tauri::command]
pub async fn add_snippet(
    name: String,
    body: String,
    state: State<'_, AppState>,
) -> Result<SnippetItem, LychiError> {
    let store = SnippetsStore::new();
    store.add_snippet(&state.db, &name, &body)
}

#[tauri::command]
pub async fn update_snippet(
    id: String,
    name: String,
    body: String,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    let store = SnippetsStore::new();
    store.update_snippet(&state.db, &id, &name, &body)
}

#[tauri::command]
pub async fn delete_snippet(id: String, state: State<'_, AppState>) -> Result<(), LychiError> {
    let store = SnippetsStore::new();
    store.delete_snippet(&state.db, &id)
}
