use async_trait::async_trait;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::sync::Mutex;

use crate::action_registry::handlers::clipboard::write_to_clipboard;
use crate::action_registry::{ActionHandler, ActionResult, CompletionItem, OutputType};
use crate::error::LychiError;

/// Cached nucleo matcher — reused across calls to avoid ~192ms cold-start.
static MATCHER: Mutex<Option<Matcher>> = Mutex::new(None);

/// Popular emoji shown when query is empty.
const POPULAR: &[(&str, &str)] = &[
    ("😀", "grinning face"),
    ("❤️", "red heart"),
    ("👍", "thumbs up"),
    ("🔥", "fire"),
    ("😂", "face with tears of joy"),
    ("🎉", "party popper"),
    ("✅", "check mark button"),
    ("💀", "skull"),
    ("🤔", "thinking face"),
    ("😊", "smiling face with smiling eyes"),
    ("👋", "waving hand"),
    ("🙏", "folded hands"),
    ("💯", "hundred points"),
    ("🎶", "musical notes"),
    ("😎", "smiling face with sunglasses"),
    ("🥳", "partying face"),
    ("🚀", "rocket"),
    ("⭐", "star"),
    ("💡", "light bulb"),
    ("👀", "eyes"),
];

pub struct EmojiHandler;

impl Default for EmojiHandler {
    fn default() -> Self {
        Self
    }
}

impl EmojiHandler {
    pub fn new() -> Self {
        Self
    }

    /// Extract the emoji character from a completion label like "🔥 fire".
    fn extract_emoji(label: &str) -> &str {
        // The emoji is everything before the first space
        label.split_once(' ').map_or(label, |(emoji, _)| emoji)
    }
}

#[async_trait]
impl ActionHandler for EmojiHandler {
    fn id(&self) -> &str {
        "emoji"
    }

    fn description(&self) -> &str {
        "Search and copy emoji by name (e:fire or emoji fire)"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some("Usage: e:<name> or emoji <name>".to_string()),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
                launch_desktop: None,
                focus_app: None,
            });
        }

        // If args looks like a completion label ("🔥 fire"), extract the emoji
        let emoji_char = if trimmed.starts_with(|c: char| !c.is_ascii()) {
            Self::extract_emoji(trimmed)
        } else {
            // It's a search query — find best match
            let found = emojis::iter()
                .find(|e| e.name().to_lowercase().contains(&trimmed.to_lowercase()))
                .or_else(|| {
                    emojis::iter().find(|e| {
                        e.shortcodes()
                            .any(|sc| sc.to_lowercase().contains(&trimmed.to_lowercase()))
                    })
                });
            match found {
                Some(e) => e.as_str(),
                None => {
                    return Ok(ActionResult {
                        success: false,
                        output: None,
                        error: Some(format!("No emoji found for: {trimmed}")),
                        duration_ms: 0,
                        routed_by: None,
                        open_url: None,
                        needs_confirmation: None,
                        risk_level: None,
                        output_type: None,
                        executed_args: None,
                        launch_desktop: None,
                        focus_app: None,
                    });
                }
            }
        };

        match write_to_clipboard(emoji_char) {
            Ok(()) => Ok(ActionResult {
                success: true,
                output: Some(format!("Copied {emoji_char} to clipboard")),
                error: None,
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: Some(OutputType::Status),
                executed_args: None,
                launch_desktop: None,
                focus_app: None,
            }),
            Err(e) => Ok(ActionResult {
                success: false,
                output: None,
                error: Some(format!("Clipboard error: {e}")),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
                launch_desktop: None,
                focus_app: None,
            }),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let query = partial.trim();

        // Empty query → popular emoji
        if query.is_empty() {
            return POPULAR
                .iter()
                .enumerate()
                .map(|(i, (ch, name))| CompletionItem {
                    label: format!("{ch} {name}"),
                    icon_path: Some("__none__".to_string()),
                    score: (POPULAR.len() - i) as u16,
                    description: Some("Popular".to_string()),
                    reason: None,
                })
                .collect();
        }

        // Fuzzy match against emoji names + shortcodes
        let mut matcher_guard = MATCHER.lock().unwrap();
        let matcher = matcher_guard.get_or_insert_with(|| Matcher::new(Config::DEFAULT));

        let pattern = Atom::new(
            query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
            false,
        );

        let mut buf = Vec::new();
        let mut results: Vec<(String, String, String, u16)> = Vec::new(); // (emoji, name, group, score)

        for emoji in emojis::iter() {
            let name = emoji.name();
            let haystack = Utf32Str::new(name, &mut buf);
            if let Some(score) = pattern.score(haystack, matcher) {
                results.push((
                    emoji.as_str().to_string(),
                    name.to_lowercase(),
                    format!("{:?}", emoji.group()),
                    score,
                ));
                continue;
            }

            // Also try matching against shortcodes
            for sc in emoji.shortcodes() {
                buf.clear();
                let haystack = Utf32Str::new(sc, &mut buf);
                if let Some(score) = pattern.score(haystack, matcher) {
                    results.push((
                        emoji.as_str().to_string(),
                        name.to_lowercase(),
                        format!("{:?}", emoji.group()),
                        score,
                    ));
                    break;
                }
            }
        }

        results.sort_by(|a, b| b.3.cmp(&a.3));
        results.truncate(20);

        results
            .into_iter()
            .map(|(ch, name, group, score)| CompletionItem {
                label: format!("{ch} {name}"),
                icon_path: Some("__none__".to_string()),
                score,
                description: Some(group),
                reason: None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_emoji_completions_empty() {
        let handler = EmojiHandler::new();
        let results = handler.completions("").await;
        assert_eq!(results.len(), POPULAR.len());
        assert!(results[0].label.contains("grinning"));
    }

    #[tokio::test]
    async fn test_emoji_completions_fire() {
        let handler = EmojiHandler::new();
        let results = handler.completions("fire").await;
        assert!(!results.is_empty());
        assert!(results[0].label.contains('🔥'));
    }

    #[tokio::test]
    async fn test_emoji_completions_heart() {
        let handler = EmojiHandler::new();
        let results = handler.completions("heart").await;
        assert!(!results.is_empty());
    }
}
