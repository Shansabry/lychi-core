use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::LychiError;
use crate::notes::{NoteItem, NotesData, TodoItem};

/// Maximum number of notes.
pub const MAX_NOTES: usize = 5;

/// Maximum character length per note.
const MAX_NOTE_CHARS: usize = 500;

/// Maximum number of todo items.
const MAX_TODOS: usize = 20;

pub struct NotesStore {
    data: NotesData,
    path: PathBuf,
}

impl NotesStore {
    pub fn load_or_create(path: &PathBuf) -> Result<Self, LychiError> {
        let data = if path.exists() {
            let content = fs::read_to_string(path)?;
            serde_json::from_str(&content).unwrap_or_else(|e| {
                tracing::warn!(
                    "Corrupt notes file {}: {e} — starting fresh",
                    path.display()
                );
                NotesData::default()
            })
        } else {
            NotesData::default()
        };

        let mut store = Self {
            data,
            path: path.clone(),
        };

        // Migrate legacy single-note format
        if store.data.migrate_legacy_note() {
            tracing::info!("Migrated legacy single-note to multi-note format");
            let _ = store.save();
        }

        Ok(store)
    }

    // ---- Notes ----

    pub fn get_notes(&self) -> &[NoteItem] {
        &self.data.notes
    }

    pub fn add_note(&mut self, text: &str) -> Result<NoteItem, LychiError> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(LychiError::Notes("Note text cannot be empty".into()));
        }
        if text.len() > MAX_NOTE_CHARS {
            return Err(LychiError::Notes(format!(
                "Note exceeds {MAX_NOTE_CHARS} character limit"
            )));
        }
        if self.data.notes.len() >= MAX_NOTES {
            return Err(LychiError::Notes(format!(
                "Note limit reached ({MAX_NOTES}/{MAX_NOTES}). Delete a note to make room."
            )));
        }

        let now = now_millis();
        let item = NoteItem {
            id: generate_id(),
            text,
            created_at: now,
            updated_at: now,
        };
        self.data.notes.push(item.clone());
        self.save()?;
        Ok(item)
    }

    pub fn update_note(&mut self, id: &str, text: &str) -> Result<(), LychiError> {
        if text.trim().is_empty() {
            return Err(LychiError::Notes("Note text cannot be empty".into()));
        }
        if text.len() > MAX_NOTE_CHARS {
            return Err(LychiError::Notes(format!(
                "Note exceeds {MAX_NOTE_CHARS} character limit"
            )));
        }
        let item = self
            .data
            .notes
            .iter_mut()
            .find(|n| n.id == id)
            .ok_or_else(|| LychiError::Notes(format!("Note not found: {id}")))?;
        item.text = text.to_string();
        item.updated_at = now_millis();
        self.save()
    }

    pub fn delete_note(&mut self, id: &str) -> Result<(), LychiError> {
        let len_before = self.data.notes.len();
        self.data.notes.retain(|n| n.id != id);
        if self.data.notes.len() == len_before {
            return Err(LychiError::Notes(format!("Note not found: {id}")));
        }
        self.save()
    }

    pub fn is_notes_full(&self) -> bool {
        self.data.notes.len() >= MAX_NOTES
    }

    pub fn notes_count(&self) -> usize {
        self.data.notes.len()
    }

    // ---- Todos ----

    pub fn get_todos(&self) -> &[TodoItem] {
        &self.data.todos
    }

    pub fn add_todo(&mut self, text: &str) -> Result<TodoItem, LychiError> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(LychiError::Notes("Todo text cannot be empty".into()));
        }
        if self.data.todos.len() >= MAX_TODOS {
            return Err(LychiError::Notes(format!(
                "Maximum of {MAX_TODOS} todos reached"
            )));
        }

        let item = TodoItem {
            id: generate_id(),
            text,
            done: false,
        };
        self.data.todos.push(item.clone());
        self.save()?;
        Ok(item)
    }

    pub fn toggle_todo(&mut self, id: &str) -> Result<(), LychiError> {
        let item = self
            .data
            .todos
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| LychiError::Notes(format!("Todo not found: {id}")))?;
        item.done = !item.done;
        self.save()
    }

    pub fn delete_todo(&mut self, id: &str) -> Result<(), LychiError> {
        let len_before = self.data.todos.len();
        self.data.todos.retain(|t| t.id != id);
        if self.data.todos.len() == len_before {
            return Err(LychiError::Notes(format!("Todo not found: {id}")));
        }
        self.save()
    }

    // ---- Persistence ----

    fn save(&self) -> Result<(), LychiError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.data)?;
        fs::write(&self.path, json)?;
        Ok(())
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Generate a short random hex ID (6 chars).
pub fn generate_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let pid = std::process::id();
    format!("{:06x}", (nanos ^ pid) & 0xFFFFFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        env::temp_dir().join(format!("lychi-test-notes-{}-{id}.json", std::process::id()))
    }

    #[test]
    fn note_add_and_list() {
        let path = temp_path();
        let mut store = NotesStore::load_or_create(&path).unwrap();
        assert!(store.get_notes().is_empty());

        let item = store.add_note("hello world").unwrap();
        assert_eq!(store.get_notes().len(), 1);
        assert_eq!(store.get_notes()[0].text, "hello world");
        assert!(!item.id.is_empty());
        assert!(item.created_at > 0);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn note_char_limit() {
        let path = temp_path();
        let mut store = NotesStore::load_or_create(&path).unwrap();
        let long = "x".repeat(501);
        assert!(store.add_note(&long).is_err());
        let exact = "x".repeat(500);
        assert!(store.add_note(&exact).is_ok());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn note_limit_reached() {
        let path = temp_path();
        let mut store = NotesStore::load_or_create(&path).unwrap();
        for i in 0..MAX_NOTES {
            store.add_note(&format!("note {i}")).unwrap();
        }
        let err = store.add_note("one too many").unwrap_err();
        assert!(err.to_string().contains("limit reached"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn note_update() {
        let path = temp_path();
        let mut store = NotesStore::load_or_create(&path).unwrap();
        let item = store.add_note("original").unwrap();
        store.update_note(&item.id, "updated").unwrap();
        assert_eq!(store.get_notes()[0].text, "updated");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn note_delete() {
        let path = temp_path();
        let mut store = NotesStore::load_or_create(&path).unwrap();
        let item = store.add_note("to delete").unwrap();
        assert_eq!(store.notes_count(), 1);
        store.delete_note(&item.id).unwrap();
        assert_eq!(store.notes_count(), 0);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn note_migration_from_legacy() {
        let path = temp_path();
        // Write legacy format with single "note" field
        let legacy = r#"{"note": "my old note", "todos": []}"#;
        fs::write(&path, legacy).unwrap();

        let store = NotesStore::load_or_create(&path).unwrap();
        assert_eq!(store.get_notes().len(), 1);
        assert_eq!(store.get_notes()[0].text, "my old note");

        // Verify the saved file no longer has the legacy field
        let saved = fs::read_to_string(&path).unwrap();
        let data: serde_json::Value = serde_json::from_str(&saved).unwrap();
        assert!(data.get("note").is_none());
        assert!(data.get("notes").is_some());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn todo_add_toggle_delete() {
        let path = temp_path();
        let mut store = NotesStore::load_or_create(&path).unwrap();

        let item = store.add_todo("Buy milk").unwrap();
        assert!(!item.done);
        assert_eq!(store.get_todos().len(), 1);

        store.toggle_todo(&item.id).unwrap();
        assert!(store.get_todos()[0].done);

        store.delete_todo(&item.id).unwrap();
        assert!(store.get_todos().is_empty());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn todo_empty_text_rejected() {
        let path = temp_path();
        let mut store = NotesStore::load_or_create(&path).unwrap();
        assert!(store.add_todo("").is_err());
        assert!(store.add_todo("   ").is_err());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn persistence() {
        let path = temp_path();
        {
            let mut store = NotesStore::load_or_create(&path).unwrap();
            store.add_note("persistent note").unwrap();
            store.add_todo("persistent todo").unwrap();
        }
        {
            let store = NotesStore::load_or_create(&path).unwrap();
            assert_eq!(store.get_notes().len(), 1);
            assert_eq!(store.get_notes()[0].text, "persistent note");
            assert_eq!(store.get_todos().len(), 1);
            assert_eq!(store.get_todos()[0].text, "persistent todo");
        }
        let _ = fs::remove_file(&path);
    }
}
