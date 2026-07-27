pub mod byo;
pub mod capability;
pub mod cloud;
pub mod errors;
pub mod factory;
#[cfg(feature = "local-ai")]
pub mod local;
/// Streamed model downloader (NOT feature-gated — a user can download a model
/// independent of whether this build has the inference engine).
pub mod local_download;
/// The curated local-model registry (plain metadata — NOT feature-gated, so the
/// download command + settings UI work without the `local-ai` engine feature).
pub mod local_models;
pub mod ollama;
/// Shared wire-format encoding + SSE streaming machinery for the `chat` primitive.
mod wire;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::action_registry::RiskLevel;
use crate::error::LychiError;

// ─────────────────────────────────────────────────────────────────────────────
// The chat primitive (the new AI model — replaces route_intent/route_or_plan/
// answer_question). ONE way to talk to the model: a streaming, tool-calling
// `chat` turn. See docs/ai-rewrite plan. The old route/plan types below are
// migration scaffolding and are deleted once every caller is on `chat`.
// ─────────────────────────────────────────────────────────────────────────────

/// A conversation role. Mirrors the union both wire formats share
/// (system/user/assistant/tool).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A base64-encoded image attached to a `User` turn for vision models. `data` is
/// the raw base64 (no `data:` URI prefix); `media_type` is the MIME (`image/png`,
/// `image/jpeg`, …). Each wire encoder wraps this into its dialect's image block
/// (Anthropic `source:{type:base64,…}`, OpenAI `image_url` data-URI).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ImageSource {
    pub media_type: String,
    pub data: String,
}

/// One block of a message's content. A message is a sequence of these: prose
/// (`Text`) interleaved with `Image` attachments. Text-only messages are a single
/// `Text` part — the common case — so the string constructors below stay ergonomic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ContentPart {
    Text { text: String },
    Image { source: ImageSource },
}

impl ContentPart {
    pub fn text(t: impl Into<String>) -> Self {
        ContentPart::Text { text: t.into() }
    }
    pub fn image(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        ContentPart::Image {
            source: ImageSource {
                media_type: media_type.into(),
                data: data.into(),
            },
        }
    }
}

/// One conversation turn. A `Tool`-role message carries the `tool_call_id` it
/// answers. An `Assistant` message that requested tools keeps those calls in
/// `tool_calls` so the turn round-trips to the provider on the next request
/// (Anthropic requires the `tool_use` blocks be replayed; OpenAI the
/// `tool_calls` array).
///
/// `content` is a list of [`ContentPart`]s (text interleaved with images).
/// Text-only turns hold a single `Text` part. The [`content_string_or_seq`] serde
/// helper deserializes either a plain JSON string (legacy persisted sessions) or
/// the block array, so history written before vision landed still loads.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(with = "content_string_or_seq")]
    pub content: Vec<ContentPart>,
    /// Set on `Role::Tool` messages — the id of the `ToolCall` this result answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Set on `Role::Assistant` messages that requested tool calls — preserved so
    /// the assistant turn replays correctly on the next request. Empty otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Marks a tool result as an error so the model can react (Anthropic
    /// `is_error`, OpenAI conventionally a text marker). Only meaningful on `Tool`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    /// How this turn should RENDER, when that differs from its content.
    ///
    /// A preset folds a large payload out of the bubble into a collapsed chip
    /// ("Summarize the following: …" + a chip holding the blob). The model still
    /// receives the full `content`; this records the split the sender already
    /// computed so a RECALLED conversation renders identically instead of
    /// re-deriving the boundary from a flat string — which would be a second,
    /// drifting decider. Purely presentational: never sent to any provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<MessageDisplay>,
}

