use tauri::State;

use lychi_core::aliases::AliasItem;
use lychi_core::aliases::store::AliasesStore;
use lychi_core::error::LychiError;

use crate::state::AppState;

#[tauri::command]
#[specta::specta]
pub async fn get_aliases(state: State<'_, AppState>) -> Result<Vec<AliasItem>, LychiError> {
    let store = AliasesStore::new();
    store.get_aliases(&state.db)
}

#[tauri::command]
#[specta::specta]
pub async fn add_alias(
    name: String,
    command: String,
    state: State<'_, AppState>,
) -> Result<AliasItem, LychiError> {
    let store = AliasesStore::new();
    store.add_alias(&state.db, &name, &command)
}

#[tauri::command]
#[specta::specta]
pub async fn update_alias(
    name: String,
    command: String,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    let store = AliasesStore::new();
    store.update_alias(&state.db, &name, &command)
}

#[tauri::command]
#[specta::specta]
pub async fn delete_alias(name: String, state: State<'_, AppState>) -> Result<(), LychiError> {
    let store = AliasesStore::new();
    store.delete_alias(&state.db, &name)
}
