use async_trait::async_trait;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::sync::{Mutex, OnceLock};

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::handlers::clipboard::write_to_clipboard;
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
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

/// `unicode`'s argument surface: a single free-form action whose flat form IS
/// the search query. The JSON Schema derives from this; the drift test pins
/// its rendering to the official-name lookup `execute` performs over the
/// built index.
const UNICODE_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Search Unicode characters by official name — arrows, box drawing, math \
               operators, punctuation, currency, dingbats, Greek/Cyrillic letters — \
               and copy the match to the clipboard with its codepoint. Fully local, \
               read-only.",
        mutates: false,
        operands: &[Operand {
            name: "query",
            desc: "The character to find, by a fragment of its official Unicode name — \
                   \"rightwards arrow\", \"em dash\", \"snowman\", \"bullet\". \
                   Matching is a case-insensitive substring over names like \
                   \"RIGHTWARDS ARROW\"; the first hit is copied. A literal non-ASCII \
                   character is copied as-is.",
            required: true,
            kind: ArgKind::Text,
            prefix: None,
        }],
    }],
};

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
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["unicode"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "unicode"
    }

    fn description(&self) -> &str {
        "Search Unicode characters by name (u:arrow or unicode arrow)"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(UNICODE_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Utils
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            return Ok(ActionResult::err(
                "Usage: u:<name> or unicode <name>".to_string(),
            ));
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
                        return Ok(ActionResult::err(format!(
                            "No Unicode character found for: {trimmed}"
                        )));
                    }
                }
            }
        } else {
            return Ok(ActionResult::err(format!(
                "No Unicode character found for: {trimmed}"
            )));
        };

        let ch_str = ch.to_string();
        match write_to_clipboard(&ch_str) {
            Ok(()) => Ok(ActionResult::ok(
                format!("Copied {ch} (U+{:04X}) to clipboard", ch as u32),
                OutputType::Status,
            )),
            Err(e) => Ok(ActionResult::err(format!("Clipboard error: {e}"))),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let query = partial.trim();

        // Empty query → popular characters
        if query.is_empty() {
            return POPULAR
                .iter()
                .enumerate()
                .map(|(i, (ch, name))| {
                    CompletionItem::new(
                        Self::format_label(*ch, name),
                        Some("__none__".into()),
                        (POPULAR.len() - i) as u16,
                    )
                    .with_run(format!("unicode {ch}"))
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

        results.sort_by_key(|b| std::cmp::Reverse(b.1));
        results.truncate(20);

        results
            .into_iter()
            .filter_map(|(i, score)| {
                index.get(i).map(|entry| {
                    CompletionItem::new(
                        Self::format_label(entry.ch, &entry.name),
                        Some("__none__".into()),
                        score,
                    )
                    .with_run(format!("unicode {}", entry.ch))
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

    #[test]
    fn unicode_args_flatten_from_structured_json() {
        // The grammar's flat rendering must be exactly what `execute`'s
        // official-name lookup accepts.
        let flat = UNICODE_GRAMMAR
            .flatten_json(r#"{"query":"rightwards arrow"}"#)
            .unwrap();
        assert_eq!(flat, "rightwards arrow");
        let lower = flat.to_lowercase();
        assert!(
            get_index()
                .iter()
                .any(|e| e.name.to_lowercase().contains(&lower)),
            "lookup should find a match for {flat:?}"
        );
        // Flat/legacy callers pass through untouched (caller keeps raw).
        assert_eq!(UNICODE_GRAMMAR.flatten_json("arrow"), None);
    }
}
