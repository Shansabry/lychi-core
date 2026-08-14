use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use redb::Database;
use serde::{Deserialize, Serialize};

use crate::action_registry::CompletionItem;
use crate::db::frecency;
use crate::error::LychiError;
use crate::filestore::JsonlLog;

/// Cached nucleo matcher for history fuzzy search.
static MATCHER: Mutex<Option<Matcher>> = Mutex::new(None);

/// One command in the history log. Just the command text — command history is a
/// device-local usage record, not portable content, so it needs no sync/tombstone
/// bookkeeping (the redb version's `deleted_at`/`sync_status` are gone: `clear`
/// unlinks the file, so there is nothing to soft-delete).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryRecord {
    command: String,
}

/// Command history, an append-only JSONL log (newest last) at
/// [`crate::paths::history_file`].
///
/// Device-local usage data, not user-authored content, so it lives in a file,
/// not the database. Ordering is file order; `push` dedups (removes a prior
/// occurrence so a repeat moves to the end) and caps to `max_entries` by
/// rewriting the log — cheap at the 500-entry ceiling. `clear` unlinks the file,
/// which actually reclaims the disk (the old soft-delete left every command the
/// user had ever typed sitting in the database, merely hidden — a privacy bug).
#[derive(Clone)]
pub struct HistoryStore {
    max_entries: usize,
    deduplicate: bool,
    path: PathBuf,
}

impl HistoryStore {
    pub fn new(max_entries: usize, deduplicate: bool) -> Self {
        Self {
            max_entries,
            deduplicate,
            path: crate::paths::history_file(),
        }
    }

    /// Store backed by an explicit file — for tests, so they never touch the real
    /// history file or race each other.
    #[cfg(test)]
    fn with_path(max_entries: usize, deduplicate: bool, path: PathBuf) -> Self {
        Self {
            max_entries,
            deduplicate,
            path,
        }
    }

    fn log(&self) -> JsonlLog {
        JsonlLog::new(self.path.clone())
    }

    /// Append a command. Dedups (drops any prior identical command so the repeat
    /// moves to the end) and caps to `max_entries`, rewriting the log when either
    /// removes something. A blank command is ignored.
    pub fn push(&self, entry: &str) -> Result<(), LychiError> {
        let entry = entry.trim();
        if entry.is_empty() {
            return Ok(());
        }
        let log = self.log();
        let mut records: Vec<HistoryRecord> = log.load()?;
        let new_record = HistoryRecord {
            command: entry.to_string(),
        };

        // Dedup: remove any earlier occurrence so the repeat becomes the newest.
        let removed_a_duplicate = if self.deduplicate {
            let before = records.len();
            records.retain(|r| r.command != entry);
            records.len() != before
        } else {
            false
        };
        records.push(new_record.clone());

        // Cap: keep the newest `max_entries` (a `max_entries` of 0 means unbounded,
        // matching the redb store, which only trimmed when `len > max_entries`).
        let trimmed = if self.max_entries > 0 && records.len() > self.max_entries {
            let excess = records.len() - self.max_entries;
            records.drain(0..excess);
            true
        } else {
            false
        };

        // Fast path — a single append — is valid ONLY when the on-disk log already
        // equals `records` minus the new tail: i.e. nothing was removed (no dedup
        // hit, no trim). Otherwise the file and the in-memory list have diverged
        // and the whole log must be rewritten. (A wrong fast-path append here left
        // trimmed/duplicate rows on disk — the `max_entries_enforced` failure.)
        if !removed_a_duplicate && !trimmed {
            log.append(&new_record)?;
        } else {
            log.rewrite(&records)?;
        }
        Ok(())
    }

    /// All commands, oldest → newest (file order).
    pub fn entries(&self) -> Result<Vec<String>, LychiError> {
        Ok(self
            .log()
            .load::<HistoryRecord>()?
            .into_iter()
            .map(|r| r.command)
            .collect())
    }

    /// Delete every history entry by unlinking the file — the data is actually
    /// gone, not merely hidden.
    pub fn clear(&self) -> Result<(), LychiError> {
        self.log().clear()
    }

