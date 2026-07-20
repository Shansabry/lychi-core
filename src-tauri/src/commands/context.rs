use tauri::State;

use lychi_core::context::EnvironmentContext;
use lychi_core::error::LychiError;

use crate::state::AppState;

#[tauri::command]
#[specta::specta]
pub async fn get_context(
    state: State<'_, AppState>,
) -> Result<Option<EnvironmentContext>, LychiError> {
    let executor = state.executor.read().await;
    Ok(executor.context.clone())
}
