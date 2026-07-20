//! Dictionary lookup — `define <word>`.
//!
//! Uses the free, key-less dictionaryapi.dev (Free Dictionary API). Results are
//! cached in-memory per word so repeat lookups (and the live completion preview
//! → Enter round-trip) are instant. The completion pass shows a short gloss as
//! it resolves; executing renders the full entry inline as readable text.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::action_registry::{
    ActionHandler, ActionResult, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;

const API_URL: &str = "https://api.dictionaryapi.dev/api/v2/entries/en/";

// ── API response shapes (only the fields we render) ─────────────────────

#[derive(Debug, Clone, Deserialize)]
struct ApiEntry {
    word: String,
    #[serde(default)]
    phonetic: Option<String>,
    #[serde(default)]
    meanings: Vec<ApiMeaning>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiMeaning {
    #[serde(rename = "partOfSpeech", default)]
    part_of_speech: String,
    #[serde(default)]
    definitions: Vec<ApiDefinition>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiDefinition {
    #[serde(default)]
    definition: String,
    #[serde(default)]
    example: Option<String>,
}

/// A resolved dictionary entry, condensed to what we display. `None` cached
/// value means "looked up, but the word wasn't found" — so we don't refetch a
/// miss on every keystroke.
#[derive(Debug, Clone)]
struct Entry {
    word: String,
    phonetic: Option<String>,
    /// (part-of-speech, definition, optional example), already truncated.
    senses: Vec<(String, String, Option<String>)>,
}

pub struct DefineHandler {
    client: Client,
    cache: Arc<RwLock<HashMap<String, Option<Entry>>>>,
}

impl Default for DefineHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl DefineHandler {
    pub fn new() -> Self {
        let client = Client::builder()
            .user_agent("lychi")
            .timeout(std::time::Duration::from_secs(6))
            .build()
            .unwrap_or_default();
        Self {
            client,
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// The lookup key for a word: trimmed, lowercased. Empty → None.
    fn normalize(word: &str) -> Option<String> {
        let w = word.trim().to_lowercase();
        if w.is_empty() { None } else { Some(w) }
    }

    /// Fetch (or return cached) the entry for `word`. `Ok(None)` = word not
    /// found (a definitive miss, cached); `Err` = network/parse failure (not
    /// cached, so it can be retried).
    async fn lookup(&self, word: &str) -> Result<Option<Entry>, LychiError> {
        let Some(key) = Self::normalize(word) else {
            return Ok(None);
        };
        if let Some(hit) = self.cache.read().await.get(&key) {
            return Ok(hit.clone());
        }

        let url = format!("{API_URL}{}", urlencoding_min(&key));
        let resp =
            self.client.get(&url).send().await.map_err(|e| {
                LychiError::ExecutionFailed(format!("dictionary request failed: {e}"))
            })?;

        // The API returns 404 with a JSON "no definitions" body for unknown
        // words — treat any non-success as a definitive miss (cache it).
        if !resp.status().is_success() {
            self.cache.write().await.insert(key, None);
            return Ok(None);
        }

        let entries: Vec<ApiEntry> = resp
            .json()
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("dictionary parse error: {e}")))?;

        let entry = condense(entries);
        self.cache.write().await.insert(key, entry.clone());
        Ok(entry)
    }
}

/// Condense the API's array of entries into our compact `Entry`. Flattens all
/// meanings' definitions, keeping at most the first few senses so the inline
/// card stays scannable. Returns `None` if there's nothing usable.
fn condense(entries: Vec<ApiEntry>) -> Option<Entry> {
    const MAX_SENSES: usize = 5;
    let first = entries.first()?;
    let word = first.word.clone();
    let phonetic = entries
        .iter()
        .find_map(|e| e.phonetic.clone().filter(|p| !p.is_empty()));

    let mut senses = Vec::new();
    for entry in &entries {
        for meaning in &entry.meanings {
            for def in &meaning.definitions {
                if def.definition.trim().is_empty() {
                    continue;
                }
                senses.push((
                    meaning.part_of_speech.clone(),
                    def.definition.clone(),
                    def.example.clone().filter(|e| !e.is_empty()),
                ));
                if senses.len() >= MAX_SENSES {
                    break;
                }
            }
            if senses.len() >= MAX_SENSES {
                break;
            }
        }
        if senses.len() >= MAX_SENSES {
            break;
        }
    }

    if senses.is_empty() {
        return None;
    }
    Some(Entry {
        word,
        phonetic,
        senses,
    })
}

/// Minimal percent-encoding for a single dictionary word (spaces/reserved
/// chars). Words are almost always plain, but hyphenated/multi-word queries can
/// contain a space; avoid pulling a dep for this one use.
fn urlencoding_min(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Render an entry as readable inline text.
fn render(entry: &Entry) -> String {
    let mut out = String::new();
    match &entry.phonetic {
        Some(p) => out.push_str(&format!("{}  {}\n\n", entry.word, p)),
        None => out.push_str(&format!("{}\n\n", entry.word)),
    }
    for (i, (pos, def, example)) in entry.senses.iter().enumerate() {
        if pos.is_empty() {
            out.push_str(&format!("{}. {def}\n", i + 1));
        } else {
            out.push_str(&format!("{}. ({pos}) {def}\n", i + 1));
        }
        if let Some(ex) = example {
            out.push_str(&format!("   \u{201c}{ex}\u{201d}\n"));
        }
    }
    out.trim_end().to_string()
}

#[async_trait]
impl ActionHandler for DefineHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["define"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "define"
    }

    fn description(&self) -> &str {
        "Define a word (dictionary lookup)"
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        if Self::normalize(args).is_none() {
            return Ok(ActionResult::err("Usage: define <word>".to_string()));
        }
        match self.lookup(args).await? {
            Some(entry) => Ok(ActionResult::ok(render(&entry), OutputType::Text)),
            None => Ok(ActionResult::err(format!(
                "No definition found for \u{201c}{}\u{201d}",
                args.trim()
            ))),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let Some(key) = Self::normalize(partial) else {
            return Vec::new();
        };
        // Only show a preview once we have a cached result — the live pass is
        // non-blocking. A cache miss triggers a background fetch so the gloss
        // appears on a later keystroke and Enter is instant.
        let cached = self.cache.read().await.get(&key).cloned();
        match cached {
            Some(Some(entry)) => {
                let (pos, def, _) = &entry.senses[0];
                let gloss = if pos.is_empty() {
                    def.clone()
                } else {
                    format!("({pos}) {def}")
                };
                vec![
                    CompletionItem::new(format!("{} — {gloss}", entry.word), None, 1000)
                        .with_run(format!("define {key}"))
                        .with_description("Enter for the full entry"),
                ]
            }
            Some(None) => Vec::new(), // known miss — don't offer a row
            None => {
                // Not yet fetched — warm it in the background (fire-and-forget).
                let client = self.client.clone();
                let cache = self.cache.clone();
                tokio::spawn(async move {
                    let handler = DefineHandler { client, cache };
                    let _ = handler.lookup(&key).await;
                });
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_trims_and_lowercases() {
        assert_eq!(
            DefineHandler::normalize("  Ephemeral "),
            Some("ephemeral".into())
        );
        assert_eq!(DefineHandler::normalize("   "), None);
    }

    #[test]
    fn urlencoding_handles_spaces_and_plain() {
        assert_eq!(urlencoding_min("cat"), "cat");
        assert_eq!(urlencoding_min("ad hoc"), "ad%20hoc");
        assert_eq!(urlencoding_min("well-known"), "well-known");
    }

    #[test]
    fn condense_flattens_and_caps() {
        let entries = vec![ApiEntry {
            word: "test".into(),
            phonetic: Some("/tɛst/".into()),
            meanings: vec![ApiMeaning {
                part_of_speech: "noun".into(),
                definitions: (0..10)
                    .map(|i| ApiDefinition {
                        definition: format!("meaning {i}"),
                        example: None,
                    })
                    .collect(),
            }],
        }];
        let e = condense(entries).unwrap();
        assert_eq!(e.word, "test");
        assert_eq!(e.phonetic.as_deref(), Some("/tɛst/"));
        assert_eq!(e.senses.len(), 5); // capped
        assert_eq!(e.senses[0].0, "noun");
    }

    #[test]
    fn condense_none_when_no_definitions() {
        let entries = vec![ApiEntry {
            word: "x".into(),
            phonetic: None,
            meanings: vec![],
        }];
        assert!(condense(entries).is_none());
    }

    #[test]
    fn render_includes_word_pos_and_example() {
        let entry = Entry {
            word: "ephemeral".into(),
            phonetic: Some("/ɪˈfɛm(ə)rəl/".into()),
            senses: vec![(
                "adjective".into(),
                "lasting a very short time".into(),
                Some("ephemeral pleasures".into()),
            )],
        };
        let out = render(&entry);
        assert!(out.contains("ephemeral"));
        assert!(out.contains("(adjective)"));
        assert!(out.contains("lasting a very short time"));
        assert!(out.contains("ephemeral pleasures"));
    }
}
