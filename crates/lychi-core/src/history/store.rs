use std::sync::{Arc, Mutex};

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata};

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
                // REMOVE the previous occurrence rather than tombstoning it.
                // A tombstone stays in the table forever and is deserialized by
                // every later scan, so dedup was making the thing it scans grow.
                let mut to_delete = Vec::new();
                for result in table.iter()? {
                    let (key, val) = result?;
                    // One unreadable row must not hide the rest of the list.
                    let Some(existing) =
                        db::decode_row::<HistoryEntry>("history", key.value(), val.value())
                    else {
                        continue;
                    };
                    if existing.command == entry {
                        to_delete.push(key.value().to_string());
                    }
                }
                for key in &to_delete {
                    table.remove(key.as_str())?;
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
            // One unreadable row must not hide the rest of the list.
            let Some(entry) = db::decode_row::<HistoryEntry>("history", "?", val.value()) else {
                continue;
            };
            // Still checked although nothing writes tombstones any more: an
            // existing database carries whatever the old soft-delete wrote until
            // `purge_tombstones` runs at startup, and a deleted command must not
            // reappear in the list in the meantime.
            if entry.deleted_at.is_none() {
                entries.push(entry.command);
            }
        }
        Ok(entries)
    }

    /// Delete every history entry.
    ///
    /// Actually deletes. This used to tombstone, which meant "clear history"
    /// left every command the user had ever typed sitting in the database,
    /// merely hidden from the list — the opposite of what the action promises,
    /// and a privacy failure rather than a performance one (C6).
    pub fn clear(&self, db: &Arc<Database>) -> Result<(), LychiError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::HISTORY)?;
            let keys: Vec<String> = table
                .iter()?
                .map(|r| r.map(|(k, _)| k.value().to_string()))
                .collect::<Result<_, _>>()?;
            for key in keys {
                table.remove(key.as_str())?;
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

    /// Delete tombstones left by earlier versions.
    ///
    /// Until 2026-08-05 this store soft-deleted: rows were marked `deleted_at`
    /// and kept forever, and every push deserialized all of them. Measured on a
    /// fresh database, push cost grew linearly — 6.5ms at 500 commands, 35ms at
    /// 3000, extrapolating to 116ms at 10k — while the live set stayed at 500.
    ///
    /// New writes no longer create tombstones, but an existing database still
    /// carries whatever the old code wrote, so it would keep paying. Runs once
    /// at startup; on a database with none it is a single scan and no writes.
    pub fn purge_tombstones(&self, db: &Arc<Database>) -> Result<usize, LychiError> {
        let txn = db.begin_write()?;
        let removed = {
            let mut table = txn.open_table(db::HISTORY)?;
            let doomed: Vec<String> = table
                .iter()?
                .filter_map(|r| {
                    let (key, val) = r.ok()?;
                    let entry: HistoryEntry = postcard::from_bytes(val.value()).ok()?;
                    entry.deleted_at.map(|_| key.value().to_string())
                })
                .collect();
            for key in &doomed {
                table.remove(key.as_str())?;
            }
            doomed.len()
        };
        txn.commit()?;
        if removed > 0 {
            tracing::info!("[history] purged {removed} tombstones from earlier versions");
        }
        Ok(removed)
    }

    /// Trim the table to `max_entries`, deleting the oldest.
    ///
    /// Keys are UUID v7, which sort in creation order, so `table.len()` and the
    /// iteration order are enough — no deserialization is needed to decide what
    /// goes. That matters: this used to deserialize every row on every push.
    fn enforce_max(&self, table: &mut redb::Table<&str, &[u8]>) -> Result<(), LychiError> {
        let len = table.len()? as usize;
        if len <= self.max_entries {
            return Ok(());
        }
        let excess = len - self.max_entries;
        // Oldest first, by key order. Collected before removing because the
        // iterator borrows the table.
        let doomed: Vec<String> = table
            .iter()?
            .take(excess)
            .map(|r| r.map(|(k, _)| k.value().to_string()))
            .collect::<Result<_, _>>()?;
        for key in doomed {
            table.remove(key.as_str())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_test_database;

    /// Measures how `push` scales as tombstones accumulate. Ignored: it is a
    /// measurement, not an assertion.
    ///
    ///     cargo test -p lychi-core --lib history -- --ignored --nocapture
    #[test]
    #[ignore]
    fn measure_push_cost_as_tombstones_accumulate() {
        let db = open_test_database();
        let store = HistoryStore::new(500, true);
        let t0 = std::time::Instant::now();
        for i in 0..3000 {
            store.push(&db, &format!("cmd {i}")).unwrap();
            if (i + 1) % 500 == 0 {
                let t = std::time::Instant::now();
                store.push(&db, "probe command").unwrap();
                let live = store.entries(&db).unwrap().len();
                println!(
                    "  after {:>5} pushes: one push = {:>7}us   live entries = {}",
                    i + 1,
                    t.elapsed().as_micros(),
                    live
                );
            }
        }
        println!("  total for 3000 pushes: {:?}", t0.elapsed());
    }

    /// The bug: history soft-deleted, so the table only ever grew and every
    /// push deserialized all of it. Measured before the fix, push cost rose
    /// linearly (6.5ms at 500 commands, 35ms at 3000) while the live set stayed
    /// at 500. This asserts the table itself stays bounded, which is what makes
    /// the cost flat.
    #[test]
    fn the_table_stays_bounded_not_just_the_live_set() {
        let db = open_test_database();
        let store = HistoryStore::new(10, true);
        for i in 0..200 {
            store.push(&db, &format!("cmd {i}")).unwrap();
        }
        let txn = db.begin_read().unwrap();
        let rows = txn.open_table(db::HISTORY).unwrap().len().unwrap() as usize;
        assert!(
            rows <= 10,
            "table holds {rows} rows for a 10-entry limit — tombstones are back"
        );
        assert_eq!(store.entries(&db).unwrap().len(), 10);
    }

    /// Dedup must REMOVE the previous occurrence, not tombstone it. Tombstoning
    /// meant repeating a command grew the table it scans.
    #[test]
    fn dedup_does_not_grow_the_table() {
        let db = open_test_database();
        let store = HistoryStore::new(500, true);
        for _ in 0..50 {
            store.push(&db, "same command").unwrap();
        }
        let txn = db.begin_read().unwrap();
        let rows = txn.open_table(db::HISTORY).unwrap().len().unwrap() as usize;
        assert_eq!(rows, 1, "50 pushes of one command left {rows} rows");
    }

    /// **Privacy, not performance.** `clear` used to tombstone, so "clear
    /// history" left every command the user had typed in the database, merely
    /// hidden from the list. Clearing must actually delete (C6).
    #[test]
    fn clear_actually_deletes_the_data() {
        let db = open_test_database();
        let store = HistoryStore::new(500, true);
        store.push(&db, "something private").unwrap();
        store.clear(&db).unwrap();

        assert!(store.entries(&db).unwrap().is_empty());
        let txn = db.begin_read().unwrap();
        let rows = txn.open_table(db::HISTORY).unwrap().len().unwrap() as usize;
        assert_eq!(
            rows, 0,
            "cleared history left {rows} rows on disk — the data is still there"
        );
    }

    /// An existing database carries tombstones the old code wrote; the purge
    /// removes them without touching live rows.
    #[test]
    fn purge_removes_legacy_tombstones_only() {
        let db = open_test_database();
        let store = HistoryStore::new(500, true);
        store.push(&db, "keep me").unwrap();

        // Hand-write a tombstone the way the old code did.
        {
            let txn = db.begin_write().unwrap();
            {
                let mut t = txn.open_table(db::HISTORY).unwrap();
                let e = HistoryEntry {
                    command: "old deleted".into(),
                    deleted_at: Some(1),
                    sync_status: SYNC_LOCAL,
                };
                let bytes = postcard::to_allocvec(&e).unwrap();
                t.insert(db::new_id().as_str(), bytes.as_slice()).unwrap();
            }
            txn.commit().unwrap();
        }

        assert_eq!(store.purge_tombstones(&db).unwrap(), 1);
        assert_eq!(store.entries(&db).unwrap(), vec!["keep me"]);
        assert_eq!(
            store.purge_tombstones(&db).unwrap(),
            0,
            "must be idempotent"
        );
    }

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
