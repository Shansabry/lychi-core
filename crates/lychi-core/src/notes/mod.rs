pub mod store;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub text: String,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotesData {
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub todos: Vec<TodoItem>,
}
