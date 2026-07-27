pub mod files;
pub mod image_utils;

/// Read one ambient context source for an `@clipboard` / `@selection` prompt
/// reference. Lives here (not in the Tauri layer) because it needs `arboard`,
/// which is a core dependency — `src-tauri` stays a thin bridge.
///
/// Returns `None` when the source is empty or unreadable; the expander reports
/// that inline rather than answering about nothing.
pub fn read_context_source(
    src: crate::files::text_extract::ContextSource,
    is_wayland: bool,
) -> Option<String> {
    use crate::files::text_extract::ContextSource;
    match src {
        ContextSource::Clipboard => {
            let mut cb = arboard::Clipboard::new().ok()?;
            cb.get_text().ok()
        }
        ContextSource::Selection => selection::read_primary_selection(is_wayland),
    }
}
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
