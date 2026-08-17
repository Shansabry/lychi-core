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
    /// Names of tools whose schemas have been sent to the model in THIS
    /// conversation. Append-only: once a tool is sent it stays for every later
    /// turn, so history never references a schema the model can no longer see
    /// (which confuses models), and the request prefix only ever grows —
    /// re-selection can add tools, never remove them. See
    /// [`super::select_tools_sticky`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sent_tools: Vec<String>,
}

impl Session {
    /// A fresh session seeded with a system prompt + the first user message.
    pub fn new(system: impl Into<String>, first_user: impl Into<String>) -> Self {
        Self {
            messages: vec![ChatMessage::system(system), ChatMessage::user(first_user)],
            pending: Vec::new(),
            sent_tools: Vec::new(),
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
            sent_tools: Vec::new(),
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

    /// The current system prompt (messages[0]), or `""` if there is no leading
    /// system message. Lets a caller augment the prompt (e.g. append a generated
    /// capability manifest) without re-threading the original string.
    pub fn system_prompt(&self) -> String {
        match self.messages.first() {
            Some(m) if m.role == crate::providers::Role::System => m.content_text(),
            _ => String::new(),
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

    /// Compact old bulk once the conversation is heavy: stub tool RESULTS and
    /// elide IMAGES outside the recent window. Returns how many messages were
    /// edited (0 = under threshold, nothing touched).
    ///
    /// The whole history re-ships with EVERY model round-trip, and providers
    /// have hard per-request budgets (Groq's free tier ≈ 8k tokens) — a long
    /// conversation otherwise walks into a rejection with no way out. Two
    /// rules keep the compaction honest:
    /// - THRESHOLD-BATCHED, never incremental: each edit is a deliberate
    ///   prompt-cache miss, so pruning fires rarely and does a batch at once
    ///   (the one sanctioned violation of the append-only contract).
    /// - The last [`Self::PRUNE_KEEP_RECENT`] messages are never touched — the
    ///   model must keep verbatim sight of the work it is doing NOW.
    ///
    /// Old screenshots are the heaviest case by far (base64 re-sent every
    /// turn), so image parts outside the window are dropped regardless of the
    /// text threshold.
    pub fn prune_old_bulk(&mut self) -> usize {
        /// Serialized-content budget before TEXT tool results get stubbed
        /// (~8k tokens' worth of bytes).
        const PRUNE_THRESHOLD_BYTES: usize = 32 * 1024;
        /// Tool results at or under this stay — stubbing a one-liner saves
        /// nothing and costs cache.
        const SMALL_RESULT_BYTES: usize = 240;
        const PRUNE_KEEP_RECENT: usize = 10;

        let len = self.messages.len();
        let cutoff = len.saturating_sub(PRUNE_KEEP_RECENT);
        let mut edited = 0usize;

        // Images: elide outside the window unconditionally (they dwarf text).
        // `set_text` deliberately PRESERVES image parts, so strip them first.
        for m in &mut self.messages[..cutoff] {
            if m.has_images() {
                let kept = m.content_text();
                m.content
                    .retain(|p| !matches!(p, crate::providers::ContentPart::Image { .. }));
                m.set_text(format!(
                    "{kept}\n[image elided from history — it was analyzed above]"
                ));
                edited += 1;
            }
        }

        let total: usize = self.messages.iter().map(|m| m.content_text().len()).sum();
        if total <= PRUNE_THRESHOLD_BYTES {
            return edited;
        }

        for m in &mut self.messages[..cutoff] {
            if m.role == crate::providers::Role::Tool {
                let text = m.content_text();
                if text.len() > SMALL_RESULT_BYTES && !text.starts_with("[tool output elided") {
                    m.set_text(format!(
                        "[tool output elided from history — {} chars; re-run the tool if                          you need it again]",
                        text.len()
                    ));
                    edited += 1;
                }
            }
        }
        edited
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
    fn prune_stubs_old_bulk_and_keeps_the_recent_window() {
        let mut s = Session::new("sys", "start");
        // 12 old fat tool results (~48KB) then a recent tail.
        for i in 0..12 {
            s.push_assistant(format!("calling {i}"), Vec::new());
            s.push_tool_result(&format!("c{i}"), "x".repeat(4096), false);
        }
        for i in 0..5 {
            s.push_user(format!("follow-up {i}"));
            s.push_assistant(format!("answer {i}"), Vec::new());
        }
        let edited = s.prune_old_bulk();
        assert!(edited > 0, "over threshold must prune");
        // Old results are stubs; the newest messages are untouched.
        let stubbed = s
            .messages
            .iter()
            .filter(|m| m.content_text().starts_with("[tool output elided"))
            .count();
        assert!(stubbed >= 8, "old fat results stubbed: {stubbed}");
        let tail = &s.messages[s.messages.len() - 10..];
        assert!(
            tail.iter()
                .all(|m| !m.content_text().starts_with("[tool output elided")),
            "recent window untouched"
        );
        // Idempotent: a second pass has nothing left to do (already stubbed,
        // and now under threshold).
        assert_eq!(s.prune_old_bulk(), 0);
    }

    #[test]
    fn prune_leaves_light_conversations_alone() {
        let mut s = Session::new("sys", "hi");
        for i in 0..12 {
            s.push_assistant(format!("t{i}"), Vec::new());
            s.push_tool_result(&format!("c{i}"), "small".into(), false);
        }
        assert_eq!(s.prune_old_bulk(), 0);
        assert!(
            s.messages
                .iter()
                .all(|m| !m.content_text().contains("elided"))
        );
    }

    #[test]
    fn prune_elides_old_images_regardless_of_size() {
        let mut s = Session::new("sys", "look at this");
        s.push_user_with_images(
            "[screenshot]",
            vec![ImageSource {
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            }],
        );
        // Push the image out of the recent window with small messages.
        for i in 0..12 {
            s.push_user(format!("small {i}"));
        }
        let edited = s.prune_old_bulk();
        assert_eq!(edited, 1, "the old image message was edited");
        assert!(
            s.messages.iter().all(|m| !m.has_images()),
            "no image bytes remain in history"
        );
        let elided = s
            .messages
            .iter()
            .find(|m| m.content_text().contains("image elided"))
            .expect("elision note present");
        assert!(elided.content_text().contains("[screenshot]"), "text kept");
    }

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
            sent_tools: Vec::new(),
        };
        s.set_system("sys");
        assert_eq!(s.messages[0].role, Role::System);
        assert_eq!(s.messages[0].content_text(), "sys");
        assert_eq!(s.messages[1].role, Role::User);
    }
}
