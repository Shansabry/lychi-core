#[cfg(feature = "mpris")]
mod inner {
    use crate::state::AppState;
    use lychi_core::error::LychiError;
    use lychi_core::mpris::{MprisManager, TrackInfo};
    use tauri::State;

    /// Get playback status from all MPRIS media players.
    /// Also refreshes the player list to discover newly started/stopped players.
    #[tauri::command]
    #[specta::specta]
    pub async fn media_get_status(
        state: State<'_, AppState>,
    ) -> Result<Vec<TrackInfo>, LychiError> {
        // Brief write lock for refresh only — release before reading status
        {
            let mut guard = state.mpris.write().await;
            if let Some(manager) = guard.as_mut() {
                let _ = manager.refresh().await;
            } else {
                *guard = Some(MprisManager::connect().await?);
            }
        }

        // Read lock for status — doesn't block completions
        let mut tracks = {
            let guard = state.mpris.read().await;
            guard.as_ref().unwrap().get_all_status().await
        };
        // Resolve each track's album art to an inline `data:` URI in-process, so
        // the WebView never fetches a remote (Spotify) image — see media_art.
        for track in &mut tracks {
            crate::commands::media_art::resolve_track_art(track).await;
        }
        Ok(tracks)
    }

    /// Send a transport control action to a specific player.
    #[tauri::command]
    #[specta::specta]
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
    #[specta::specta]
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

    /// Relative seek (±seconds) within the current track — the ±10s buttons.
    #[tauri::command]
    #[specta::specta]
    pub async fn media_seek_relative(
        bus_name: String,
        offset_us: i64,
        state: State<'_, AppState>,
    ) -> Result<(), LychiError> {
        {
            let guard = state.mpris.read().await;
            if let Some(manager) = guard.as_ref() {
                return manager.seek_relative(&bus_name, offset_us).await;
            }
        }
        let manager = MprisManager::connect().await?;
        let result = manager.seek_relative(&bus_name, offset_us).await;
        *state.mpris.write().await = Some(manager);
        result
    }

    /// Toggle shuffle on a specific player.
    #[tauri::command]
    #[specta::specta]
    pub async fn media_set_shuffle(
        bus_name: String,
        on: bool,
        state: State<'_, AppState>,
    ) -> Result<(), LychiError> {
        {
            let guard = state.mpris.read().await;
            if let Some(manager) = guard.as_ref() {
                return manager.set_shuffle(&bus_name, on).await;
            }
        }
        let manager = MprisManager::connect().await?;
        let result = manager.set_shuffle(&bus_name, on).await;
        *state.mpris.write().await = Some(manager);
        result
    }

    /// Set loop mode ("None"|"Track"|"Playlist") on a specific player.
    #[tauri::command]
    #[specta::specta]
    pub async fn media_set_loop(
        bus_name: String,
        mode: String,
        state: State<'_, AppState>,
    ) -> Result<(), LychiError> {
        {
            let guard = state.mpris.read().await;
            if let Some(manager) = guard.as_ref() {
                return manager.set_loop(&bus_name, &mode).await;
            }
        }
        let manager = MprisManager::connect().await?;
        let result = manager.set_loop(&bus_name, &mode).await;
        *state.mpris.write().await = Some(manager);
        result
    }

    /// Set volume (0.0–1.0) on a specific player.
    #[tauri::command]
    #[specta::specta]
    pub async fn media_set_volume(
        bus_name: String,
        volume: f64,
        state: State<'_, AppState>,
    ) -> Result<(), LychiError> {
        {
            let guard = state.mpris.read().await;
            if let Some(manager) = guard.as_ref() {
                return manager.set_volume(&bus_name, volume).await;
            }
        }
        let manager = MprisManager::connect().await?;
        let result = manager.set_volume(&bus_name, volume).await;
        *state.mpris.write().await = Some(manager);
        result
    }

    /// Send a control action to all connected players (e.g. pause all).
    #[tauri::command]
    #[specta::specta]
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
    #[specta::specta]
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
}

#[cfg(not(feature = "mpris"))]
mod inner {
    use lychi_core::error::LychiError;

    #[tauri::command]
    #[specta::specta]
    pub async fn media_get_status() -> Result<Vec<()>, LychiError> {
        Err(LychiError::ExecutionFailed(
            "Media control not available (mpris feature disabled)".into(),
        ))
    }

    #[tauri::command]
    #[specta::specta]
    pub async fn media_control(_bus_name: String, _action: String) -> Result<(), LychiError> {
        Err(LychiError::ExecutionFailed(
            "Media control not available (mpris feature disabled)".into(),
        ))
    }

    #[tauri::command]
    #[specta::specta]
    pub async fn media_seek(
        _bus_name: String,
        _track_id: String,
        _position_us: i64,
    ) -> Result<(), LychiError> {
        Err(LychiError::ExecutionFailed(
            "Media control not available (mpris feature disabled)".into(),
        ))
    }

    #[tauri::command]
    #[specta::specta]
    pub async fn media_control_all(_action: String) -> Result<usize, LychiError> {
        Err(LychiError::ExecutionFailed(
            "Media control not available (mpris feature disabled)".into(),
        ))
    }

    #[tauri::command]
    #[specta::specta]
    pub async fn media_refresh() -> Result<(), LychiError> {
        Err(LychiError::ExecutionFailed(
            "Media control not available (mpris feature disabled)".into(),
        ))
    }
}

pub use inner::*;
