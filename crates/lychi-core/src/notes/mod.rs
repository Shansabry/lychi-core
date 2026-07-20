pub mod store;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct NoteItem {
    pub id: String,
    pub text: String,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct TodoItem {
    pub id: String,
    pub text: String,
    pub done: bool,
}

/// A single item in the unified Notes surface. A plain note has `done: None`;
/// a checklist line has `done: Some(true|false)`. The two are stored in
/// separate on-disk tables (NOTES / TODOS) for backwards compatibility, but are
/// merged into this one type at the API boundary so the UI shows a single list.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ScratchItem {
    pub id: String,
    pub text: String,
    /// None = plain note; Some = checklist line (checked/unchecked).
    pub done: Option<bool>,
    pub created_at: u64,
    pub updated_at: u64,
}
