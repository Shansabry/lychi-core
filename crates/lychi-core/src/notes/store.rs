use std::fs;
use std::path::PathBuf;

use crate::error::LychiError;
use crate::notes::{NotesData, TodoItem};

/// Maximum character length for the note.
const MAX_NOTE_CHARS: usize = 500;

/// Maximum number of todo items.
const MAX_TODOS: usize = 100;

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

        Ok(Self {
            data,
            path: path.clone(),
        })
    }

    pub fn get_note(&self) -> &str {
        &self.data.note
    }

    pub fn set_note(&mut self, text: &str) -> Result<(), LychiError> {
        if text.len() > MAX_NOTE_CHARS {
            return Err(LychiError::Notes(format!(
                "Note exceeds {MAX_NOTE_CHARS} character limit"
            )));
        }
        self.data.note = text.to_string();
        self.save()
    }

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

    fn save(&self) -> Result<(), LychiError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.data)?;
        fs::write(&self.path, json)?;
        Ok(())
    }
}

/// Generate a short random hex ID (6 chars).
fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
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
    fn note_set_and_get() {
        let path = temp_path();
        let mut store = NotesStore::load_or_create(&path).unwrap();
        assert_eq!(store.get_note(), "");
        store.set_note("hello world").unwrap();
        assert_eq!(store.get_note(), "hello world");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn note_char_limit() {
        let path = temp_path();
        let mut store = NotesStore::load_or_create(&path).unwrap();
        let long = "x".repeat(501);
        assert!(store.set_note(&long).is_err());
        let exact = "x".repeat(500);
        assert!(store.set_note(&exact).is_ok());
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
            store.set_note("persistent note").unwrap();
            store.add_todo("persistent todo").unwrap();
        }
        {
            let store = NotesStore::load_or_create(&path).unwrap();
            assert_eq!(store.get_note(), "persistent note");
            assert_eq!(store.get_todos().len(), 1);
            assert_eq!(store.get_todos()[0].text, "persistent todo");
        }
        let _ = fs::remove_file(&path);
    }
}
