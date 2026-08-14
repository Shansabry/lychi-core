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
    /// The invoking AI-command (preset) instruction when this conversation was
    /// started by a preset, e.g. `Summarize the following…`. `None` for an
    /// ordinary chat. Persisted so the recall list can badge preset runs without
    /// reading `messages`. `#[serde(default)]`: pre-field rows decode to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_label: Option<String>,
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
    pub preset_label: Option<String>,
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
    /// The AI-command label when this was a preset run (`Summarize the following…`),
    /// else `None`. Lets the recall list badge "which command" beside the title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_label: Option<String>,
    /// Number of user+assistant turns (excludes the system prompt & tool msgs).
    pub turn_count: u32,
    pub created_at: u64,
    pub updated_at: u64,
}

/// The AI-command label for a conversation, or `None` for an ordinary chat.
///
/// A preset user turn is marked by its `display` split (a typed field, not a
/// prose guess); its `instruction` is the command line the bubble shows
/// ("Summarize the following…"). That is exactly the "which command" caption the
/// recall list badges beside the answer-derived title.
pub fn derive_preset_label(messages: &[ChatMessage]) -> Option<String> {
    use crate::providers::Role;
    messages
        .iter()
        .find(|m| m.role == Role::User)
        .and_then(|m| m.display.as_ref())
        .map(|d| d.instruction.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Derive a title for the recall list.
///
/// For an ordinary chat the first user line IS the user's own question, so it
/// makes the clearest title. For an AI command (preset) the first user message
/// is the rendered TEMPLATE ("Summarize the following text…") — identical across
/// every summarize, and useless for telling two runs apart. A preset user turn
/// is marked by its `display` split (the typed field, not a prose guess), so in
/// that case the AI's OWN answer — the first line of what it produced — is the
/// descriptive, per-run title ("A rugged 150W speaker for AV environments").
/// Free: it reuses the response already in the transcript, no extra model call.
pub fn derive_title(messages: &[ChatMessage]) -> String {
    use crate::providers::Role;

    let first_user = messages.iter().find(|m| m.role == Role::User);
    let is_preset = first_user.is_some_and(|m| m.display.is_some());

    // A preset titles from the assistant's answer; a plain chat from the question.
    let source = if is_preset {
        messages
            .iter()
            .find(|m| m.role == Role::Assistant)
            .map(|m| m.content_text())
            .filter(|t| !t.trim().is_empty())
            .or_else(|| first_user.map(|m| m.content_text()))
            .unwrap_or_default()
    } else {
        first_user.map(|m| m.content_text()).unwrap_or_default()
    };

    // First non-empty line, with light markdown stripping so a heading/bullet
    // answer ("## Summary", "- point") yields a clean title, not its syntax.
    let raw = source.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = raw
        .trim()
        .trim_start_matches(|c: char| c == '#' || c == '-' || c == '*' || c == '>')
        .trim();
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

#[cfg(test)]
mod title_tests {
    use super::*;
    use crate::providers::{ChatMessage, MessageDisplay};

    fn user(content: &str) -> ChatMessage {
        ChatMessage::user(content)
    }

    fn preset_user(instruction: &str, content: &str) -> ChatMessage {
        let mut m = ChatMessage::user(content);
        m.display = Some(MessageDisplay {
            instruction: instruction.to_string(),
            label: "Selected text · 756".to_string(),
            body: content.to_string(),
        });
        m
    }

    #[test]
    fn plain_chat_titles_from_the_question() {
        let msgs = vec![
            user("what is rust?"),
            ChatMessage::assistant("Rust is a language."),
        ];
        assert_eq!(derive_title(&msgs), "what is rust?");
    }

    #[test]
    fn a_preset_titles_from_the_answer_not_the_template() {
        // The first user turn is the rendered template — identical for every
        // summarize. The `display` marks it a preset, so the title comes from
        // what the AI produced instead.
        let msgs = vec![
            preset_user(
                "Summarize the following text in 2-3 concise sentences:",
                "Summarize the following text in 2-3 concise sentences:\n\nlong blob…",
            ),
            ChatMessage::assistant("A rugged 150W speaker for AV environments."),
        ];
        assert_eq!(
            derive_title(&msgs),
            "A rugged 150W speaker for AV environments."
        );
    }

    #[test]
    fn a_preset_answer_title_strips_markdown_syntax() {
        let msgs = vec![
            preset_user("Summarize:", "Summarize:\n\nblob"),
            ChatMessage::assistant("## Speaker overview\n\nDetails follow."),
        ];
        assert_eq!(derive_title(&msgs), "Speaker overview");
    }

    #[test]
    fn a_preset_with_no_answer_yet_falls_back_to_its_content() {
        // Mid-stream (no assistant turn): don't blow up or title "Conversation";
        // fall back to the user content so the row is at least identifiable.
        let msgs = vec![preset_user("Summarize:", "Summarize:\n\nthe payload")];
        assert_eq!(derive_title(&msgs), "Summarize:");
    }
}
