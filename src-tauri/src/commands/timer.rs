use lychi_core::action_registry::handlers::timer::{TimerStatus, get_all_timers};
use tauri::State;

use crate::state::AppState;

#[tauri::command]
pub async fn get_timers(state: State<'_, AppState>) -> Result<Vec<TimerStatus>, String> {
    Ok(get_all_timers(&state.timer_state))
}
