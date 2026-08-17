use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
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
    let duration = start.elapsed().as_millis() as u64;
    let mut result = match (success, output, error) {
        (true, Some(body), _) => ActionResult::ok(body, OutputType::Status),
        (true, None, _) => ActionResult::empty_ok(),
        (false, _, Some(e)) => ActionResult::err(e),
        (false, _, None) => ActionResult::err("media command failed"),
    };
    result.duration_ms = duration;
    result
}

/// Check if a bus name matches the given target strategy.
fn bus_matches_target(bn: &str, target: Target) -> bool {
    match target {
        Target::Spotify => bn.contains("spotify"),
        Target::Browser => crate::mpris::is_browser_bus(bn),
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
        "pause" => "pause",
        // `stop` is the real MPRIS Stop (halt + reset position), distinct from
        // pause. Players that don't implement Stop fall through harmlessly.
        "stop" => "stop",
        "next" | "skip" => "next",
        "prev" | "previous" | "back" => "prev",
        "toggle" => "play_pause",
        other => {
            return Ok(media_result(
                start,
                false,
                None,
                Some(format!(
                    "Unknown action: {other}. Try: play, pause, stop, next, prev, pause all"
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
        // `stop all` truly stops; `pause all` pauses.
        let (verb, past) = if action == "stop all" {
            ("stop", "Stopped")
        } else {
            ("pause", "Paused")
        };
        let count = mgr.control_all(verb).await?;
        return Ok(media_result(
            start,
            true,
            Some(format!("{past} {count} player(s)")),
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
            .find(|p| crate::mpris::is_browser_bus(&p.bus_name))
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

const MEDIA_SUBCOMMANDS: &[&str] = &[
    "play",
    "pause",
    "stop",
    "next",
    "prev",
    "toggle",
    "pause all",
];

/// The base transport VERBS the agent chooses between — the machine-readable
/// enum fed to the tool schema so a constrained model (cloud `enum` / local
/// grammar) can only emit one the transport match in `execute_media` handles.
/// These are exactly the canonical arms of that match (aliases like `resume`,
/// `skip`, `previous` are accepted on input but the model is steered to the
/// canonical spelling). Kept next to the parser it feeds so the two can't drift.
const MEDIA_ACTION_VERBS: &[&str] = &["play", "pause", "next", "prev", "toggle", "stop"];

/// The provider keys the flat grammar reads off the first token — a static
/// mirror of [`KNOWN_PROVIDERS`]'s names for the schema `Choice`. Pinned to the
/// dynamic source by `provider_choice_matches_known_providers` so the two
/// cannot diverge.
const MEDIA_PROVIDER_KEYS: &[&str] = &["spotify", "yt"];

/// The `media` grammar: a single free-form action whose flat rendering is the
/// provider-first `"[<provider>] <command> [all]"` string
/// [`parse_provider_and_args`] + `execute_media` already parse. Free-form
/// (rather than one verb per transport) because the provider must render
/// BEFORE the command, and a named verb always renders first. Declared
/// mutating: transport control changes live playback state on another
/// application — reversible, but state-changing, like `volume`.
const MEDIA_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Control media playback via MPRIS — works with Spotify, browser \
               tabs (YouTube), and local players. Pick the transport command and \
               optionally target one player.",
        mutates: true,
        operands: &[
            Operand {
                name: "provider",
                desc: "Which player to target: \"spotify\" for the Spotify app, \
                       \"yt\" for a browser player (YouTube). Omit to target the \
                       currently-playing player.",
                required: false,
                kind: ArgKind::Choice(MEDIA_PROVIDER_KEYS),
                prefix: None,
            },
            Operand {
                name: "command",
                desc: "The transport action: play, pause, stop (halt + reset \
                       position), next, prev, or toggle (play/pause flip).",
                required: true,
                kind: ArgKind::Choice(MEDIA_ACTION_VERBS),
                prefix: None,
            },
            Operand {
                name: "all",
                desc: "Apply to every running player at once instead of one. \
                       Only meaningful with pause or stop (e.g. \"silence \
                       everything\"); omit the provider when using it.",
                required: false,
                kind: ArgKind::Bool { flag: "all" },
                prefix: None,
            },
        ],
    }],
};

/// Normalize the tool's `args` to the flat `"[<provider>] <command> [all]"`
/// string the parser already understands, via the ONE structured→flat decider
/// ([`Grammar::flatten_json`]). A human or legacy/flat caller passes through
/// unchanged. Keeps `execute` on `&str`.
fn media_args_to_flat(args: &str) -> String {
    MEDIA_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
}

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
            thumb_b64: None,
            run: Some(format!("media {s}")),
            ..Default::default()
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
    } else if crate::mpris::is_browser_bus(bus_name) {
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
                thumb_b64: None,
                run: Some(format!("media {a}")),
                ..Default::default()
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
                thumb_b64: None,
                run: Some("media play".to_string()),
                ..Default::default()
            }],
            Some(p) => {
                let lower = partial.to_lowercase();
                let desc = if p.title.is_empty() {
                    p.player_name.clone()
                } else {
                    format!("{} — {}", p.title, p.artist)
                };

                let actions: &[&str] = match p.status {
                    PlaybackStatus::Playing => &["pause", "stop", "next", "prev", "toggle"],
                    PlaybackStatus::Paused => &["play", "stop", "next", "prev", "toggle"],
                    PlaybackStatus::Stopped => &["play"],
                };

                // These actions were computed for a *specific* provider (the
                // partial began with "spotify"/"yt"). Carry that provider into
                // `run` so the right player is controlled — a bare "media pause"
                // would target Target::Any instead. Derive the provider key from
                // the matched player's bus name.
                let provider = player_to_provider_key(&p.bus_name);

                actions
                    .iter()
                    .filter(|a| lower.is_empty() || a.contains(&lower))
                    .map(|a| CompletionItem {
                        label: a.to_string(),
                        icon_path: None,
                        score: 100,
                        description: Some(desc.clone()),
                        reason: None,
                        thumb_b64: None,
                        run: Some(format!("media {provider} {a}")),
                        ..Default::default()
                    })
                    .collect()
            }
        }
    }
}

#[async_trait]
impl ActionHandler for MediaHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["media"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "media"
    }

    fn description(&self) -> &str {
        "Media controls — play, pause, next, prev. Prefix with provider (spotify, yt) to target a specific player."
    }
    fn usage(&self) -> &str {
        "play, pause, next, prev, toggle, 'pause all'. Prefix with a provider to target a specific player (e.g. 'spotify pause', 'yt next')"
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(MEDIA_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Media
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Media
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // A structured caller sends `{"command":..,"provider":..}`; flatten it
        // (and a plain-string caller passes through) to the provider-first form
        // the parser reads.
        let flat = media_args_to_flat(args);
        let (target, action) = parse_provider_and_args(&flat);
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
                label: provider_key.clone(),
                icon_path: None,
                score: if player.status == PlaybackStatus::Playing {
                    100
                } else {
                    80
                },
                description: Some(description),
                reason: None,
                thumb_b64: None,
                run: Some(format!("media {provider_key}")),
                ..Default::default()
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
                thumb_b64: None,
                run: Some("media pause all".to_string()),
                ..Default::default()
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

    #[test]
    fn media_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; it flattens to the
        // provider-first string parse_provider_and_args reads.
        assert_eq!(
            media_args_to_flat(r#"{"command":"pause","provider":"spotify"}"#),
            "spotify pause"
        );
        // No provider → just the verb.
        assert_eq!(media_args_to_flat(r#"{"command":"next"}"#), "next");
        // Empty provider is treated as no provider.
        assert_eq!(
            media_args_to_flat(r#"{"command":"toggle","provider":""}"#),
            "toggle"
        );
        // The `all` flag renders after the command — the exact "pause all"
        // phrase execute_media's control-all branch matches.
        assert_eq!(
            media_args_to_flat(r#"{"command":"pause","all":true}"#),
            "pause all"
        );
        assert_eq!(
            media_args_to_flat(r#"{"command":"stop","all":true}"#),
            "stop all"
        );
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(media_args_to_flat("spotify pause"), "spotify pause");
        assert_eq!(media_args_to_flat("next"), "next");
        assert_eq!(media_args_to_flat("{not json"), "{not json");
    }

    /// Per-verb drift test: every command the grammar's Choice offers must
    /// flatten to a string the provider/transport parsers accept.
    #[test]
    fn media_grammar_flat_renderings_are_accepted_by_the_parser() {
        // The grammar's command Choice IS MEDIA_ACTION_VERBS — same const the
        // transport match documents as its canonical arms.
        let schema = MEDIA_GRAMMAR.handler_schema();
        let en = schema["properties"]["command"]["enum"].as_array().unwrap();
        assert_eq!(en.len(), MEDIA_ACTION_VERBS.len());
        for v in MEDIA_ACTION_VERBS {
            assert!(en.iter().any(|e| e == v), "enum missing {v}");
            // Bare command flattens to itself and reads as provider-less.
            let flat = media_args_to_flat(&format!(r#"{{"command":"{v}"}}"#));
            assert_eq!(&flat, v);
            let (target, action) = parse_provider_and_args(&flat);
            assert!(matches!(target, Target::Any));
            assert_eq!(action, *v);
            // With a provider, the provider is read off the first token and the
            // command survives intact for the transport match.
            for p in MEDIA_PROVIDER_KEYS {
                let flat = media_args_to_flat(&format!(r#"{{"provider":"{p}","command":"{v}"}}"#));
                let (target, action) = parse_provider_and_args(&flat);
                assert!(!matches!(target, Target::Any), "{p} not read as provider");
                assert_eq!(action, *v);
            }
        }
    }

    /// The static provider Choice must match the dynamic parser table — the
    /// drift guard for declaring [`MEDIA_PROVIDER_KEYS`] separately from
    /// [`KNOWN_PROVIDERS`].
    #[test]
    fn provider_choice_matches_known_providers() {
        let known: Vec<&str> = KNOWN_PROVIDERS.iter().map(|(name, _)| *name).collect();
        assert_eq!(MEDIA_PROVIDER_KEYS, known.as_slice());
    }
}
