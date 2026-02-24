pub mod store;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliasItem {
    pub name: String,
    pub command: String,
    pub created_at: u64,
    pub updated_at: u64,
}
