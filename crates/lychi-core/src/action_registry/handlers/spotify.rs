use std::time::Instant;

use async_trait::async_trait;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem};
use crate::error::LychiError;
use crate::mpris::{MprisManager, TrackInfo};

/// Player targeting strategy for media commands.
#[derive(Clone, Copy)]
enum Target {
    /// `spotify` — prefer Spotify, fall back to any player
    Spotify,
    /// `yt` — prefer browser players (YouTube), fall back to any player
    Browser,
    /// `media` — prefer currently playing player, fall back to first
    Any,
}

/// Shared implementation for media transport commands.
async fn execute_media(target: Target, args: &str) -> Result<ActionResult, LychiError> {
    let action = args.trim().to_lowercase();
    let start = Instant::now();

    // No args → signal frontend to open media player panel
    if action.is_empty() {
        return Ok(ActionResult {
            success: true,
            output: Some("__media_panel__".to_string()),
            error: None,
            duration_ms: start.elapsed().as_millis() as u64,
            routed_by: None,
            open_url: None,
            needs_confirmation: None,
            risk_level: None,
        });
    }

    let manager = MprisManager::connect().await?;

    // Handle "pause all" / "stop all" — pauses every running player
    if action == "pause all" || action == "stop all" {
        let count = manager.control_all("pause").await?;
        return Ok(ActionResult {
            success: true,
            output: Some(format!("Paused {count} player(s)")),
            error: None,
            duration_ms: start.elapsed().as_millis() as u64,
            routed_by: None,
            open_url: None,
            needs_confirmation: None,
            risk_level: None,
        });
    }

    // Extract the first word as the action
    let first_word = action.split_whitespace().next().unwrap_or(&action);
    let mpris_action = match first_word {
        "play" | "resume" => "play",
        "pause" | "stop" => "pause",
        "next" | "skip" => "next",
        "prev" | "previous" | "back" => "prev",
        "toggle" => "play_pause",
        other => {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some(format!(
                    "Unknown action: {other}. Try: play, pause, next, prev, pause all"
                )),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
            });
        }
    };

    let players = manager.get_all_status().await;

    let player = find_target(&players, target);

    match player {
        None => Ok(ActionResult {
            success: false,
            output: None,
            error: Some("No media players running".to_string()),
            duration_ms: start.elapsed().as_millis() as u64,
            routed_by: None,
            open_url: None,
            needs_confirmation: None,
            risk_level: None,
        }),
        Some(p) => {
            manager.control(&p.bus_name, mpris_action).await?;
            Ok(ActionResult {
                success: true,
                output: Some(format!("{}: {action}", p.player_name)),
                error: None,
                duration_ms: start.elapsed().as_millis() as u64,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
            })
        }
    }
}

/// Find the best player matching the target strategy.
fn find_target(players: &[TrackInfo], target: Target) -> Option<&TrackInfo> {
    if players.is_empty() {
        return None;
    }

    match target {
        Target::Spotify => players
            .iter()
            .find(|p| p.bus_name.contains("spotify"))
            .or(players.first()),
        Target::Browser => players
            .iter()
            .find(|p| {
                let bn = &p.bus_name;
                bn.contains("chromium")
                    || bn.contains("chrome")
                    || bn.contains("firefox")
                    || bn.contains("brave")
                    || bn.contains("vivaldi")
                    || bn.contains("edge")
            })
            .or(players.first()),
        Target::Any => {
            // Prefer the currently playing player
            players
                .iter()
                .find(|p| p.status == crate::mpris::PlaybackStatus::Playing)
                .or(players.first())
        }
    }
}

const MEDIA_SUBCOMMANDS: &[&str] = &[
    "play", "pause", "next", "prev", "toggle", "pause all",
];

fn media_completions(partial: &str) -> Vec<CompletionItem> {
    let lower = partial.to_lowercase();
    MEDIA_SUBCOMMANDS
        .iter()
        .filter(|s| s.contains(&lower) || lower.is_empty())
        .map(|s| CompletionItem {
            label: s.to_string(),
            icon_path: None,
            score: if s.starts_with(&lower) { 100 } else { 50 },
        })
        .collect()
}

// --- Spotify handler ---

pub struct SpotifyHandler;

impl Default for SpotifyHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl SpotifyHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ActionHandler for SpotifyHandler {
    fn id(&self) -> &str {
        "spotify"
    }

    fn description(&self) -> &str {
        "Spotify controls — play, pause, next, prev"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        execute_media(Target::Spotify, args).await
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        media_completions(partial)
    }
}

// --- YouTube/browser handler ---

pub struct YtHandler;

impl Default for YtHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl YtHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ActionHandler for YtHandler {
    fn id(&self) -> &str {
        "yt"
    }

    fn description(&self) -> &str {
        "YouTube/browser media controls — play, pause, next, prev"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        execute_media(Target::Browser, args).await
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        media_completions(partial)
    }
}

// --- Unified media handler ---

pub struct MediaHandler;

impl Default for MediaHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ActionHandler for MediaHandler {
    fn id(&self) -> &str {
        "media"
    }

    fn description(&self) -> &str {
        "Media controls (any player) — play, pause, next, prev"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        execute_media(Target::Any, args).await
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        media_completions(partial)
    }
}
