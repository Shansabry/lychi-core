use std::sync::Arc;

use redb::{Database, ReadableTable};

use crate::db::{
    self,
    schema::{HistoryEntry, SYNC_LOCAL},
};
use crate::error::LychiError;

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
