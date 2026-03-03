use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem};
use crate::error::LychiError;
use crate::mpris::{MprisManager, PlaybackStatus, TrackInfo};

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

/// Helper to build a consistent ActionResult.
fn media_result(
    start: Instant,
    success: bool,
    output: Option<String>,
    error: Option<String>,
) -> ActionResult {
    ActionResult {
        success,
        output,
        error,
        duration_ms: start.elapsed().as_millis() as u64,
        routed_by: None,
        open_url: None,
        needs_confirmation: None,
        risk_level: None,
        output_type: None,
        executed_args: None,
        launch_desktop: None,
        focus_app: None,
    }
}

/// Check if a bus name matches the given target strategy.
fn bus_matches_target(bn: &str, target: Target) -> bool {
    match target {
        Target::Spotify => bn.contains("spotify"),
        Target::Browser => {
            bn.contains("chromium")
                || bn.contains("chrome")
                || bn.contains("firefox")
                || bn.contains("brave")
                || bn.contains("vivaldi")
                || bn.contains("edge")
        }
        Target::Any => true,
    }
}

/// Shared implementation for media transport commands.
async fn execute_media(
    mpris: &Arc<RwLock<Option<MprisManager>>>,
    target: Target,
    args: &str,
) -> Result<ActionResult, LychiError> {
    let action = args.trim().to_lowercase();
    let start = Instant::now();

    // No args → signal frontend to open media player panel
    if action.is_empty() {
        return Ok(media_result(
            start,
            true,
            Some("__media_panel__".to_string()),
            None,
        ));
    }

    // Parse the action verb
    let first_word = action.split_whitespace().next().unwrap_or(&action);
    let mpris_action = match first_word {
        "play" | "resume" => "play",
        "pause" | "stop" => "pause",
        "next" | "skip" => "next",
        "prev" | "previous" | "back" => "prev",
        "toggle" => "play_pause",
        other => {
            return Ok(media_result(
                start,
                false,
                None,
                Some(format!(
                    "Unknown action: {other}. Try: play, pause, next, prev, pause all"
                )),
            ));
        }
    };

    // Get manager: cached or fresh
    let guard = mpris.read().await;
    let fresh;
    let mgr: &MprisManager = if let Some(m) = guard.as_ref() {
        m
    } else {
        drop(guard);
        tracing::debug!("[media] No cached manager, connecting fresh");
        fresh = MprisManager::connect().await?;
        &fresh
    };

    // Dispatch
    if action == "pause all" || action == "stop all" {
        let count = mgr.control_all("pause").await?;
        return Ok(media_result(
            start,
            true,
            Some(format!("Paused {count} player(s)")),
            None,
        ));
    }

    match mgr
        .control_matching(|bn| bus_matches_target(bn, target), mpris_action)
        .await
    {
        Ok(player_name) => Ok(media_result(
            start,
            true,
            Some(format!("{player_name}: {action}")),
            None,
        )),
        Err(_) => Ok(media_result(
            start,
            false,
            None,
            Some("No media players running".to_string()),
        )),
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
                .find(|p| p.status == PlaybackStatus::Playing)
                .or(players.first())
        }
    }
}

const MEDIA_SUBCOMMANDS: &[&str] = &["play", "pause", "next", "prev", "toggle", "pause all"];

fn media_completions(partial: &str) -> Vec<CompletionItem> {
    let lower = partial.to_lowercase();
    MEDIA_SUBCOMMANDS
        .iter()
        .filter(|s| s.contains(&lower) || lower.is_empty())
        .map(|s| CompletionItem {
            label: s.to_string(),
            icon_path: None,
            score: if s.starts_with(&lower) { 100 } else { 50 },
            description: None,
            reason: None,
        })
        .collect()
}

/// Known media providers mapped to their targeting strategy.
const KNOWN_PROVIDERS: &[(&str, Target)] = &[
    ("spotify", Target::Spotify),
    ("yt", Target::Browser),
    // Future: ("soundcloud", Target::SoundCloud), etc.
];

/// Parse an optional provider prefix from args, returning the target and remaining args.
///
/// - `"spotify pause"` → `(Target::Spotify, "pause")`
/// - `"yt next"` → `(Target::Browser, "next")`
/// - `"pause"` → `(Target::Any, "pause")`
/// - `"spotify"` → `(Target::Spotify, "")`
fn parse_provider_and_args(args: &str) -> (Target, &str) {
    let first = args.split_whitespace().next().unwrap_or("");
    for (name, target) in KNOWN_PROVIDERS {
        if first == *name {
            let rest = args[first.len()..].trim_start();
            return (*target, rest);
        }
    }
    (Target::Any, args)
}

/// Map a D-Bus bus name back to the provider key used in args.
fn player_to_provider_key(bus_name: &str) -> String {
    if bus_name.contains("spotify") {
        "spotify".to_string()
    } else if bus_name.contains("chromium")
        || bus_name.contains("chrome")
        || bus_name.contains("firefox")
        || bus_name.contains("brave")
        || bus_name.contains("vivaldi")
        || bus_name.contains("edge")
    {
        "yt".to_string()
    } else {
        crate::mpris::friendly_name(bus_name).to_lowercase()
    }
}

