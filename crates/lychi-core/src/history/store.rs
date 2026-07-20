use std::sync::{Arc, Mutex};

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use redb::{Database, ReadableDatabase, ReadableTable};

use crate::action_registry::CompletionItem;
use crate::db::{
    self, frecency,
    schema::{HistoryEntry, SYNC_LOCAL},
};
use crate::error::LychiError;

/// Cached nucleo matcher for history fuzzy search.
static MATCHER: Mutex<Option<Matcher>> = Mutex::new(None);

#[derive(Clone)]
pub struct HistoryStore {
    max_entries: usize,
    deduplicate: bool,
}

impl HistoryStore {
    pub fn new(max_entries: usize, deduplicate: bool) -> Self {
        Self {
            max_entries,
            deduplicate,
        }
    }

    pub fn push(&self, db: &Arc<Database>, entry: &str) -> Result<(), LychiError> {
        let entry = entry.trim();
        if entry.is_empty() {
            return Ok(());
        }

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::HISTORY)?;

            if self.deduplicate {
                // Soft-delete existing entries with the same command
                let mut to_delete = Vec::new();
                for result in table.iter()? {
                    let (key, val) = result?;
                    let existing: HistoryEntry = postcard::from_bytes(val.value())
                        .map_err(|e| LychiError::Database(e.to_string()))?;
                    if existing.deleted_at.is_none() && existing.command == entry {
                        to_delete.push(key.value().to_string());
                    }
                }
                for key in &to_delete {
                    // Read, deserialize, drop guard before inserting
                    let existing = {
                        let guard = table.get(key.as_str())?;
                        guard.map(|g| {
                            postcard::from_bytes::<HistoryEntry>(g.value())
                                .map_err(|e| LychiError::Database(e.to_string()))
                        })
                    };
                    if let Some(Ok(mut entry)) = existing {
                        entry.deleted_at = Some(db::now_millis());
                        let bytes = postcard::to_allocvec(&entry)
                            .map_err(|e| LychiError::Database(e.to_string()))?;
                        table.insert(key.as_str(), bytes.as_slice())?;
                    }
                }
            }

            // Insert new entry with UUID v7 key (time-ordered)
            let id = db::new_id();
            let data = HistoryEntry {
                command: entry.to_string(),
                deleted_at: None,
                sync_status: SYNC_LOCAL,
            };
            let bytes =
                postcard::to_allocvec(&data).map_err(|e| LychiError::Database(e.to_string()))?;
            table.insert(id.as_str(), bytes.as_slice())?;

