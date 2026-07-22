//! AI preset CRUD commands (Phase 3 "AI Commands"). Thin wrappers over the core
//! `AiPresetsStore` — a user creates a preset (keyword + instruction template),
//! and typing `<keyword> <text>` seeds an AI conversation with the rendered
//! template. The built-in translate/summarize/rewrite are seeded defaults using
//! the exact same mechanism; users can add, edit, or delete any of them.

use tauri::State;

use lychi_core::ai_presets::AiPresetItem;
use lychi_core::ai_presets::store::AiPresetsStore;
use lychi_core::error::LychiError;

use crate::state::AppState;

#[tauri::command]
#[specta::specta]
pub async fn get_ai_presets(state: State<'_, AppState>) -> Result<Vec<AiPresetItem>, LychiError> {
    AiPresetsStore::new().get_presets(&state.db)
}

#[tauri::command]
#[specta::specta]
pub async fn add_ai_preset(
    keyword: String,
    name: String,
    template: String,
    state: State<'_, AppState>,
) -> Result<AiPresetItem, LychiError> {
    AiPresetsStore::new().add_preset(&state.db, &keyword, &name, &template)
}

#[tauri::command]
#[specta::specta]
pub async fn update_ai_preset(
    id: String,
    keyword: String,
    name: String,
    template: String,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    AiPresetsStore::new().update_preset(&state.db, &id, &keyword, &name, &template)
}

#[tauri::command]
#[specta::specta]
pub async fn delete_ai_preset(id: String, state: State<'_, AppState>) -> Result<(), LychiError> {
    AiPresetsStore::new().delete_preset(&state.db, &id)
}
