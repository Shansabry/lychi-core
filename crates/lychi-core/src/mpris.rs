use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use zbus::Connection;
use zbus::zvariant::OwnedValue;

use crate::error::LychiError;

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

/// Current playback state from a media player via MPRIS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: Option<String>,
    /// MPRIS track object path (needed for SetPosition).
    pub track_id: String,
    /// Track length in microseconds.
    pub length_us: i64,
    /// Current playback position in microseconds.
    pub position_us: i64,
    pub status: PlaybackStatus,
    /// D-Bus bus name (e.g. "org.mpris.MediaPlayer2.spotify").
    pub bus_name: String,
    /// Friendly player name (e.g. "Spotify", "Firefox").
    pub player_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

/// Async MPRIS client for controlling a single media player over D-Bus.
pub struct MprisClient {
    conn: Arc<Connection>,
    bus_name: String,
}

impl MprisClient {
    /// Connect to a specific MPRIS player by bus name.
    pub async fn connect(bus_name: &str) -> Result<Self, LychiError> {
        let conn = Connection::session()
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("D-Bus session error: {e}")))?;

        Ok(Self {
            conn: Arc::new(conn),
            bus_name: bus_name.to_string(),
        })
    }

    /// Create a client from an existing D-Bus connection.
    pub fn from_connection(conn: Arc<Connection>, bus_name: String) -> Self {
        Self { conn, bus_name }
    }

    /// Clone the inner connection for use in background tasks.
    pub fn clone_inner(&self) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
            bus_name: self.bus_name.clone(),
        }
    }

    pub fn bus_name(&self) -> &str {
        &self.bus_name
    }

    /// Extract friendly player name from bus name.
    pub fn player_name(&self) -> String {
        friendly_name(&self.bus_name)
    }

    async fn player_proxy(&self) -> Result<zbus::Proxy<'_>, LychiError> {
        zbus::Proxy::new(&self.conn, &*self.bus_name, OBJECT_PATH, PLAYER_IFACE)
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("MPRIS proxy error: {e}")))
    }

    /// Read the current playback state.
    pub async fn get_status(&self) -> Result<TrackInfo, LychiError> {
        let proxy = self.player_proxy().await?;

        let status_str: String = proxy
            .get_property("PlaybackStatus")
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("PlaybackStatus: {e}")))?;

        let metadata: HashMap<String, OwnedValue> = proxy
            .get_property("Metadata")
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("Metadata: {e}")))?;

        let position: i64 = proxy.get_property("Position").await.unwrap_or(0);

        let title = extract_string(&metadata, "xesam:title").unwrap_or_default();
        let artist = extract_artists(&metadata).unwrap_or_default();
        let album = extract_string(&metadata, "xesam:album").unwrap_or_default();
        let art_url = extract_string(&metadata, "mpris:artUrl");
        let track_id = extract_object_path(&metadata, "mpris:trackid")
            .or_else(|| extract_string(&metadata, "mpris:trackid"))
            .unwrap_or_default();
        let length_us = extract_i64(&metadata, "mpris:length")
            .or_else(|| extract_u64(&metadata, "mpris:length").map(|v| v as i64))
            .unwrap_or(0);

        tracing::debug!(
            "[mpris] {} track_id={track_id:?} length_us={length_us} position={position}",
            self.bus_name
        );

        let status = match status_str.as_str() {
            "Playing" => PlaybackStatus::Playing,
            "Paused" => PlaybackStatus::Paused,
            _ => PlaybackStatus::Stopped,
        };

        Ok(TrackInfo {
            title,
            artist,
            album,
            art_url,
            track_id,
            length_us,
            position_us: position,
            status,
            bus_name: self.bus_name.clone(),
            player_name: self.player_name(),
        })
    }

    /// Send a transport control command.
    pub async fn control(&self, action: &str) -> Result<(), LychiError> {
        let proxy = self.player_proxy().await?;

        let method = match action {
            "play_pause" => "PlayPause",
            "play" => "Play",
            "pause" => "Pause",
            "next" => "Next",
            "prev" => "Previous",
            other => {
                return Err(LychiError::ExecutionFailed(format!(
                    "Unknown media action: {other}"
                )));
            }
        };

        proxy
            .call_method(method, &())
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("MPRIS {method}: {e}")))?;

        Ok(())
    }

    /// Seek to an absolute position in the current track.
    pub async fn set_position(&self, track_id: &str, position_us: i64) -> Result<(), LychiError> {
        tracing::debug!("[mpris] SetPosition track_id={track_id:?} position_us={position_us}");
        let proxy = self.player_proxy().await?;
        let object_path = zbus::zvariant::ObjectPath::try_from(track_id).map_err(|e| {
            tracing::error!("[mpris] Invalid track ID {track_id:?}: {e}");
            LychiError::ExecutionFailed(format!("Invalid track ID: {e}"))
        })?;
        proxy
            .call_method("SetPosition", &(object_path, position_us))
            .await
            .map_err(|e| {
                tracing::error!("[mpris] SetPosition failed: {e}");
                LychiError::ExecutionFailed(format!("MPRIS SetPosition: {e}"))
            })?;
        tracing::debug!("[mpris] SetPosition succeeded");
        Ok(())
    }

    /// Subscribe to PropertiesChanged signals.
    /// Yields a new `TrackInfo` each time the player's state changes.
    /// Returns an owned stream (no borrow on self).
    pub async fn subscribe_changes(
        &self,
    ) -> Result<std::pin::Pin<Box<dyn futures_core::Stream<Item = TrackInfo> + Send>>, LychiError>
    {
        use futures_util::StreamExt;

        let props_proxy = zbus::fdo::PropertiesProxy::builder(&self.conn)
            .destination(&*self.bus_name)
            .map_err(|e| LychiError::ExecutionFailed(e.to_string()))?
            .path(OBJECT_PATH)
            .map_err(|e| LychiError::ExecutionFailed(e.to_string()))?
            .build()
            .await
            .map_err(|e| LychiError::ExecutionFailed(e.to_string()))?;

        let stream = props_proxy
            .receive_properties_changed()
            .await
            .map_err(|e| {
                LychiError::ExecutionFailed(format!("PropertiesChanged subscribe: {e}"))
            })?;

        let client = self.clone_inner();
        Ok(Box::pin(stream.filter_map(move |_signal| {
            let client = client.clone_inner();
            async move { client.get_status().await.ok() }
        })))
    }
}

