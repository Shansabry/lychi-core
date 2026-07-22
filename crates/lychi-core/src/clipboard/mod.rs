pub mod image_utils;
pub mod selection;
pub mod store;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardItem {
    pub id: String,
    pub text: String,
    pub created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<ClipboardImageInfo>,
}

/// Image metadata sent to frontend via IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardImageInfo {
    pub width: u32,
    pub height: u32,
    pub thumb_b64: String,
}
