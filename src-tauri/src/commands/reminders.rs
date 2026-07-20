use tauri::State;

use lychi_core::error::LychiError;
use lychi_core::reminders::ReminderItem;
use lychi_core::reminders::store::RemindersStore;

use crate::state::AppState;

#[tauri::command]
#[specta::specta]
pub async fn get_reminders(state: State<'_, AppState>) -> Result<Vec<ReminderItem>, LychiError> {
    let store = RemindersStore::new();
    store.list_reminders(&state.db)
}

#[tauri::command]
#[specta::specta]
pub async fn add_reminder(
    text: String,
    due_at: u64,
    state: State<'_, AppState>,
) -> Result<ReminderItem, LychiError> {
    let store = RemindersStore::new();
    store.add_reminder(&state.db, &text, due_at)
}

#[tauri::command]
#[specta::specta]
pub async fn delete_reminder(id: String, state: State<'_, AppState>) -> Result<(), LychiError> {
    let store = RemindersStore::new();
    store.delete_reminder(&state.db, &id)
}