    /// Fuzzy-search history entries against `query`, blended with frecency scores.
    /// Returns up to 5 `CompletionItem`s sorted by blended score (descending).
    ///
    /// Still takes the database because frecency scores live in redb; when
    /// frecency also moves to a file this argument goes away.
    pub fn fuzzy_search(&self, db: &Arc<Database>, query: &str) -> Vec<CompletionItem> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }

        let entries = match self.entries() {
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
        scored.sort_unstable_by_key(|b| std::cmp::Reverse(b.1));

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

        items.sort_by_key(|b| std::cmp::Reverse(b.score));
        items.truncate(5);
        items
    }

    /// Pre-warm the nucleo matcher so the first fuzzy search doesn't pay cold-start cost.
    pub fn warmup() {
        let mut guard = MATCHER.lock().unwrap();
        guard.get_or_insert_with(|| Matcher::new(Config::DEFAULT));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_test_database;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn store(max_entries: usize, deduplicate: bool) -> HistoryStore {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "lychi_history_test_{}_{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        HistoryStore::with_path(max_entries, deduplicate, path)
    }

    #[test]
    fn push_and_retrieve() {
        let store = store(500, true);
        store.push("web rust").unwrap();
        store.push("open firefox").unwrap();
        assert_eq!(store.entries().unwrap(), vec!["web rust", "open firefox"]);
        store.clear().unwrap();
    }

    #[test]
    fn deduplication_moves_repeat_to_the_end() {
        let store = store(500, true);
        store.push("web rust").unwrap();
        store.push("open firefox").unwrap();
        store.push("web rust").unwrap();
        assert_eq!(store.entries().unwrap(), vec!["open firefox", "web rust"]);
        store.clear().unwrap();
    }

    #[test]
    fn dedup_keeps_the_file_bounded() {
        // 50 pushes of one command must leave exactly one record, not grow the
        // file (the redb version's tombstones made the repeated-command case grow
        // the very thing every read scans).
        let store = store(500, true);
        for _ in 0..50 {
            store.push("same command").unwrap();
        }
        assert_eq!(store.entries().unwrap(), vec!["same command"]);
        assert_eq!(store.log().approx_len().unwrap(), 1);
        store.clear().unwrap();
    }

    #[test]
    fn max_entries_enforced() {
        let store = store(3, false);
        store.push("a").unwrap();
        store.push("b").unwrap();
        store.push("c").unwrap();
        store.push("d").unwrap();
        assert_eq!(store.entries().unwrap(), vec!["b", "c", "d"]);
        store.clear().unwrap();
    }

    #[test]
    fn the_file_stays_bounded_not_just_the_live_set() {
        // Push far more than the cap; the on-disk record count must stay at the
        // cap, so read cost stays flat.
        let store = store(10, true);
        for i in 0..200 {
            store.push(&format!("cmd {i}")).unwrap();
        }
        assert_eq!(store.entries().unwrap().len(), 10);
        assert!(
            store.log().approx_len().unwrap() <= 10,
            "file holds {} records for a 10-entry limit",
            store.log().approx_len().unwrap()
        );
        store.clear().unwrap();
    }

    #[test]
    fn clear_actually_deletes_the_data() {
        // Privacy: clear must remove the file, not hide entries. After clear, the
        // backing file is gone (nothing left on disk to recover).
        let store = store(500, true);
        store.push("something private").unwrap();
        assert!(store.path.exists());
        store.clear().unwrap();
        assert!(store.entries().unwrap().is_empty());
        assert!(
            !store.path.exists(),
            "cleared history left the file on disk"
        );
    }

    #[test]
    fn blank_commands_are_ignored() {
        let store = store(500, true);
        store.push("   ").unwrap();
        store.push("").unwrap();
        assert!(store.entries().unwrap().is_empty());
        store.clear().unwrap();
    }

    #[test]
    fn fuzzy_search_sets_run_to_full_command() {
        let db = open_test_database();
        let store = store(500, true);
        store.push("run htop").unwrap();
        let items = store.fuzzy_search(&db, "run htop");
        assert!(!items.is_empty());
        // The label is the past command, and `run` carries it verbatim so the
        // frontend dispatches it as-is (never re-prefixed into "run run htop").
        assert_eq!(items[0].label, "run htop");
        assert_eq!(items[0].run.as_deref(), Some("run htop"));
        store.clear().unwrap();
    }
}
