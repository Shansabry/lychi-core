//! Conversation state + human-in-the-loop approval types.
//!
//! `Session` is the append-only message array — the whole truth of a
//! conversation. It's serializable (so history/persistence come for free) and
//! the append-before-suspend invariant is what makes approval resume safe:
//! every already-executed tool result is in `messages` before the coordinator
//! returns `AwaitingApproval`, so resuming only runs the *pending* tools.

use serde::{Deserialize, Serialize};

use crate::providers::{ChatMessage, ImageSource, ToolCall};

use super::tool_executor::ResumeToken;

/// The running conversation. Append-only: user turns, assistant turns (with any
/// tool calls they requested), and tool results all accumulate here in order.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Session {
    /// The full message history, in order. This is the model's context each turn.
    pub messages: Vec<ChatMessage>,
    /// Tool calls from the latest assistant turn that are paused awaiting user
    /// approval (destructive, Rules-Engine-gated). Empty unless suspended. On
    /// resume, these are executed (or rejected) in order and their results
    /// appended to `messages`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<PendingApproval>,
}

impl Session {
    /// A fresh session seeded with a system prompt + the first user message.
    pub fn new(system: impl Into<String>, first_user: impl Into<String>) -> Self {
        Self {
            messages: vec![ChatMessage::system(system), ChatMessage::user(first_user)],
            pending: Vec::new(),
        }
    }

    /// A fresh session seeded with a system prompt + a first user message that
    /// carries image attachments (vision). Equivalent to `new` when `images` is
    /// empty.
    pub fn new_with_images(
        system: impl Into<String>,
        first_user: impl Into<String>,
        images: Vec<ImageSource>,
    ) -> Self {
        Self {
            messages: vec![
                ChatMessage::system(system),
                ChatMessage::user_with_images(first_user, images),
            ],
            pending: Vec::new(),
        }
    }

    /// Append a user turn (a follow-up or a resumed conversation).
    pub fn push_user(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage::user(content));
    }

    /// Append a user turn carrying image attachments. Equivalent to `push_user`
    /// when `images` is empty.
    pub fn push_user_with_images(&mut self, content: impl Into<String>, images: Vec<ImageSource>) {
        self.messages
            .push(ChatMessage::user_with_images(content, images));
    }

    /// Stamp the presentational split onto the most recent user turn.
    ///
    /// The sender already computed how the bubble folds (instruction + chip);
    /// recording it here is what lets a recalled conversation render identically
    /// without a second decider re-deriving the boundary from flat text. No-op
    /// when there is no user turn or no split to record.
    pub fn set_last_user_display(&mut self, display: Option<crate::providers::MessageDisplay>) {
        let Some(display) = display else { return };
        if let Some(m) = self
            .messages
            .iter_mut()
            .rev()
            .find(|m| m.role == crate::providers::Role::User)
        {
            m.display = Some(display);
        }
    }

    /// Replace the system prompt (messages[0]) in place, keeping all history.
    /// Used when escalating a quick-AI answer into the full agent: the short
    /// "2-3 sentences" prompt is swapped for the tool-aware agent prompt, so
    /// follow-ups behave like the full agent while retaining the answer already
    /// produced. No-op if there's no leading system message (defensive).
    pub fn set_system(&mut self, system: impl Into<String>) {
        if let Some(first) = self.messages.first_mut()
            && first.role == crate::providers::Role::System
        {
            first.set_text(system);
        } else {
            self.messages.insert(0, ChatMessage::system(system));
        }
    }

    /// Append the assistant turn for a completed model response: its prose plus
    /// any tool calls it requested (preserved so the turn round-trips to the
    /// provider on the next request).
    pub fn push_assistant(&mut self, text: String, tool_calls: Vec<ToolCall>) {
        self.messages.push(ChatMessage {
            role: crate::providers::Role::Assistant,
            content: vec![crate::providers::ContentPart::text(text)],
            tool_call_id: None,
            tool_calls,
            is_error: false,
            display: None,
        });
    }

    /// Append a tool result, answering a specific tool call.
    pub fn push_tool_result(&mut self, call_id: &str, output: String, is_error: bool) {
        self.messages
            .push(ChatMessage::tool_result(call_id, output, is_error));
    }
}