            // Enforce max entries by soft-deleting oldest
            self.enforce_max(&mut table)?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn entries(&self, db: &Arc<Database>) -> Result<Vec<String>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::HISTORY)?;
        let mut entries = Vec::new();
        for result in table.iter()? {
            let (_, val) = result?;
            let entry: HistoryEntry = postcard::from_bytes(val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if entry.deleted_at.is_none() {
                entries.push(entry.command);
            }
        }
        Ok(entries)
    }

    pub fn clear(&self, db: &Arc<Database>) -> Result<(), LychiError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::HISTORY)?;
            let now = db::now_millis();
            let keys: Vec<String> = table
                .iter()?
                .filter_map(|r| {
                    let (key, val) = r.ok()?;
                    let entry: HistoryEntry = postcard::from_bytes(val.value()).ok()?;
                    if entry.deleted_at.is_none() {
                        Some(key.value().to_string())
                    } else {
                        None
                    }
                })
                .collect();
            for key in &keys {
                let existing = {
                    let guard = table.get(key.as_str())?;
                    guard.map(|g| {
                        postcard::from_bytes::<HistoryEntry>(g.value())
                            .map_err(|e| LychiError::Database(e.to_string()))
                    })
                };
                if let Some(Ok(mut entry)) = existing {
                    entry.deleted_at = Some(now);
                    let bytes = postcard::to_allocvec(&entry)
                        .map_err(|e| LychiError::Database(e.to_string()))?;
                    table.insert(key.as_str(), bytes.as_slice())?;
                }
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Fuzzy-search history entries against `query`, blended with frecency scores.
    /// Returns up to 5 `CompletionItem`s sorted by blended score (descending).
    pub fn fuzzy_search(&self, db: &Arc<Database>, query: &str) -> Vec<CompletionItem> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }

        let entries = match self.entries(db) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        let mut guard = MATCHER.lock().unwrap();
        let matcher = guard.get_or_insert_with(|| Matcher::new(Config::DEFAULT));
        let pattern = Atom::new(
            query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
            false,
        );

        // Minimum score to accept a fuzzy match — prevents low-quality matches from
        // getting boosted to the top by frecency (e.g. "project" matching "whats my system memory?")
        const MIN_SCORE: u16 = 30;

        let mut scored: Vec<(&str, u16)> = entries
            .iter()
            .filter_map(|cmd| {
                let mut buf = Vec::new();
                let haystack = Utf32Str::new(cmd, &mut buf);
                let score = pattern.score(haystack, matcher)?;
                if score < MIN_SCORE {
                    return None;
                }
                Some((cmd.as_str(), score))
            })
            .collect();

        // Sort by nucleo score descending before taking top-N, so frecency boost
        // only affects items that are already strong fuzzy matches.
        scored.sort_unstable_by(|a, b| b.1.cmp(&a.1));

        // Blend with frecency
        let frecency_scores = frecency::get_scores(db);
        let mut items: Vec<CompletionItem> = scored
            .drain(..)
            .take(10)
            .map(|(cmd, nucleo_score)| {
                let key = format!("history:{cmd}");
                let frecency_val = frecency_scores.get(&key).copied().unwrap_or(0.0);
                let frecency_boost = (frecency_val * 300.0) as u16;
                let blended = nucleo_score.saturating_add(frecency_boost);

                CompletionItem {
                    label: cmd.to_string(),
                    icon_path: Some("__history__".to_string()),
                    score: blended,
                    description: None,
                    reason: None,
                    thumb_b64: None,
                    // A history entry's label IS the exact command already run —
                    // dispatch it verbatim so the frontend never re-prefixes it
                    // (input "run htop" + label "run htop" must not become
                    // "run run htop").
                    run: Some(cmd.to_string()),
                    ..Default::default()
                }
            })
            .collect();

        items.sort_by(|a, b| b.score.cmp(&a.score));
        items.truncate(5);
        items
    }

    /// Pre-warm the nucleo matcher so the first fuzzy search doesn't pay cold-start cost.
    pub fn warmup() {
        let mut guard = MATCHER.lock().unwrap();
        guard.get_or_insert_with(|| Matcher::new(Config::DEFAULT));
    }

    fn enforce_max(&self, table: &mut redb::Table<&str, &[u8]>) -> Result<(), LychiError> {
        let mut live_keys = Vec::new();
        for result in table.iter()? {
            let (key, val) = result?;
            let entry: HistoryEntry = postcard::from_bytes(val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if entry.deleted_at.is_none() {
                live_keys.push(key.value().to_string());
            }
        }

        if live_keys.len() > self.max_entries {
            let excess = live_keys.len() - self.max_entries;
            let now = db::now_millis();
            for key in live_keys.iter().take(excess) {
                let existing = {
                    let guard = table.get(key.as_str())?;
                    guard.map(|g| {
                        postcard::from_bytes::<HistoryEntry>(g.value())
                            .map_err(|e| LychiError::Database(e.to_string()))
                    })
                };
                if let Some(Ok(mut entry)) = existing {
                    entry.deleted_at = Some(now);
                    let bytes = postcard::to_allocvec(&entry)
                        .map_err(|e| LychiError::Database(e.to_string()))?;
                    table.insert(key.as_str(), bytes.as_slice())?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_test_database;

    #[test]
    fn push_and_retrieve() {
        let db = open_test_database();
        let store = HistoryStore::new(500, true);
        store.push(&db, "web rust").unwrap();
        store.push(&db, "open firefox").unwrap();
        let entries = store.entries(&db).unwrap();
        assert_eq!(entries, vec!["web rust", "open firefox"]);
    }

    #[test]
    fn fuzzy_search_sets_run_to_full_command() {
        let db = open_test_database();
        let store = HistoryStore::new(500, true);
        store.push(&db, "run htop").unwrap();
        let items = store.fuzzy_search(&db, "run htop");
        assert!(!items.is_empty());
        // The label is the past command, and `run` carries it verbatim so the
        // frontend dispatches it as-is (never re-prefixed into "run run htop").
        assert_eq!(items[0].label, "run htop");
        assert_eq!(items[0].run.as_deref(), Some("run htop"));
    }

    #[test]
    fn deduplication() {
        let db = open_test_database();
        let store = HistoryStore::new(500, true);
        store.push(&db, "web rust").unwrap();
        store.push(&db, "open firefox").unwrap();
        store.push(&db, "web rust").unwrap();
        let entries = store.entries(&db).unwrap();
        assert_eq!(entries, vec!["open firefox", "web rust"]);
    }

    #[test]
    fn max_entries_enforced() {
        let db = open_test_database();
        let store = HistoryStore::new(3, false);
        store.push(&db, "a").unwrap();
        store.push(&db, "b").unwrap();
        store.push(&db, "c").unwrap();
        store.push(&db, "d").unwrap();
        let entries = store.entries(&db).unwrap();
        assert_eq!(entries, vec!["b", "c", "d"]);
    }

    #[test]
    fn clear_history() {
        let db = open_test_database();
        let store = HistoryStore::new(500, true);
        store.push(&db, "web rust").unwrap();
        store.push(&db, "open firefox").unwrap();
        store.clear(&db).unwrap();
        let entries = store.entries(&db).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn empty_entry_ignored() {
        let db = open_test_database();
        let store = HistoryStore::new(500, true);
        store.push(&db, "").unwrap();
        store.push(&db, "   ").unwrap();
        let entries = store.entries(&db).unwrap();
        assert!(entries.is_empty());
    }
}
