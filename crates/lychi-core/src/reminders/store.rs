use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable};

use crate::db::{
    self,
    schema::{ReminderEntry, SYNC_LOCAL},
};
use crate::error::LychiError;
use crate::reminders::ReminderItem;

/// Maximum number of active (non-deleted, non-fired) reminders.
const MAX_REMINDERS: usize = 50;

#[derive(Default)]
pub struct RemindersStore;

impl RemindersStore {
    pub fn new() -> Self {
        Self
    }

    /// Add a new reminder. Returns the created item.
    pub fn add_reminder(
        &self,
        db: &Arc<Database>,
        text: &str,
        due_at: u64,
    ) -> Result<ReminderItem, LychiError> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(LychiError::Database("Reminder text cannot be empty".into()));
        }

        let active = self.active_count(db)?;
        if active >= MAX_REMINDERS {
            return Err(LychiError::Database(format!(
                "Reminder limit reached ({MAX_REMINDERS}/{MAX_REMINDERS})"
            )));
        }

        let now = db::now_millis();
        let id = db::new_id();
        let entry = ReminderEntry {
            text: text.clone(),
            due_at,
            fired: false,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            sync_status: SYNC_LOCAL,
        };

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::REMINDERS)?;
            let bytes = crate::db::encode_row(&entry)?;
            table.insert(id.as_str(), bytes.as_slice())?;
        }
        txn.commit()?;

        Ok(ReminderItem {
            id,
            text,
            due_at,
            fired: false,
            created_at: now,
        })
    }

    /// List all non-deleted reminders, sorted by due_at ascending.
    pub fn list_reminders(&self, db: &Arc<Database>) -> Result<Vec<ReminderItem>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::REMINDERS)?;
        let mut items = Vec::new();
        for result in table.iter()? {
            let (key, val) = result?;
            // One unreadable row must not hide the rest of the list.
            let Some(entry) =
                db::decode_row::<ReminderEntry>("reminders", key.value(), val.value())
            else {
                continue;
            };
            if entry.deleted_at.is_none() {
                items.push(ReminderItem {
                    id: key.value().to_string(),
                    text: entry.text,
                    due_at: entry.due_at,
                    fired: entry.fired,
                    created_at: entry.created_at,
                });
            }
        }
        items.sort_by_key(|r| r.due_at);
        Ok(items)
    }

    /// Soft-delete a reminder.
    pub fn delete_reminder(&self, db: &Arc<Database>, id: &str) -> Result<(), LychiError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::REMINDERS)?;
            let existing_val = table
                .get(id)?
                .ok_or_else(|| LychiError::Database(format!("Reminder not found: {id}")))?;
            let mut entry: ReminderEntry = crate::db::decode_value(existing_val.value())?;
            if entry.deleted_at.is_some() {
                return Err(LychiError::Database(format!("Reminder not found: {id}")));
            }
            entry.deleted_at = Some(db::now_millis());
            let bytes = crate::db::encode_row(&entry)?;
            drop(existing_val);
            table.insert(id, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Get all pending reminders (due, not fired, not deleted).
    pub fn get_pending(
        &self,
        db: &Arc<Database>,
    ) -> Result<Vec<(String, ReminderEntry)>, LychiError> {
        let now = db::now_millis();
        let txn = db.begin_read()?;
        let table = txn.open_table(db::REMINDERS)?;
        let mut pending = Vec::new();
        for result in table.iter()? {
            let (key, val) = result?;
            // One unreadable row must not hide the rest of the list.
            let Some(entry) =
                db::decode_row::<ReminderEntry>("reminders", key.value(), val.value())
            else {
                continue;
            };
            if !entry.fired && entry.deleted_at.is_none() && entry.due_at <= now {
                pending.push((key.value().to_string(), entry));
            }
        }
        Ok(pending)
    }

    /// Mark a reminder as fired.
    pub fn mark_fired(&self, db: &Arc<Database>, id: &str) -> Result<(), LychiError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::REMINDERS)?;
            let existing_val = table
                .get(id)?
                .ok_or_else(|| LychiError::Database(format!("Reminder not found: {id}")))?;
            let mut entry: ReminderEntry = crate::db::decode_value(existing_val.value())?;
            entry.fired = true;
            entry.updated_at = db::now_millis();
            let bytes = crate::db::encode_row(&entry)?;
            drop(existing_val);
            table.insert(id, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    fn active_count(&self, db: &Arc<Database>) -> Result<usize, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::REMINDERS)?;
        let mut count = 0;
        for result in table.iter()? {
            let (_, val) = result?;
            // One unreadable row must not hide the rest of the list.
            let Some(entry) = db::decode_row::<ReminderEntry>("reminders", "?", val.value()) else {
                continue;
            };
            if entry.deleted_at.is_none() && !entry.fired {
                count += 1;
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_test_database;

    #[test]
    fn add_and_list() {
        let db = open_test_database();
        let store = RemindersStore::new();
        assert!(store.list_reminders(&db).unwrap().is_empty());

        let due = db::now_millis() + 60_000;
        let item = store.add_reminder(&db, "buy milk", due).unwrap();
        assert_eq!(item.text, "buy milk");
        assert_eq!(item.due_at, due);
        assert!(!item.fired);

        let list = store.list_reminders(&db).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].text, "buy milk");
    }

    #[test]
    fn delete_reminder() {
        let db = open_test_database();
        let store = RemindersStore::new();
        let due = db::now_millis() + 60_000;
        let item = store.add_reminder(&db, "test", due).unwrap();

        store.delete_reminder(&db, &item.id).unwrap();
        assert!(store.list_reminders(&db).unwrap().is_empty());
    }

    #[test]
    fn pending_and_fire() {
        let db = open_test_database();
        let store = RemindersStore::new();

        // Due in the past → should be pending
        let past = db::now_millis().saturating_sub(1000);
        let item = store.add_reminder(&db, "overdue", past).unwrap();

        let pending = store.get_pending(&db).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, item.id);

        store.mark_fired(&db, &item.id).unwrap();
        let pending = store.get_pending(&db).unwrap();
        assert!(pending.is_empty());

        // Should still be in list, just fired
        let list = store.list_reminders(&db).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].fired);
    }

    #[test]
    fn empty_text_rejected() {
        let db = open_test_database();
        let store = RemindersStore::new();
        assert!(store.add_reminder(&db, "", db::now_millis()).is_err());
        assert!(store.add_reminder(&db, "   ", db::now_millis()).is_err());
    }
}
