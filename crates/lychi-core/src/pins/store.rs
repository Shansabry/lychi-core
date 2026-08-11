use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable};

use crate::db::{
    self,
    schema::{PinEntry, SYNC_LOCAL},
};
use crate::error::LychiError;
use crate::pins::{PinItem, normalize_run};

/// Deliberately small: pins are the rows the user wants WITHOUT scrolling, and
/// the zero state's whole budget is about that many rows. A larger cap would
/// just recreate the unranked wall of text pins exist to replace.
pub const MAX_PINS: usize = 8;
pub const MAX_LABEL: usize = 80;
pub const MAX_RUN: usize = 500;

#[derive(Default)]
pub struct PinsStore;

impl PinsStore {
    pub fn new() -> Self {
        Self
    }

    /// Active pins in display order.
    pub fn list(&self, db: &Arc<Database>) -> Result<Vec<PinItem>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::PINS)?;
        let mut pins = Vec::new();
        for result in table.iter()? {
            let (key, val) = result?;
            // One unreadable row must not hide the rest of the list.
            let Some(entry) = db::decode_row::<PinEntry>("pins", key.value(), val.value()) else {
                continue;
            };
            if entry.deleted_at.is_none() {
                pins.push(PinItem {
                    run: entry.run,
                    label: entry.label,
                    position: entry.position,
                    created_at: entry.created_at,
                });
            }
        }
        pins.sort_by_key(|p| p.position);
        Ok(pins)
    }

    /// Pin a command. Idempotent upsert: re-pinning an existing (or previously
    /// unpinned) run updates its label and moves it to the end — the ⌘K action
    /// cannot know reliably whether a typed row is already pinned, so a double
    /// pin must be harmless, never an error.
    pub fn add(&self, db: &Arc<Database>, run: &str, label: &str) -> Result<PinItem, LychiError> {
        let run = run.trim().to_string();
        let label = label.trim().to_string();
        if run.is_empty() {
            return Err(LychiError::Pin("Cannot pin an empty command".into()));
        }
        if run.len() > MAX_RUN {
            return Err(LychiError::Pin(format!(
                "Pinned command exceeds {MAX_RUN} character limit"
            )));
        }
        if label.is_empty() {
            return Err(LychiError::Pin("Pin label cannot be empty".into()));
        }
        if label.len() > MAX_LABEL {
            return Err(LychiError::Pin(format!(
                "Pin label exceeds {MAX_LABEL} character limit"
            )));
        }

        let key = normalize_run(&run);
        let existing = self.list(db)?;
        let already_pinned = existing.iter().any(|p| normalize_run(&p.run) == key);
        if !already_pinned && existing.len() >= MAX_PINS {
            return Err(LychiError::Pin(format!(
                "Pin limit reached ({MAX_PINS}/{MAX_PINS}). Unpin one to make room."
            )));
        }
        let next_position = existing.iter().map(|p| p.position + 1).max().unwrap_or(0);

        let now = db::now_millis();
        let entry = PinEntry {
            run: run.clone(),
            label: label.clone(),
            position: next_position,
            created_at: now,
            deleted_at: None,
            sync_status: SYNC_LOCAL,
        };

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::PINS)?;
            let bytes = db::encode_row(&entry)?;
            table.insert(key.as_str(), bytes.as_slice())?;
        }
        txn.commit()?;

        Ok(PinItem {
            run,
            label,
            position: next_position,
            created_at: now,
        })
    }

    /// Unpin by run string (any casing/spacing). Soft delete, mirroring the
    /// other stores; missing pins are a no-op, not an error.
    pub fn remove(&self, db: &Arc<Database>, run: &str) -> Result<(), LychiError> {
        let key = normalize_run(run);
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::PINS)?;
            let existing = match table.get(key.as_str())? {
                Some(val) => db::decode_value::<PinEntry>(val.value()).ok(),
                None => None,
            };
            if let Some(mut entry) = existing {
                entry.deleted_at = Some(db::now_millis());
                let bytes = db::encode_row(&entry)?;
                table.insert(key.as_str(), bytes.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    pub fn is_pinned(&self, db: &Arc<Database>, run: &str) -> Result<bool, LychiError> {
        let key = normalize_run(run);
        let txn = db.begin_read()?;
        let table = txn.open_table(db::PINS)?;
        Ok(match table.get(key.as_str())? {
            Some(val) => db::decode_value::<PinEntry>(val.value())
                .map(|e: PinEntry| e.deleted_at.is_none())
                .unwrap_or(false),
            None => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_database;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_DB_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn test_db() -> Arc<Database> {
        let n = TEST_DB_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("lychi-pins-test-{}-{n}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        open_database(&path).unwrap()
    }

    #[test]
    fn pins_list_in_pin_order_not_key_order() {
        let db = test_db();
        let store = PinsStore::new();
        // "z" sorts after "a" lexicographically; pin order must win.
        store.add(&db, "z-last-key", "Zeta").unwrap();
        store.add(&db, "a-first-key", "Alpha").unwrap();
        let pins = store.list(&db).unwrap();
        assert_eq!(
            pins.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(),
            vec!["Zeta", "Alpha"]
        );
    }

    #[test]
    fn re_pinning_updates_in_place_and_moves_to_end() {
        let db = test_db();
        let store = PinsStore::new();
        store.add(&db, "open Spotify", "Spotify").unwrap();
        store.add(&db, "cargo test", "cargo test").unwrap();
        // Re-pin with different spacing/casing: same pin, new position.
        store.add(&db, "Open  spotify", "Spotify Music").unwrap();
        let pins = store.list(&db).unwrap();
        assert_eq!(pins.len(), 2, "re-pin must not duplicate");
        assert_eq!(pins[1].label, "Spotify Music");
        assert_eq!(pins[1].run, "Open  spotify");
    }

    #[test]
    fn the_cap_refuses_a_ninth_pin_but_allows_re_pinning() {
        let db = test_db();
        let store = PinsStore::new();
        for i in 0..MAX_PINS {
            store
                .add(&db, &format!("cmd {i}"), &format!("Cmd {i}"))
                .unwrap();
        }
        let err = store.add(&db, "one more", "One More").unwrap_err();
        assert!(matches!(err, LychiError::Pin(_)));
        // Re-pinning an existing run is not "one more".
        store.add(&db, "cmd 0", "Cmd Zero").unwrap();
    }

    #[test]
    fn remove_soft_deletes_and_re_add_resurrects() {
        let db = test_db();
        let store = PinsStore::new();
        store.add(&db, "open Firefox", "Firefox").unwrap();
        store.remove(&db, "OPEN FIREFOX").unwrap();
        assert!(store.list(&db).unwrap().is_empty());
        assert!(!store.is_pinned(&db, "open firefox").unwrap());
        // Removing again (or removing the never-pinned) is a no-op.
        store.remove(&db, "open Firefox").unwrap();
        store.add(&db, "open Firefox", "Firefox").unwrap();
        assert!(store.is_pinned(&db, "open  firefox ").unwrap());
    }

    #[test]
    fn validation_refuses_empty_and_oversized() {
        let db = test_db();
        let store = PinsStore::new();
        assert!(store.add(&db, "  ", "Label").is_err());
        assert!(store.add(&db, "run x", " ").is_err());
        assert!(store.add(&db, &"x".repeat(MAX_RUN + 1), "L").is_err());
        assert!(store.add(&db, "run x", &"y".repeat(MAX_LABEL + 1)).is_err());
    }

    #[test]
    fn normalize_collapses_whitespace_and_case() {
        assert_eq!(normalize_run("  Open   Spotify "), "open spotify");
        assert_eq!(normalize_run("open spotify"), "open spotify");
    }
}
