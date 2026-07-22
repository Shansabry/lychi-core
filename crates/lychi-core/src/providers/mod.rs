pub mod byo;
pub mod cloud;
pub mod factory;
#[cfg(feature = "local-ai")]
pub mod local;
/// The curated local-model registry (plain metadata — NOT feature-gated, so the
/// download command + settings UI work without the `local-ai` engine feature).
pub mod local_models;
/// Streamed model downloader (NOT feature-gated — a user can download a model
/// independent of whether this build has the inference engine).
pub mod local_download;
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

/// One conversation turn. A `Tool`-role message carries the `tool_call_id` it
/// answers. An `Assistant` message that requested tools keeps those calls in
/// `tool_calls` so the turn round-trips to the provider on the next request
/// (Anthropic requires the `tool_use` blocks be replayed; OpenAI the
/// `tool_calls` array).
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
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
    /// A tool-result turn answering `tool_call_id`.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>, is_error: bool) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: Vec::new(),
            is_error,
        }
    }
    fn plain(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_call_id: None,
            tool_calls: Vec::new(),
            is_error: false,
        }
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
    ToolCallComplete { id: String, name: String, args: String },
    /// The turn ended. `usage` carries token counts when the provider reports them
    /// (Anthropic/OpenAI SSE); `None` for providers that don't (e.g. local).
    Done { stop_reason: StopReason, usage: Option<Usage> },
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