/// Manages multiple MPRIS media players, discovering them on the D-Bus.
pub struct MprisManager {
    conn: Arc<Connection>,
    players: HashMap<String, MprisClient>,
}

impl MprisManager {
    /// Connect to the session D-Bus and discover all MPRIS players.
    pub async fn connect() -> Result<Self, LychiError> {
        let conn = Connection::session()
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("D-Bus session error: {e}")))?;

        let conn = Arc::new(conn);
        let mut manager = Self {
            conn,
            players: HashMap::new(),
        };
        manager.refresh().await?;
        Ok(manager)
    }

    /// Re-discover MPRIS players on the bus. Adds new players, removes gone ones.
    pub async fn refresh(&mut self) -> Result<(), LychiError> {
        let dbus = zbus::fdo::DBusProxy::new(&self.conn)
            .await
            .map_err(|e| LychiError::ExecutionFailed(e.to_string()))?;

        let names = dbus
            .list_names()
            .await
            .map_err(|e| LychiError::ExecutionFailed(e.to_string()))?;

        let mut active_names: Vec<String> = names
            .iter()
            .filter(|n| n.as_str().starts_with(MPRIS_PREFIX))
            .map(|n| n.as_str().to_string())
            .collect();

        // Plasma Browser Integration mirrors the browser's own MPRIS player.
        // If the real browser player is present, skip the duplicate bridge.
        let has_browser = active_names.iter().any(|n| {
            let suffix = n.strip_prefix(MPRIS_PREFIX).unwrap_or("");
            suffix.starts_with("chromium")
                || suffix.starts_with("chrome")
                || suffix.starts_with("firefox")
                || suffix.starts_with("brave")
                || suffix.starts_with("vivaldi")
                || suffix.starts_with("edge")
        });
        if has_browser {
            active_names.retain(|n| !n.contains("plasma-browser-integration"));
        }

        // Remove players no longer on the bus
        self.players.retain(|name, _| active_names.contains(name));

        // Add new players
        for name in &active_names {
            if !self.players.contains_key(name) {
                let client = MprisClient::from_connection(Arc::clone(&self.conn), name.clone());
                self.players.insert(name.clone(), client);
                tracing::info!("[mpris] Discovered player: {name}");
            }
        }

        Ok(())
    }

    /// Get status from all connected players.
    pub async fn get_all_status(&self) -> Vec<TrackInfo> {
        let mut results = Vec::new();
        for client in self.players.values() {
            if let Ok(info) = client.get_status().await {
                results.push(info);
            }
        }
        // Sort: playing first, then paused, then stopped
        results.sort_by_key(|t| match t.status {
            PlaybackStatus::Playing => 0,
            PlaybackStatus::Paused => 1,
            PlaybackStatus::Stopped => 2,
        });
        results
    }

    /// Get status from a specific player.
    pub async fn get_status(&self, bus_name: &str) -> Result<TrackInfo, LychiError> {
        self.players
            .get(bus_name)
            .ok_or_else(|| LychiError::ExecutionFailed(format!("Player not found: {bus_name}")))?
            .get_status()
            .await
    }

    /// Send a control command to a specific player.
    pub async fn control(&self, bus_name: &str, action: &str) -> Result<(), LychiError> {
        self.players
            .get(bus_name)
            .ok_or_else(|| LychiError::ExecutionFailed(format!("Player not found: {bus_name}")))?
            .control(action)
            .await
    }

    /// Seek on a specific player.
    pub async fn seek(
        &self,
        bus_name: &str,
        track_id: &str,
        position_us: i64,
    ) -> Result<(), LychiError> {
        self.players
            .get(bus_name)
            .ok_or_else(|| LychiError::ExecutionFailed(format!("Player not found: {bus_name}")))?
            .set_position(track_id, position_us)
            .await
    }

    /// Send a control command to all connected players.
    pub async fn control_all(&self, action: &str) -> Result<usize, LychiError> {
        let mut count = 0;
        for player in self.players.values() {
            if player.control(action).await.is_ok() {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Get the list of currently known player bus names.
    pub fn player_names(&self) -> Vec<String> {
        self.players.keys().cloned().collect()
    }

    /// Check if any players are connected.
    pub fn has_players(&self) -> bool {
        !self.players.is_empty()
    }

    /// Subscribe to changes on all current players, returning a merged owned stream.
    pub async fn subscribe_all_changes(
        &self,
    ) -> Result<std::pin::Pin<Box<dyn futures_core::Stream<Item = TrackInfo> + Send>>, LychiError>
    {
        use futures_util::stream::SelectAll;

        let mut select_all = SelectAll::new();
        for client in self.players.values() {
            match client.subscribe_changes().await {
                Ok(stream) => select_all.push(stream),
                Err(e) => {
                    tracing::warn!("[mpris] Failed to subscribe to {}: {e}", client.bus_name())
                }
            }
        }
        Ok(Box::pin(select_all))
    }
}

/// Extract friendly player name from a bus name.
fn friendly_name(bus_name: &str) -> String {
    let raw = bus_name.strip_prefix(MPRIS_PREFIX).unwrap_or(bus_name);
    // Some players add ".instanceXXX" suffix — strip it
    let base = raw.split('.').next().unwrap_or(raw);
    // Capitalize first letter
    let mut chars = base.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

// --- Metadata extraction helpers ---

fn extract_string(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(|v| v.downcast_ref::<String>().ok())
}

fn extract_artists(metadata: &HashMap<String, OwnedValue>) -> Option<String> {
    metadata.get("xesam:artist").and_then(|v| {
        // xesam:artist is an array of strings (as)
        let arr = v.downcast_ref::<zbus::zvariant::Array>().ok()?;
        let artists: Vec<String> = arr
            .iter()
            .filter_map(|item| item.downcast_ref::<String>().ok())
            .collect();
        if artists.is_empty() {
            None
        } else {
            Some(artists.join(", "))
        }
    })
}

fn extract_i64(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<i64> {
    metadata.get(key).and_then(|v| v.downcast_ref::<i64>().ok())
}

fn extract_u64(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<u64> {
    metadata.get(key).and_then(|v| v.downcast_ref::<u64>().ok())
}

fn extract_object_path(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    metadata.get(key).and_then(|v| {
        v.downcast_ref::<zbus::zvariant::ObjectPath>()
            .ok()
            .map(|p| p.to_string())
    })
}