/// The presentational split of a user turn: the instruction line shown in the
/// bubble plus the payload folded into a collapsed chip. Computed ONCE by the
/// sender (the frontend's `presetDisplay`) and persisted with the message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct MessageDisplay {
    /// The short line the bubble shows in place of the full content.
    pub instruction: String,
    /// The chip's caption (e.g. `Selected text · 1.2k`).
    pub label: String,
    /// The folded-out payload, revealed when the chip is expanded.
    pub body: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::plain(Role::System, content)
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self::plain(Role::User, content)
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::plain(Role::Assistant, content)
    }
    /// A user turn carrying text plus one or more image attachments (vision).
    /// Empty `text` is fine — the images stand alone.
    pub fn user_with_images(text: impl Into<String>, images: Vec<ImageSource>) -> Self {
        let text = text.into();
        let mut content: Vec<ContentPart> = Vec::with_capacity(images.len() + 1);
        if !text.is_empty() {
            content.push(ContentPart::Text { text });
        }
        content.extend(
            images
                .into_iter()
                .map(|source| ContentPart::Image { source }),
        );
        Self {
            role: Role::User,
            content,
            tool_call_id: None,
            tool_calls: Vec::new(),
            is_error: false,
            display: None,
        }
    }
    /// A tool-result turn answering `tool_call_id`.
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentPart::text(content)],
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: Vec::new(),
            is_error,
            display: None,
        }
    }
    fn plain(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentPart::text(content)],
            tool_call_id: None,
            tool_calls: Vec::new(),
            is_error: false,
            display: None,
        }
    }

    /// The message's text, concatenating every `Text` part (images skipped).
    /// This is what the many text-only read sites want.
    pub fn content_text(&self) -> String {
        let mut out = String::new();
        for part in &self.content {
            if let ContentPart::Text { text } = part {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }

    /// True when the message carries at least one image attachment.
    pub fn has_images(&self) -> bool {
        self.content
            .iter()
            .any(|p| matches!(p, ContentPart::Image { .. }))
    }

    /// Replace the message's text in place, preserving any image parts (used by
    /// `Session::set_system`, which only ever holds text).
    pub fn set_text(&mut self, text: impl Into<String>) {
        let images: Vec<ContentPart> = self
            .content
            .drain(..)
            .filter(|p| matches!(p, ContentPart::Image { .. }))
            .collect();
        self.content = std::iter::once(ContentPart::text(text))
            .chain(images)
            .collect();
    }
}

/// Serde adapter for `ChatMessage::content`: serializes as the block array, but
/// deserializes from EITHER a plain string (legacy: pre-vision persisted history
/// stored `content` as a bare JSON string) OR the block array. This is what keeps
/// old `ai_history` conversations loadable after the multimodal migration.
mod content_string_or_seq {
    use super::ContentPart;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &[ContentPart], s: S) -> Result<S::Ok, S::Error> {
        v.serialize(s)
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrSeq {
        Str(String),
        Seq(Vec<ContentPart>),
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<ContentPart>, D::Error> {
        Ok(match StringOrSeq::deserialize(d)? {
            StringOrSeq::Str(s) => vec![ContentPart::Text { text: s }],
            StringOrSeq::Seq(v) => v,
        })
    }
}

/// A tool the model may call. Every Lychi handler takes a single `args: &str`,
/// so the parameter schema is uniform (`{ "args": string }`) and lives in the
/// provider's wire encoder, not here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
}

/// A tool invocation the model requested. `id` correlates the eventual
/// `ChatMessage::tool_result`. `args` is the single string argument (the
/// provider decodes its wire-specific `{"args": ...}` object into this).
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: String,
}

/// Why a `chat` stream ended. Extend as providers surface more (refusal, length…).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The assistant finished its turn (final answer).
    EndTurn,
    /// The assistant wants to call tools (the coordinator executes + loops).
    ToolUse,
    /// Output hit the max-token cap.
    MaxTokens,
}

/// A normalized event from a `chat` stream. Every provider (SSE or the local
/// engine) maps its wire/loop into this one shape, so the coordinator consumes a
/// single stream regardless of provider. Errors are the stream's `Result` item,
/// not a variant here (a terminal `Err` ends the stream — idiomatic Rust).
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Turn started — carries the model id (for logging / display).
    MessageStart { model: String },
    /// A chunk of assistant-visible prose.
    TextDelta(String),
    /// A chunk of extended-thinking / reasoning text (shown separately, if at all).
    ReasoningDelta(String),
    /// A tool call began — its id + name are known; args stream after.
    ToolCallStart { id: String, name: String },
    /// A fragment of a tool call's argument JSON (accumulate by `id`).
    ToolCallArgsDelta { id: String, delta: String },
    /// A tool call is fully assembled. `args` is the single Lychi argument string
    /// (the provider has already unwrapped its `{"args": …}` wire object).
    ToolCallComplete {
        id: String,
        name: String,
        args: String,
    },
    /// The turn ended. `usage` carries token counts when the provider reports them
    /// (Anthropic/OpenAI SSE); `None` for providers that don't (e.g. local).
    Done {
        stop_reason: StopReason,
        usage: Option<Usage>,
    },
}

