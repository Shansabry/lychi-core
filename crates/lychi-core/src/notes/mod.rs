pub mod store;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteItem {
    pub id: String,
    pub text: String,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub text: String,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotesData {
    /// Legacy single-note field — read during migration, never written back.
    #[serde(default, skip_serializing)]
    note: String,
    #[serde(default)]
    pub notes: Vec<NoteItem>,
    #[serde(default)]
    pub todos: Vec<TodoItem>,
}

impl NotesData {
    /// Migrate legacy single-note to multi-note format.
    /// Returns `true` if migration occurred and a save is needed.
    pub fn migrate_legacy_note(&mut self) -> bool {
        if !self.note.is_empty() && self.notes.is_empty() {
            self.notes.push(NoteItem {
                id: crate::notes::store::generate_id(),
                text: self.note.clone(),
                created_at: 0,
                updated_at: 0,
            });
            self.note.clear();
            true
        } else {
            false
        }
    }
}
