use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use zbus::Connection;
use zbus::zvariant::OwnedValue;

use crate::error::LychiError;

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

/// Web-browser MPRIS bus-name fragments (browsers expose YouTube/web audio as
/// MPRIS players). Single source of truth — the browser-dedup in `refresh` and
/// the `Browser`/`yt` targeting in the media handler both key off this.
const BROWSER_BUS_FRAGMENTS: &[&str] =
    &["chromium", "chrome", "firefox", "brave", "vivaldi", "edge"];

/// Whether an MPRIS bus name belongs to a web browser.
pub fn is_browser_bus(bus_name: &str) -> bool {
    BROWSER_BUS_FRAGMENTS
        .iter()
        .any(|frag| bus_name.contains(frag))
}

/// Current playback state from a media player via MPRIS.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, specta::Type)]
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
            "stop" => "Stop",
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

/// Tracks last-activity ordering across MPRIS players by bus name.
///
/// A monotonically increasing sequence is stamped on a player whenever it is
/// observed playing or freshly discovered; the highest sequence is the most
/// recently active. This is how we pick the "most recently active" player among
/// equal-status candidates — the MPRIS convention — instead of an arbitrary
/// alphabetical tiebreak. Interior mutability (Mutex + atomic) because the
/// pick/status methods that stamp activity take `&self`.
#[derive(Default)]
struct ActivityTracker {
    ranks: std::sync::Mutex<HashMap<String, u64>>,
    seq: std::sync::atomic::AtomicU64,
}

impl ActivityTracker {
    /// Stamp a bus name as just-active (next in the monotonic sequence).
    fn mark(&self, bus_name: &str) {
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut map) = self.ranks.lock() {
            map.insert(bus_name.to_string(), seq);
        }
    }

    /// Current activity rank for a bus name (higher = more recently active,
    /// 0 if never observed active).
    fn rank(&self, bus_name: &str) -> u64 {
        self.ranks
            .lock()
            .ok()
            .and_then(|m| m.get(bus_name).copied())
            .unwrap_or(0)
    }

    /// Drop ranks for players no longer present.
    fn retain(&self, keep: &[String]) {
        if let Ok(mut map) = self.ranks.lock() {
            map.retain(|name, _| keep.contains(name));
        }
    }
}

/// Total-order sort key for ranking players: primary by status tier
/// (Playing < Paused < Stopped, ascending so Playing sorts first), secondary by
/// *descending* activity recency (encoded as `u64::MAX - rank`) so the most
/// recently active player wins ties. Pure — unit-tested independently of D-Bus.
fn sort_key(status: &PlaybackStatus, activity_rank: u64) -> (u8, u64) {
    let tier = match status {
        PlaybackStatus::Playing => 0,
        PlaybackStatus::Paused => 1,
        PlaybackStatus::Stopped => 2,
    };
    (tier, u64::MAX - activity_rank)
}