/// Token usage for one model turn, when the provider reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// The stream a provider returns: a boxed, `Send + 'static` stream of events (or
/// a terminal error). Boxed because trait objects can't return `impl Stream`
/// (RPITIT is not `dyn`-compatible as of 2025); `'static` because the stream
/// owns its state (Arc clones), never borrows `self`.
pub type EventStream = futures_util::stream::BoxStream<'static, Result<StreamEvent, LychiError>>;

/// Re-export so providers/coordinator name one cancellation type.
pub use tokio_util::sync::CancellationToken;

/// The result of AI intent routing — a structured action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiRoute {
    pub action_id: String,
    pub args: String,
}

/// A single step in an agent plan.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct AgentStep {
    pub action_id: String,
    pub args: String,
    pub label: String,
    pub risk: RiskLevel,
}

/// A multi-step plan generated by AI.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct AgentPlan {
    pub id: String,
    pub input: String,
    pub steps: Vec<AgentStep>,
}

/// The AI can return either a single route or a multi-step plan.
#[derive(Debug, Clone)]
pub enum AiResponse {
    SingleRoute(AiRoute),
    Plan(AgentPlan),
}

/// Generate a short random plan ID.
pub fn generate_plan_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("plan-{ts}")
}

/// Trait for AI providers (BYO, Ollama, Cloud).
///
/// Each provider implements the same interface so the router
/// can swap between them based on config.
#[async_trait]
pub trait AiProvider: Send + Sync {
    /// Parse natural language input into a structured command route (single-shot only).
    async fn route_intent(
        &self,
        input: &str,
        known_actions: &[&str],
    ) -> Result<AiRoute, LychiError>;

    /// Route input, returning either a single route or a multi-step plan.
    /// `context_hint` is an optional environment context string appended to the system prompt.
    async fn route_or_plan(
        &self,
        input: &str,
        known_actions: &[&str],
        context_hint: Option<&str>,
    ) -> Result<AiResponse, LychiError>;

    /// Check if the provider is reachable and functional.
    async fn health_check(&self) -> bool;

    /// Human-readable provider name (e.g. "anthropic", "openai", "ollama").
    fn name(&self) -> &str;

    /// Send a direct question to the AI with a custom system prompt.
    /// Used by the "ask" handler for QA rather than intent routing.
    async fn answer_question(
        &self,
        _system_prompt: &str,
        _question: &str,
    ) -> Result<String, LychiError> {
        Err(LychiError::Ai(
            "answer_question not supported by this provider".to_string(),
        ))
    }

    /// The ONE chat call — streaming, tool-calling. Returns a boxed stream of
    /// normalized `StreamEvent`s: prose (`TextDelta`), tool calls (`ToolCall*`),
    /// and a terminal `Done`. `tools` empty ⇒ pure chat (no acting). A terminal
    /// `Err` item ends the stream on failure.
    ///
    /// The method is SYNC (constructs + returns the stream; all async/IO work
    /// happens when the stream is polled). This keeps the trait object-safe for
    /// `Box<dyn AiProvider>`. `cancel` is honored by every provider — HTTP ones
    /// also cancel by drop (dropping the stream closes the connection), but the
    /// local engine (spawn_blocking, un-abortable) MUST poll the token.
    ///
    /// Replaces `route_intent` / `route_or_plan` / `answer_question` (kept
    /// temporarily as migration scaffolding, deleted once every caller migrates).
    /// No default impl — every provider MUST implement it (no fallback path).
    fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        cancel: CancellationToken,
    ) -> EventStream;
}
