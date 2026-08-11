use tauri::State;

use lychi_core::error::LychiError;
use lychi_core::pins::PinItem;
use lychi_core::pins::store::PinsStore;

use crate::state::AppState;

#[tauri::command]
#[specta::specta]
pub async fn get_pins(state: State<'_, AppState>) -> Result<Vec<PinItem>, LychiError> {
    PinsStore::new().list(&state.db)
}

#[tauri::command]
#[specta::specta]
pub async fn add_pin(
    run: String,
    label: String,
    state: State<'_, AppState>,
) -> Result<PinItem, LychiError> {
    PinsStore::new().add(&state.db, &run, &label)
}

#[tauri::command]
#[specta::specta]
pub async fn remove_pin(run: String, state: State<'_, AppState>) -> Result<(), LychiError> {
    PinsStore::new().remove(&state.db, &run)
}