/// Manages multiple MPRIS media players, discovering them on the D-Bus.
pub struct MprisManager {
    conn: Arc<Connection>,
    players: HashMap<String, MprisClient>,
    activity: ActivityTracker,
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
            activity: ActivityTracker::default(),
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
            // Match on the suffix so the bridge itself ("plasma-browser-…")
            // doesn't count as a real browser player.
            is_browser_bus(suffix)
        });
        if has_browser {
            active_names.retain(|n| !n.contains("plasma-browser-integration"));
        }

        // Remove players no longer on the bus
        self.players.retain(|name, _| active_names.contains(name));
        // Drop stale activity entries for players that are gone.
        self.activity.retain(&active_names);

        // Add new players. A newly-appeared player is the most recent activity
        // (the user just opened/started it), so stamp it active.
        for name in &active_names {
            if !self.players.contains_key(name) {
                let client = MprisClient::from_connection(Arc::clone(&self.conn), name.clone());
                self.players.insert(name.clone(), client);
                self.activity.mark(name);
                tracing::info!("[mpris] Discovered player: {name}");
            }
        }

        Ok(())
    }

    /// Get status from all connected players.
    ///
    /// Sorted by playback status (Playing > Paused > Stopped), then by
    /// **last-activity recency** within each status tier — so the most recently
    /// active player wins ties, per the MPRIS convention, rather than an
    /// arbitrary alphabetical order. A player observed Playing here is stamped
    /// active, keeping the recency ranking fresh as playback state is polled.
    pub async fn get_all_status(&self) -> Vec<TrackInfo> {
        let mut results = Vec::new();
        for client in self.players.values() {
            if let Ok(info) = client.get_status().await {
                if info.status == PlaybackStatus::Playing {
                    self.activity.mark(&info.bus_name);
                }
                results.push(info);
            }
        }
        results.sort_by(|a, b| {
            sort_key(&a.status, self.activity.rank(&a.bus_name))
                .cmp(&sort_key(&b.status, self.activity.rank(&b.bus_name)))
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

    /// Find the best player whose bus name matches a predicate, or fall back to
    /// any player. Returns the bus name. Avoids fetching full metadata just to
    /// pick a target. When several players match (or as the no-match fallback),
    /// the **most recently active** one wins — the MPRIS convention — instead of
    /// an arbitrary alphabetical pick.
    pub fn find_player_by_bus(&self, predicate: impl Fn(&str) -> bool) -> Option<String> {
        let by_recency =
            |a: &&String, b: &&String| self.activity.rank(a).cmp(&self.activity.rank(b));
        // Prefer the most-recently-active matching player.
        if let Some(name) = self
            .players
            .keys()
            .filter(|name| predicate(name.as_str()))
            .max_by(by_recency)
        {
            return Some(name.clone());
        }
        // Fallback: the most-recently-active player overall.
        self.players.keys().max_by(by_recency).cloned()
    }

    /// Send a control command to the first player matching a predicate (or any player).
    /// Prefers the currently **playing** player among matches (via status-sorted list).
    pub async fn control_matching(
        &self,
        predicate: impl Fn(&str) -> bool,
        action: &str,
    ) -> Result<String, LychiError> {
        // get_all_status() returns players sorted: Playing > Paused > Stopped
        let statuses = self.get_all_status().await;

        // First matching player in priority order (playing first)
        let target = statuses
            .iter()
            .find(|t| predicate(&t.bus_name))
            .or_else(|| statuses.first());

        let bus_name = target
            .map(|t| t.bus_name.clone())
            .ok_or_else(|| LychiError::ExecutionFailed("No media players running".to_string()))?;

        self.control(&bus_name, action).await?;
        Ok(friendly_name(&bus_name))
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

    /// A stream that yields once every time an MPRIS player appears or
    /// disappears on the bus (a `NameOwnerChanged` for `org.mpris.MediaPlayer2.*`).
    ///
    /// The push listener builds its merged change-stream over the players that
    /// exist RIGHT NOW (`subscribe_all_changes`). Launched at login there are
    /// usually zero, so that stream is empty forever and a player started later
    /// is never observed (RES-8). The supervisor uses this to know WHEN to
    /// `refresh()` + rebuild the merged stream.
    pub async fn watch_players(
        &self,
    ) -> Result<std::pin::Pin<Box<dyn futures_core::Stream<Item = ()> + Send>>, LychiError> {
        use futures_util::StreamExt;

        let dbus = zbus::fdo::DBusProxy::new(&self.conn)
            .await
            .map_err(|e| LychiError::ExecutionFailed(e.to_string()))?;
        let changes = dbus
            .receive_name_owner_changed()
            .await
            .map_err(|e| LychiError::ExecutionFailed(e.to_string()))?;

        let stream = changes.filter_map(|signal| async move {
            match signal.args() {
                Ok(args) if args.name().starts_with(MPRIS_PREFIX) => Some(()),
                _ => None,
            }
        });
        Ok(Box::pin(stream))
    }
}

impl TrackInfo {
    /// Human-readable status with player name.
    pub fn status_label(&self) -> String {
        match self.status {
            PlaybackStatus::Playing => format!("▶ {}", self.player_name),
            PlaybackStatus::Paused => format!("⏸ {}", self.player_name),
            PlaybackStatus::Stopped => format!("⏹ {}", self.player_name),
        }
    }
}

/// Extract friendly player name from a bus name.
pub fn friendly_name(bus_name: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_browser_bus_matches_known_browsers() {
        for bn in [
            "org.mpris.MediaPlayer2.firefox.instance1234",
            "org.mpris.MediaPlayer2.chromium.instance99",
            "org.mpris.MediaPlayer2.chrome",
            "org.mpris.MediaPlayer2.brave.instance1",
            "org.mpris.MediaPlayer2.vivaldi",
            "org.mpris.MediaPlayer2.edge",
        ] {
            assert!(is_browser_bus(bn), "should match: {bn}");
        }
        // Non-browsers must not match.
        for bn in [
            "org.mpris.MediaPlayer2.spotify",
            "org.mpris.MediaPlayer2.vlc",
            "org.mpris.MediaPlayer2.mpv",
        ] {
            assert!(!is_browser_bus(bn), "should NOT match: {bn}");
        }
    }

    #[test]
    fn friendly_name_strips_prefix_instance_and_capitalizes() {
        assert_eq!(friendly_name("org.mpris.MediaPlayer2.spotify"), "Spotify");
        assert_eq!(
            friendly_name("org.mpris.MediaPlayer2.firefox.instance42"),
            "Firefox"
        );
    }

    #[test]
    fn activity_tracker_ranks_by_recency() {
        let t = ActivityTracker::default();
        // Never-seen players rank 0.
        assert_eq!(t.rank("a"), 0);
        // Marking increases rank monotonically; the last-marked ranks highest.
        t.mark("a");
        t.mark("b");
        t.mark("a"); // a is now more recent than b
        assert!(
            t.rank("a") > t.rank("b"),
            "re-marked player should be newer"
        );
        // retain drops absent players.
        t.retain(&["a".to_string()]);
        assert_eq!(t.rank("b"), 0, "dropped player resets to 0");
        assert!(t.rank("a") > 0);
    }

    #[test]
    fn sort_key_orders_status_then_recency() {
        use PlaybackStatus::*;
        // Status tier dominates: any Playing sorts before any Paused/Stopped,
        // regardless of recency.
        assert!(sort_key(&Playing, 0) < sort_key(&Paused, u64::MAX - 1));
        assert!(sort_key(&Paused, 5) < sort_key(&Stopped, u64::MAX - 1));
        // Within a tier, higher activity rank (more recent) sorts first.
        assert!(sort_key(&Paused, 10) < sort_key(&Paused, 3));
        assert!(sort_key(&Playing, 100) < sort_key(&Playing, 99));
    }

    #[test]
    fn sort_key_full_ordering_picks_recent_paused_over_old_paused() {
        use PlaybackStatus::*;
        // Simulate get_all_status: a playing player, then two paused ones where
        // the second was used more recently. Expect: playing first, then the
        // recently-used paused player, then the older paused one.
        let mut players = [
            ("old_paused", Paused, 2u64),
            ("playing", Playing, 1u64),
            ("recent_paused", Paused, 5u64),
        ];
        players.sort_by_key(|a| sort_key(&a.1, a.2));
        let order: Vec<&str> = players.iter().map(|p| p.0).collect();
        assert_eq!(order, ["playing", "recent_paused", "old_paused"]);
    }
}
