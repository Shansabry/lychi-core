use std::sync::Arc;

use redb::{Database, ReadableTable};

use crate::db::{
    self,
    schema::{NoteEntry, SYNC_LOCAL, TodoEntry},
};
use crate::error::LychiError;
use crate::notes::{NoteItem, TodoItem};

/// Maximum number of notes.
pub const MAX_NOTES: usize = 5;

/// Maximum character length per note.
const MAX_NOTE_CHARS: usize = 500;

/// Maximum number of todo items.
const MAX_TODOS: usize = 20;

#[derive(Default)]
pub struct NotesStore;

impl NotesStore {
    pub fn new() -> Self {
        Self
    }

    // ---- Notes ----

    pub fn get_notes(&self, db: &Arc<Database>) -> Result<Vec<NoteItem>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::NOTES)?;
        let mut notes = Vec::new();
        for result in table.iter()? {
            let (key, val) = result?;
            let entry: NoteEntry = postcard::from_bytes(val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if entry.deleted_at.is_none() {
                notes.push(NoteItem {
                    id: key.value().to_string(),
                    text: entry.text,
                    created_at: entry.created_at,
                    updated_at: entry.updated_at,
                });
            }
        }
        Ok(notes)
    }

    pub fn add_note(&self, db: &Arc<Database>, text: &str) -> Result<NoteItem, LychiError> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(LychiError::Notes("Note text cannot be empty".into()));
        }
        if text.len() > MAX_NOTE_CHARS {
            return Err(LychiError::Notes(format!(
                "Note exceeds {MAX_NOTE_CHARS} character limit"
            )));
        }

        // Check note count
        let current_count = self.notes_count(db)?;
        if current_count >= MAX_NOTES {
            return Err(LychiError::Notes(format!(
                "Note limit reached ({MAX_NOTES}/{MAX_NOTES}). Delete a note to make room."
            )));
        }

        let now = db::now_millis();
        let id = db::new_id();
        let entry = NoteEntry {
            text: text.clone(),
            created_at: now,
            updated_at: now,
            deleted_at: None,
            sync_status: SYNC_LOCAL,
        };

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::NOTES)?;
            let bytes =
                postcard::to_allocvec(&entry).map_err(|e| LychiError::Database(e.to_string()))?;
            table.insert(id.as_str(), bytes.as_slice())?;
        }
        txn.commit()?;

        Ok(NoteItem {
            id,
            text,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn update_note(&self, db: &Arc<Database>, id: &str, text: &str) -> Result<(), LychiError> {
        if text.trim().is_empty() {
            return Err(LychiError::Notes("Note text cannot be empty".into()));
        }
        if text.len() > MAX_NOTE_CHARS {
            return Err(LychiError::Notes(format!(
                "Note exceeds {MAX_NOTE_CHARS} character limit"
            )));
        }

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::NOTES)?;
            let existing_val = table
                .get(id)?
                .ok_or_else(|| LychiError::Notes(format!("Note not found: {id}")))?;
            let mut entry: NoteEntry = postcard::from_bytes(existing_val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if entry.deleted_at.is_some() {
                return Err(LychiError::Notes(format!("Note not found: {id}")));
            }
            entry.text = text.to_string();
            entry.updated_at = db::now_millis();
            let bytes =
                postcard::to_allocvec(&entry).map_err(|e| LychiError::Database(e.to_string()))?;
            drop(existing_val);
            table.insert(id, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn delete_note(&self, db: &Arc<Database>, id: &str) -> Result<(), LychiError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::NOTES)?;
            let existing_val = table
                .get(id)?
                .ok_or_else(|| LychiError::Notes(format!("Note not found: {id}")))?;
            let mut entry: NoteEntry = postcard::from_bytes(existing_val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if entry.deleted_at.is_some() {
                return Err(LychiError::Notes(format!("Note not found: {id}")));
            }
            entry.deleted_at = Some(db::now_millis());
            let bytes =
                postcard::to_allocvec(&entry).map_err(|e| LychiError::Database(e.to_string()))?;
            drop(existing_val);
            table.insert(id, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn is_notes_full(&self, db: &Arc<Database>) -> Result<bool, LychiError> {
        Ok(self.notes_count(db)? >= MAX_NOTES)
    }

    pub fn notes_count(&self, db: &Arc<Database>) -> Result<usize, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::NOTES)?;
        let mut count = 0;
        for result in table.iter()? {
            let (_, val) = result?;
            let entry: NoteEntry = postcard::from_bytes(val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if entry.deleted_at.is_none() {
                count += 1;
            }
        }
        Ok(count)
    }

    // ---- Todos ----

    pub fn get_todos(&self, db: &Arc<Database>) -> Result<Vec<TodoItem>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::TODOS)?;
        let mut todos = Vec::new();
        for result in table.iter()? {
            let (key, val) = result?;
            let entry: TodoEntry = postcard::from_bytes(val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if entry.deleted_at.is_none() {
                todos.push(TodoItem {
                    id: key.value().to_string(),
                    text: entry.text,
                    done: entry.done,
                });
            }
        }
        Ok(todos)
    }

    pub fn add_todo(&self, db: &Arc<Database>, text: &str) -> Result<TodoItem, LychiError> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(LychiError::Notes("Todo text cannot be empty".into()));
        }

        // Check todo count
        let current_count = self.todo_count(db)?;
        if current_count >= MAX_TODOS {
            return Err(LychiError::Notes(format!(
                "Maximum of {MAX_TODOS} todos reached"
            )));
        }

        let now = db::now_millis();
        let id = db::new_id();
        let entry = TodoEntry {
            text: text.clone(),
            done: false,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            sync_status: SYNC_LOCAL,
        };

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::TODOS)?;
            let bytes =
                postcard::to_allocvec(&entry).map_err(|e| LychiError::Database(e.to_string()))?;
            table.insert(id.as_str(), bytes.as_slice())?;
        }
        txn.commit()?;

        Ok(TodoItem {
            id,
            text,
            done: false,
        })
    }

    pub fn toggle_todo(&self, db: &Arc<Database>, id: &str) -> Result<(), LychiError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::TODOS)?;
            let existing_val = table
                .get(id)?
                .ok_or_else(|| LychiError::Notes(format!("Todo not found: {id}")))?;
            let mut entry: TodoEntry = postcard::from_bytes(existing_val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if entry.deleted_at.is_some() {
                return Err(LychiError::Notes(format!("Todo not found: {id}")));
            }
            entry.done = !entry.done;
            entry.updated_at = db::now_millis();
            let bytes =
                postcard::to_allocvec(&entry).map_err(|e| LychiError::Database(e.to_string()))?;
            drop(existing_val);
            table.insert(id, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn delete_todo(&self, db: &Arc<Database>, id: &str) -> Result<(), LychiError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::TODOS)?;
            let existing_val = table
                .get(id)?
                .ok_or_else(|| LychiError::Notes(format!("Todo not found: {id}")))?;
            let mut entry: TodoEntry = postcard::from_bytes(existing_val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if entry.deleted_at.is_some() {
                return Err(LychiError::Notes(format!("Todo not found: {id}")));
            }
            entry.deleted_at = Some(db::now_millis());
            let bytes =
                postcard::to_allocvec(&entry).map_err(|e| LychiError::Database(e.to_string()))?;
            drop(existing_val);
            table.insert(id, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    fn todo_count(&self, db: &Arc<Database>) -> Result<usize, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::TODOS)?;
        let mut count = 0;
        for result in table.iter()? {
            let (_, val) = result?;
            let entry: TodoEntry = postcard::from_bytes(val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if entry.deleted_at.is_none() {
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
    fn note_add_and_list() {
        let db = open_test_database();
        let store = NotesStore::new();
        assert!(store.get_notes(&db).unwrap().is_empty());

        let item = store.add_note(&db, "hello world").unwrap();
        let notes = store.get_notes(&db).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].text, "hello world");
        assert!(!item.id.is_empty());
        assert!(item.created_at > 0);
    }

    #[test]
    fn note_char_limit() {
        let db = open_test_database();
        let store = NotesStore::new();
        let long = "x".repeat(501);
        assert!(store.add_note(&db, &long).is_err());
        let exact = "x".repeat(500);
        assert!(store.add_note(&db, &exact).is_ok());
    }

    #[test]
    fn note_limit_reached() {
        let db = open_test_database();
        let store = NotesStore::new();
        for i in 0..MAX_NOTES {
            store.add_note(&db, &format!("note {i}")).unwrap();
        }
        let err = store.add_note(&db, "one too many").unwrap_err();
        assert!(err.to_string().contains("limit reached"));
    }

    #[test]
    fn note_update() {
        let db = open_test_database();
        let store = NotesStore::new();
        let item = store.add_note(&db, "original").unwrap();
        store.update_note(&db, &item.id, "updated").unwrap();
        let notes = store.get_notes(&db).unwrap();
        assert_eq!(notes[0].text, "updated");
    }

    #[test]
    fn note_delete() {
        let db = open_test_database();
        let store = NotesStore::new();
        let item = store.add_note(&db, "to delete").unwrap();
        assert_eq!(store.notes_count(&db).unwrap(), 1);
        store.delete_note(&db, &item.id).unwrap();
        assert_eq!(store.notes_count(&db).unwrap(), 0);
    }

    #[test]
    fn todo_add_toggle_delete() {
        let db = open_test_database();
        let store = NotesStore::new();

        let item = store.add_todo(&db, "Buy milk").unwrap();
        assert!(!item.done);
        assert_eq!(store.get_todos(&db).unwrap().len(), 1);

        store.toggle_todo(&db, &item.id).unwrap();
        assert!(store.get_todos(&db).unwrap()[0].done);

        store.delete_todo(&db, &item.id).unwrap();
        assert!(store.get_todos(&db).unwrap().is_empty());
    }

    #[test]
    fn todo_empty_text_rejected() {
        let db = open_test_database();
        let store = NotesStore::new();
        assert!(store.add_todo(&db, "").is_err());
        assert!(store.add_todo(&db, "   ").is_err());
    }
}