/// A tool call captured mid-loop because the Rules Engine flagged it destructive.
/// Serialized into the `Session` at the suspend boundary so the exact assessed
/// action can be run (not re-resolved) on approval.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingApproval {
    pub call: ToolCall,
    pub reason: String,
    /// Opaque resume handle from the executor (runs THIS assessed action).
    pub resume: ResumeToken,
}

/// A request for the user to approve (or reject) a destructive tool call. Handed
/// to the caller in `Outcome::AwaitingApproval`; the caller shows a prompt and
/// calls `Coordinator::resume` with an `ApprovalDecision`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub call_id: String,
    pub tool_name: String,
    pub args: String,
    /// Why it needs approval (from the Rules Engine / `needs_confirmation`).
    pub reason: String,
}

/// What the user decided about a pending tool call. The standard vocabulary
/// (minus the niche "respond"). A launcher needs Approve + Reject; Edit is there
/// for later ("run it, but with these args instead").
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ApprovalDecision {
    /// Run the tool as assessed.
    Approve,
    /// Run the tool, but with edited args.
    ApproveWithEdit { args: String },
    /// Don't run it; feed the (optional) message back to the model as the tool
    /// result so it can adjust.
    Reject { message: Option<String> },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Role;

    #[test]
    fn display_split_is_stamped_on_the_last_user_turn_and_survives_a_roundtrip() {
        use crate::providers::MessageDisplay;

        let mut s = Session::new("sys", "Summarize the following: <a very long blob>");
        s.set_last_user_display(Some(MessageDisplay {
            instruction: "Summarize the following: …".into(),
            label: "Selected text · 1.2k".into(),
            body: "<a very long blob>".into(),
        }));

        let user = &s.messages[1];
        assert_eq!(user.role, Role::User);
        let d = user.display.as_ref().expect("display should be stamped");
        assert_eq!(d.instruction, "Summarize the following: …");
        assert_eq!(d.body, "<a very long blob>");
        // The model still receives the FULL content — the split is presentational.
        assert!(user.content_text().contains("<a very long blob>"));

        // Persisted history must carry it back, otherwise recall has to guess.
        let json = serde_json::to_string(&s.messages).unwrap();
        let back: Vec<crate::providers::ChatMessage> = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back[1].display.as_ref().unwrap().label,
            "Selected text · 1.2k"
        );
    }

    #[test]
    fn messages_without_a_display_split_stay_none() {
        let mut s = Session::new("sys", "hello");
        s.set_last_user_display(None);
        assert!(s.messages[1].display.is_none());
        // A legacy message (no `display` key at all) still deserializes.
        let legacy = r#"[{"role":"user","content":"hi"}]"#;
        let back: Vec<crate::providers::ChatMessage> = serde_json::from_str(legacy).unwrap();
        assert!(back[0].display.is_none());
    }

    #[test]
    fn set_system_replaces_leading_prompt_keeping_history() {
        let mut s = Session::new("terse prompt", "what is rust?");
        s.push_assistant("Rust is a systems language.".into(), Vec::new());
        s.set_system("full agent prompt");
        // System swapped, history (user + assistant) intact and in order.
        assert_eq!(s.messages[0].role, Role::System);
        assert_eq!(s.messages[0].content_text(), "full agent prompt");
        assert_eq!(s.messages[1].role, Role::User);
        assert_eq!(s.messages[1].content_text(), "what is rust?");
        assert_eq!(s.messages[2].role, Role::Assistant);
        assert_eq!(s.messages.len(), 3);
    }

    #[test]
    fn set_system_inserts_when_no_leading_system() {
        let mut s = Session {
            messages: vec![ChatMessage::user("hi")],
            pending: Vec::new(),
        };
        s.set_system("sys");
        assert_eq!(s.messages[0].role, Role::System);
        assert_eq!(s.messages[0].content_text(), "sys");
        assert_eq!(s.messages[1].role, Role::User);
    }
}
