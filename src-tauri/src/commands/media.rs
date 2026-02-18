use crate::state::AppState;
use lychi_core::error::LychiError;
use lychi_core::mpris::{MprisManager, TrackInfo};
use tauri::State;

/// Get playback status from all MPRIS media players.
/// Also refreshes the player list to discover newly started/stopped players.
#[tauri::command]
pub async fn media_get_status(state: State<'_, AppState>) -> Result<Vec<TrackInfo>, LychiError> {
    // Try cached manager — refresh to pick up new players
    {
        let mut guard = state.mpris.write().await;
        if let Some(manager) = guard.as_mut() {
            let _ = manager.refresh().await;
            return Ok(manager.get_all_status().await);
        }
    }

    // Not connected yet — try to connect
    let manager = MprisManager::connect().await?;
    let status = manager.get_all_status().await;
    let mut guard = state.mpris.write().await;
    *guard = Some(manager);
    Ok(status)
}

/// Send a transport control action to a specific player.
#[tauri::command]
pub async fn media_control(
    bus_name: String,
    action: String,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    {
        let guard = state.mpris.read().await;
        if let Some(manager) = guard.as_ref() {
            return manager.control(&bus_name, &action).await;
        }
    }

    let manager = MprisManager::connect().await?;
    let result = manager.control(&bus_name, &action).await;
    let mut guard = state.mpris.write().await;
    *guard = Some(manager);
    result
}

/// Seek to an absolute position in the current track of a specific player.
#[tauri::command]
pub async fn media_seek(
    bus_name: String,
    track_id: String,
    position_us: i64,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    {
        let guard = state.mpris.read().await;
        if let Some(manager) = guard.as_ref() {
            return manager.seek(&bus_name, &track_id, position_us).await;
        }
    }

    let manager = MprisManager::connect().await?;
    let result = manager.seek(&bus_name, &track_id, position_us).await;
    let mut guard = state.mpris.write().await;
    *guard = Some(manager);
    result
}

/// Send a control action to all connected players (e.g. pause all).
#[tauri::command]
pub async fn media_control_all(
    action: String,
    state: State<'_, AppState>,
) -> Result<usize, LychiError> {
    {
        let guard = state.mpris.read().await;
        if let Some(manager) = guard.as_ref() {
            return manager.control_all(&action).await;
        }
    }

    let manager = MprisManager::connect().await?;
    let result = manager.control_all(&action).await;
    let mut guard = state.mpris.write().await;
    *guard = Some(manager);
    result
}

/// Refresh the player list (discovers new/removed players).
#[tauri::command]
pub async fn media_refresh(state: State<'_, AppState>) -> Result<(), LychiError> {
    let mut guard = state.mpris.write().await;
    if let Some(manager) = guard.as_mut() {
        manager.refresh().await?;
    } else {
        let manager = MprisManager::connect().await?;
        *guard = Some(manager);
    }
    Ok(())
}
