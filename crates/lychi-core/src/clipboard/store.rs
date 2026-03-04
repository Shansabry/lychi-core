use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata};

use crate::clipboard::ClipboardItem;
use crate::db::{self, schema::ClipboardEntry};
use crate::error::LychiError;

/// Maximum clipboard entries to keep.
const MAX_ENTRIES: u64 = 100;

#[derive(Default)]
pub struct ClipboardStore;

impl ClipboardStore {
    pub fn new() -> Self {
        Self
    }

    /// Get all clipboard entries, most recent first.
    pub fn get_entries(
        &self,
        db: &Arc<Database>,
        limit: usize,
    ) -> Result<Vec<ClipboardItem>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::CLIPBOARD)?;
        let mut items = Vec::new();
        // UUID v7 keys are time-ordered, so iterating gives chronological order.
        // Reverse for most-recent-first.
        for result in table.iter()? {
            let (key, val) = result?;
            let entry: ClipboardEntry = postcard::from_bytes(val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            items.push(ClipboardItem {
                id: key.value().to_string(),
                text: entry.text,
                created_at: entry.created_at,
            });
        }
        items.reverse();
        items.truncate(limit);
        Ok(items)
    }

    /// Add a new clipboard entry. Returns true if it was actually stored (not a duplicate).
    pub fn push(&self, db: &Arc<Database>, text: &str) -> Result<bool, LychiError> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(false);
        }

        // Check if the most recent entry is the same text (avoid duplicates)
        let txn = db.begin_read()?;
        let table = txn.open_table(db::CLIPBOARD)?;
        if let Some(last) = table.iter()?.next_back() {
            let (_key, val) = last?;
            let entry: ClipboardEntry = postcard::from_bytes(val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if entry.text == text {
                return Ok(false); // Duplicate of most recent
            }
        }
        drop(table);
        drop(txn);

        // Insert
        let id = db::new_id();
        let entry = ClipboardEntry {
            text: text.to_string(),
            created_at: db::now_millis(),
        };
        let bytes =
            postcard::to_allocvec(&entry).map_err(|e| LychiError::Database(e.to_string()))?;

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::CLIPBOARD)?;
            table.insert(id.as_str(), bytes.as_slice())?;

            // Prune oldest if over limit
            let len = table.len()?;
            if len > MAX_ENTRIES {
                let to_remove = len - MAX_ENTRIES;
                let mut keys_to_remove = Vec::with_capacity(to_remove as usize);
                for result in table.iter()? {
                    if keys_to_remove.len() >= to_remove as usize {
                        break;
                    }
                    let (key, _) = result?;
                    keys_to_remove.push(key.value().to_string());
                }
                for key in &keys_to_remove {
                    table.remove(key.as_str())?;
                }
            }
        }
        txn.commit()?;

        Ok(true)
    }

    /// Get the number of clipboard entries.
    pub fn count(&self, db: &Arc<Database>) -> Result<u64, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::CLIPBOARD)?;
        Ok(table.len()?)
    }

    /// Clear all clipboard history.
    pub fn clear(&self, db: &Arc<Database>) -> Result<(), LychiError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::CLIPBOARD)?;
            // Drain all entries
            let keys: Vec<String> = table
                .iter()?
                .map(|r| r.map(|(k, _)| k.value().to_string()))
                .collect::<Result<_, _>>()?;
            for key in &keys {
                table.remove(key.as_str())?;
            }
        }
        txn.commit()?;
        Ok(())
    }
}

/// Hash text for quick duplicate comparison in the background monitor.
pub fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Background clipboard monitor — polls system clipboard every 500ms and stores new entries.
/// Runs on a dedicated OS thread until `running` is set to false.
/// Automatically recovers from panics (logs and restarts the poll loop).
pub fn run_clipboard_monitor(db: Arc<Database>, running: Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::Ordering;
    tracing::info!("Clipboard monitor started");

    loop {
        if !running.load(Ordering::Relaxed) {
            break;
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            clipboard_monitor_loop(&db, &running);
        }));

        if let Err(_panic) = result {
            tracing::error!(
                "Clipboard monitor panicked — restarting in 1s \
                 (clipboard history may have a gap)"
            );
            std::thread::sleep(std::time::Duration::from_secs(1));
        } else {
            // Loop returned normally — `running` is false, exit cleanly
            break;
        }
    }
    tracing::info!("Clipboard monitor stopped");
}

fn clipboard_monitor_loop(db: &Arc<Database>, running: &Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::Ordering;
    let store = ClipboardStore::new();
    let mut last_hash: u64 = 0;

    // Initialize with current clipboard content hash (don't store what's already there)
    if let Ok(mut cb) = arboard::Clipboard::new()
        && let Ok(text) = cb.get_text()
    {
        last_hash = hash_text(&text);
    }

    while running.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(500));

        let text = match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
            Ok(t) => t,
            Err(_) => continue, // Clipboard unavailable or contains non-text
        };

        let text = text.trim();
        if text.is_empty() {
            continue;
        }

        let current_hash = hash_text(text);
        if current_hash == last_hash {
            continue;
        }
        last_hash = current_hash;

        if let Err(e) = store.push(db, text) {
            tracing::warn!("Clipboard store error: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_and_get() {
        let db = crate::db::open_test_database();
        let store = ClipboardStore::new();

        // Push some entries
        assert!(store.push(&db, "hello").unwrap());
        assert!(store.push(&db, "world").unwrap());
        assert!(store.push(&db, "foo").unwrap());

        // Get entries (most recent first)
        let entries = store.get_entries(&db, 10).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].text, "foo");
        assert_eq!(entries[1].text, "world");
        assert_eq!(entries[2].text, "hello");
    }

    #[test]
    fn test_duplicate_rejection() {
        let db = crate::db::open_test_database();
        let store = ClipboardStore::new();

        assert!(store.push(&db, "hello").unwrap());
        assert!(!store.push(&db, "hello").unwrap()); // Duplicate
        assert!(store.push(&db, "world").unwrap()); // Different
        assert!(!store.push(&db, "world").unwrap()); // Duplicate again

        assert_eq!(store.count(&db).unwrap(), 2);
    }

    #[test]
    fn test_empty_text_rejected() {
        let db = crate::db::open_test_database();
        let store = ClipboardStore::new();

        assert!(!store.push(&db, "").unwrap());
        assert!(!store.push(&db, "   ").unwrap());
        assert_eq!(store.count(&db).unwrap(), 0);
    }

    #[test]
    fn test_clear() {
        let db = crate::db::open_test_database();
        let store = ClipboardStore::new();

        store.push(&db, "a").unwrap();
        store.push(&db, "b").unwrap();
        assert_eq!(store.count(&db).unwrap(), 2);

        store.clear(&db).unwrap();
        assert_eq!(store.count(&db).unwrap(), 0);
    }

    #[test]
    fn test_limit() {
        let db = crate::db::open_test_database();
        let store = ClipboardStore::new();

        for i in 0..5 {
            store.push(&db, &format!("entry {i}")).unwrap();
        }

        let entries = store.get_entries(&db, 3).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].text, "entry 4"); // Most recent
    }
}
