//! AI conversation history — persist completed agent conversations so they can
//! be recalled and continued (Phase 4, "the assistant remembers").
//!
//! The one-box surface stays fast-and-forget (each summon starts fresh), but
//! nothing is lost: every completed conversation is upserted here, and a recall
//! entry point (the `chat` keyword) lists recent threads to pick from and
//! continue. Persistence is nearly free — the message array already lives in
//! memory as a `Session`; this just serializes it.

pub mod store;

use serde::{Deserialize, Serialize};

use crate::providers::ChatMessage;

/// A persisted conversation. `messages` is the full `Session` history (system +
/// user/assistant/tool turns), so recall can continue it exactly.
///
/// `turn_count` is stored (not derived on read) so the recall LIST can be built
/// from a summary-shaped deserialize that skips `messages` entirely — listing
/// after every agent turn used to fully parse every conversation's bodies and
/// inline base64 images. Field order matters: `messages` sits AFTER the summary
/// fields so a partial serde struct that omits it still maps the rest by name.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct Conversation {
    pub id: String,
    /// A short title for the recall list — the first user line, truncated.
    pub title: String,
    /// Number of user+assistant turns. Persisted so `list`/`prune` never touch
    /// `messages`. `#[serde(default)]`: rows written before this field existed
    /// decode to 0, and are corrected on their next upsert.
    #[serde(default)]
    pub turn_count: u32,
    pub created_at: u64,
    pub updated_at: u64,
    pub messages: Vec<ChatMessage>,
}

/// The summary view of a persisted row — the same JSON, deserialized WITHOUT the
/// `messages` array. serde populates the named fields it finds and ignores the
/// rest, so `messages` (and its base64 image payloads) is never allocated. This
/// is what makes `list`/`prune` genuinely cheap.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ConversationMeta {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub turn_count: u32,
    #[serde(default)]
    pub created_at: u64,
    pub updated_at: u64,
}

/// A lightweight row for the recall list — no message bodies, so listing is cheap.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ConversationSummary {
    pub id: String,
    pub title: String,
    /// Number of user+assistant turns (excludes the system prompt & tool msgs).
    pub turn_count: u32,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Derive a title from the first user message: trimmed, single-line, truncated.
pub fn derive_title(messages: &[ChatMessage]) -> String {
    use crate::providers::Role;
    let first_user = messages
        .iter()
        .find(|m| m.role == Role::User)
        .map(|m| m.content_text())
        .unwrap_or_default();
    let line = first_user.lines().next().unwrap_or("").trim();
    const MAX: usize = 60;
    if line.chars().count() > MAX {
        let truncated: String = line.chars().take(MAX).collect();
        format!("{truncated}…")
    } else if line.is_empty() {
        "Conversation".to_string()
    } else {
        line.to_string()
    }
}
