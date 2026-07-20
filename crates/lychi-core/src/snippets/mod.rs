pub mod store;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct SnippetItem {
    pub id: String,
    pub name: String,
    pub body: String,
    pub created_at: u64,
    pub updated_at: u64,
}