/// Show actions filtered by player state, with track info in description.
fn state_aware_actions(players: &[TrackInfo], partial: &str) -> Vec<CompletionItem> {
    let lower = partial.to_lowercase();
    MEDIA_SUBCOMMANDS
        .iter()
        .filter(|a| a.contains(&lower))
        .filter_map(|a| {
            let p = players
                .iter()
                .find(|p| p.status == PlaybackStatus::Playing)
                .or(players.first())?;
            Some(CompletionItem {
                label: a.to_string(),
                icon_path: None,
                score: 70,
                description: Some(format!("{}: {}", p.player_name, p.title)),
                reason: None,
            })
        })
        .collect()
}

// --- Unified media handler ---

pub struct MediaHandler {
    mpris: Arc<RwLock<Option<MprisManager>>>,
}

impl MediaHandler {
    pub fn new(mpris: Arc<RwLock<Option<MprisManager>>>) -> Self {
        Self { mpris }
    }

    /// Read player state from cached MPRIS, falling back to a fresh connection.
    async fn get_players(&self) -> Vec<TrackInfo> {
        let guard = self.mpris.read().await;
        if let Some(mgr) = guard.as_ref() {
            return mgr.get_all_status().await;
        }
        drop(guard);

        // Fallback: fresh connection (first use before panel opened)
        match MprisManager::connect().await {
            Ok(mgr) => mgr.get_all_status().await,
            Err(_) => vec![],
        }
    }

    /// State-aware actions for a targeted provider.
    async fn player_action_completions(
        &self,
        target: Target,
        partial: &str,
    ) -> Vec<CompletionItem> {
        let players = self.get_players().await;
        let player = find_target(&players, target);

        match player {
            None => vec![CompletionItem {
                label: "play".to_string(),
                icon_path: None,
                score: 100,
                description: Some("Player not running".to_string()),
                reason: None,
            }],
            Some(p) => {
                let lower = partial.to_lowercase();
                let desc = if p.title.is_empty() {
                    p.player_name.clone()
                } else {
                    format!("{} — {}", p.title, p.artist)
                };

                let actions: &[&str] = match p.status {
                    PlaybackStatus::Playing => &["pause", "next", "prev", "toggle"],
                    PlaybackStatus::Paused => &["play", "next", "prev", "toggle"],
                    PlaybackStatus::Stopped => &["play"],
                };

                actions
                    .iter()
                    .filter(|a| lower.is_empty() || a.contains(&lower))
                    .map(|a| CompletionItem {
                        label: a.to_string(),
                        icon_path: None,
                        score: 100,
                        description: Some(desc.clone()),
                        reason: None,
                    })
                    .collect()
            }
        }
    }
}

#[async_trait]
impl ActionHandler for MediaHandler {
    fn id(&self) -> &str {
        "media"
    }

    fn description(&self) -> &str {
        "Media controls — play, pause, next, prev. Prefix with provider (spotify, yt) to target a specific player."
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let (target, action) = parse_provider_and_args(args);
        execute_media(&self.mpris, target, action).await
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let (target, action_partial) = parse_provider_and_args(partial);

        // If a specific provider is selected, show state-aware commands for that target
        if !matches!(target, Target::Any) {
            return self.player_action_completions(target, action_partial).await;
        }

        // No provider selected — show active players + global commands
        let players = self.get_players().await;

        if players.is_empty() {
            return media_completions(partial);
        }

        let mut items = Vec::new();

        // Active players as top-level completions
        for player in &players {
            let provider_key = player_to_provider_key(&player.bus_name);
            let status_icon = match player.status {
                PlaybackStatus::Playing => "\u{25b6}",
                PlaybackStatus::Paused => "\u{23f8}",
                PlaybackStatus::Stopped => "\u{23f9}",
            };
            let description = if player.title.is_empty() {
                player.status_label()
            } else {
                format!("{status_icon} {} — {}", player.title, player.artist)
            };

            // Filter by partial
            if !partial.is_empty()
                && !provider_key.contains(&partial.to_lowercase())
                && !player
                    .player_name
                    .to_lowercase()
                    .contains(&partial.to_lowercase())
            {
                continue;
            }

            items.push(CompletionItem {
                label: provider_key,
                icon_path: None,
                score: if player.status == PlaybackStatus::Playing {
                    100
                } else {
                    80
                },
                description: Some(description),
                reason: None,
            });
        }

        // Global: "pause all" if multiple active players
        if players.len() > 1 && players.iter().any(|p| p.status == PlaybackStatus::Playing) {
            items.push(CompletionItem {
                label: "pause all".to_string(),
                icon_path: None,
                score: 60,
                description: Some(format!("{} players active", players.len())),
                reason: None,
            });
        }

        // If partial looks like an action verb, show it with target info
        if !partial.is_empty() {
            items.extend(state_aware_actions(&players, partial));
        }

        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_provider_spotify() {
        let (target, args) = parse_provider_and_args("spotify pause");
        assert!(matches!(target, Target::Spotify));
        assert_eq!(args, "pause");
    }

    #[test]
    fn parse_provider_yt() {
        let (target, args) = parse_provider_and_args("yt next");
        assert!(matches!(target, Target::Browser));
        assert_eq!(args, "next");
    }

    #[test]
    fn parse_no_provider() {
        let (target, args) = parse_provider_and_args("pause");
        assert!(matches!(target, Target::Any));
        assert_eq!(args, "pause");
    }

    #[test]
    fn parse_empty() {
        let (target, args) = parse_provider_and_args("");
        assert!(matches!(target, Target::Any));
        assert_eq!(args, "");
    }

    #[test]
    fn parse_provider_only() {
        let (target, args) = parse_provider_and_args("spotify");
        assert!(matches!(target, Target::Spotify));
        assert_eq!(args, "");
    }
}
