use async_trait::async_trait;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::sync::{Mutex, OnceLock};

use crate::action_registry::handlers::clipboard::write_to_clipboard;
use crate::action_registry::{ActionHandler, ActionResult, CompletionItem, OutputType};
use crate::error::LychiError;

/// Cached nucleo matcher.
static MATCHER: Mutex<Option<Matcher>> = Mutex::new(None);

struct UnicodeEntry {
    ch: char,
    name: String,
}

/// Lazily-built index of searchable Unicode characters (~6000-8000 entries).
static INDEX: OnceLock<Vec<UnicodeEntry>> = OnceLock::new();

/// Popular characters shown on empty query.
const POPULAR: &[(char, &str)] = &[
    ('→', "RIGHTWARDS ARROW"),
    ('←', "LEFTWARDS ARROW"),
    ('↑', "UPWARDS ARROW"),
    ('↓', "DOWNWARDS ARROW"),
    ('•', "BULLET"),
    ('—', "EM DASH"),
    ('–', "EN DASH"),
    ('…', "HORIZONTAL ELLIPSIS"),
    ('™', "TRADE MARK SIGN"),
    ('©', "COPYRIGHT SIGN"),
    ('°', "DEGREE SIGN"),
    ('±', "PLUS-MINUS SIGN"),
    ('≠', "NOT EQUAL TO"),
    ('∞', "INFINITY"),
    ('×', "MULTIPLICATION SIGN"),
    ('÷', "DIVISION SIGN"),
    ('√', "SQUARE ROOT"),
    ('π', "GREEK SMALL LETTER PI"),
    ('λ', "GREEK SMALL LETTER LAMDA"),
    ('α', "GREEK SMALL LETTER ALPHA"),
];

/// Unicode ranges that contain useful, searchable characters.
const RANGES: &[(u32, u32)] = &[
    (0x0020, 0x007E), // Basic Latin (printable ASCII)
    (0x00A0, 0x00FF), // Latin-1 Supplement
    (0x0100, 0x024F), // Latin Extended-A & B
    (0x0370, 0x03FF), // Greek and Coptic
    (0x0400, 0x04FF), // Cyrillic
    (0x2000, 0x206F), // General Punctuation
    (0x2070, 0x209F), // Superscripts and Subscripts
    (0x20A0, 0x20CF), // Currency Symbols
    (0x2100, 0x214F), // Letterlike Symbols
    (0x2150, 0x218F), // Number Forms
    (0x2190, 0x21FF), // Arrows
    (0x2200, 0x22FF), // Mathematical Operators
    (0x2300, 0x23FF), // Miscellaneous Technical
    (0x2500, 0x257F), // Box Drawing
    (0x2580, 0x259F), // Block Elements
    (0x25A0, 0x25FF), // Geometric Shapes
    (0x2600, 0x26FF), // Miscellaneous Symbols
    (0x2700, 0x27BF), // Dingbats
    (0x2900, 0x297F), // Supplemental Arrows-B
    (0x2B00, 0x2BFF), // Miscellaneous Symbols and Arrows
];

fn build_index() -> Vec<UnicodeEntry> {
    let mut entries = Vec::with_capacity(8000);

    for &(start, end) in RANGES {
        for codepoint in start..=end {
            let Some(ch) = char::from_u32(codepoint) else {
                continue;
            };

            // Skip control characters and unassigned codepoints
            if ch.is_control() {
                continue;
            }

            if let Some(name) = unicode_names2::name(ch) {
                let name_str = name.to_string();
                // Skip entries with unhelpful names
                if name_str.starts_with('<') {
                    continue;
                }
                entries.push(UnicodeEntry { ch, name: name_str });
            }
        }
    }

    entries
}

fn get_index() -> &'static Vec<UnicodeEntry> {
    INDEX.get_or_init(build_index)
}

pub struct UnicodeHandler;

impl Default for UnicodeHandler {
    fn default() -> Self {
        Self
    }
}

impl UnicodeHandler {
    pub fn new() -> Self {
        Self
    }

    fn format_label(ch: char, name: &str) -> String {
        format!("{ch} {name} (U+{:04X})", ch as u32)
    }

    /// Extract the character from a completion label like "→ RIGHTWARDS ARROW (U+2192)".
    fn extract_char(label: &str) -> Option<char> {
        label.chars().next()
    }
}

#[async_trait]
impl ActionHandler for UnicodeHandler {
    fn id(&self) -> &str {
        "unicode"
    }

    fn description(&self) -> &str {
        "Search Unicode characters by name (u:arrow or unicode arrow)"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some("Usage: u:<name> or unicode <name>".to_string()),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            });
        }

        // Try to extract char from label format
        let ch = if let Some(c) = Self::extract_char(trimmed) {
            if !c.is_ascii_alphanumeric() || !c.is_ascii() {
                c
            } else {
                // It's a search query — find best match
                let index = get_index();
                let lower = trimmed.to_lowercase();
                match index
                    .iter()
                    .find(|e| e.name.to_lowercase().contains(&lower))
                {
                    Some(e) => e.ch,
                    None => {
                        return Ok(ActionResult {
                            success: false,
                            output: None,
                            error: Some(format!("No Unicode character found for: {trimmed}")),
                            duration_ms: 0,
                            routed_by: None,
                            open_url: None,
                            needs_confirmation: None,
                            risk_level: None,
                            output_type: None,
                            executed_args: None,
                        });
                    }
                }
            }
        } else {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some(format!("No Unicode character found for: {trimmed}")),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            });
        };

        let ch_str = ch.to_string();
        match write_to_clipboard(&ch_str) {
            Ok(()) => Ok(ActionResult {
                success: true,
                output: Some(format!("Copied {ch} (U+{:04X}) to clipboard", ch as u32)),
                error: None,
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: Some(OutputType::Status),
                executed_args: None,
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
            }),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let query = partial.trim();

        // Empty query → popular characters
        if query.is_empty() {
            return POPULAR
                .iter()
                .enumerate()
                .map(|(i, (ch, name))| CompletionItem {
                    label: Self::format_label(*ch, name),
                    icon_path: Some("__none__".to_string()),
                    score: (POPULAR.len() - i) as u16,
                    description: None,
                })
                .collect();
        }

        let index = get_index();

        // Fuzzy match against Unicode names
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
        let mut results: Vec<(usize, u16)> = Vec::new();

        for (i, entry) in index.iter().enumerate() {
            buf.clear();
            let haystack = Utf32Str::new(&entry.name, &mut buf);
            if let Some(score) = pattern.score(haystack, matcher) {
                results.push((i, score));
            }
        }

        results.sort_by(|a, b| b.1.cmp(&a.1));
        results.truncate(20);

        results
            .into_iter()
            .filter_map(|(i, score)| {
                index.get(i).map(|entry| CompletionItem {
                    label: Self::format_label(entry.ch, &entry.name),
                    icon_path: Some("__none__".to_string()),
                    score,
                    description: None,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_unicode_completions_empty() {
        let handler = UnicodeHandler::new();
        let results = handler.completions("").await;
        assert_eq!(results.len(), POPULAR.len());
    }

    #[tokio::test]
    async fn test_unicode_completions_arrow() {
        let handler = UnicodeHandler::new();
        let results = handler.completions("arrow").await;
        assert!(!results.is_empty());
        // Should find arrow characters
        assert!(results[0].label.contains("ARROW"));
    }

    #[tokio::test]
    async fn test_unicode_index_builds() {
        let index = get_index();
        // Should have thousands of entries
        assert!(index.len() > 1000, "Index has {} entries", index.len());
    }
}
