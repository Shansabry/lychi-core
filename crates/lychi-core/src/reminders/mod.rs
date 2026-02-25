pub mod monitor;
pub mod store;
pub mod time_parse;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReminderItem {
    pub id: String,
    pub text: String,
    pub due_at: u64,
    pub fired: bool,
    pub created_at: u64,
}
