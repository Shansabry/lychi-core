//! Shared wire-format machinery for the streaming `chat` primitive.
//!
//! The HTTP providers (BYO / Ollama / Cloud) all speak one of two dialects —
//! OpenAI-compatible or Anthropic Messages. Rather than each reimplementing the
//! "build request → POST → check status → parse SSE → yield events" flow, that
//! entire mechanism lives ONCE in [`WireClient`]. A provider is then thin: it
//! constructs a `WireClient` with the right dialect + auth + endpoint and calls
//! [`WireClient::stream`]. Provider *identity* (auth choice, health-check probe,
//! model discovery, name) stays in each provider; only the wire *mechanism* is
//! shared. (The local llama.cpp engine is not a wire client — it has its own.)

use std::sync::Arc;

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};

use crate::error::LychiError;

use super::{
    CancellationToken, ChatMessage, ContentPart, EventStream, Role, StopReason, StreamEvent,
    ToolDef,
};

// ── WireClient: the one HTTP→SSE→event flow, per dialect ──────────────────────

/// Which request/response dialect an endpoint speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
    /// OpenAI `/v1/chat/completions` (also Ollama, Groq, Grok, Gemini-compat, …).
    OpenAi,
    /// Anthropic Messages API.
    Anthropic,
}

/// How to authenticate the request. Providers own their key; the wire client
/// just applies the right header style.
#[derive(Debug, Clone)]
pub(crate) enum AuthStyle {
    /// `Authorization: Bearer <key>` (OpenAI-family).
    Bearer(String),
    /// `x-api-key: <key>` + `anthropic-version` (Anthropic).
    AnthropicKey(String),
    /// No auth header (e.g. a local Ollama instance).
    None,
}

/// Notified when a request fails, so callers can LEARN from it (see
/// `providers::capability`). Kept as a callback rather than a DB handle so the
/// wire layer stays free of storage concerns and remains testable in isolation.
pub(crate) type ErrorObserver = Arc<dyn Fn(&super::errors::AiError) + Send + Sync>;

/// How many times a transient (rate-limit / overload) request is retried before
/// the error is surfaced. Small on purpose: a limit that won't clear in three
/// backed-off attempts is not a transient blip, and the user is better told than
/// left waiting. Only retriable statuses are retried, and only before any tokens
/// have streamed (see the retry loop) — a mid-stream failure can't be replayed.
const MAX_RATE_LIMIT_RETRIES: u32 = 10;

/// Whether an HTTP status is a transient "back off and try again" signal, common
/// to every provider — not Groq- or Anthropic-specific:
///   - 429 Too Many Requests — rate limit (all providers)
///   - 529 — Anthropic "Overloaded" (their most common transient failure)
///   - 503 Service Unavailable / 502 Bad Gateway / 504 Gateway Timeout — the
///     provider or a proxy in front of it is momentarily unavailable
///
/// A non-transient failure (401 auth, 400 bad request, 404 model) is NOT here:
/// retrying it just delays the same error. The `Retry-After` header (when the
/// provider sends one) still drives the wait for any of these.
fn is_retriable_status(status: u16) -> bool {
    matches!(status, 429 | 529 | 503 | 502 | 504)
}

/// The wait between rate-limit retries. Deliberately SHORT and flat rather
/// than exponential: field evidence (2026-08-17 logs) showed Groq accepting an
/// identical request seconds after rejecting it, and honouring its 30s
/// `Retry-After` just parked the turn — many quick, polite probes beat one
/// long, obedient wait. 10 tries x 5s still spans a full TPM minute window.
const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// How long to wait before a retry: our flat interval, shortened further only
/// when the provider's `Retry-After` header promises the window clears sooner.
/// A LONGER hint is ignored on purpose — see [`RETRY_INTERVAL`].
fn retry_delay(headers: &reqwest::header::HeaderMap, _attempt: u32) -> std::time::Duration {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
        .unwrap_or(RETRY_INTERVAL)
        .min(RETRY_INTERVAL)
}

/// A configured client for one HTTP endpoint + dialect. Owns the complete
/// streaming-chat mechanism so providers don't duplicate it.
pub(crate) struct WireClient {
    http: Client,
    dialect: Dialect,
    /// Full endpoint URL (chat-completions or messages endpoint).
    url: String,
    model: String,
    max_tokens: u32,
    auth: AuthStyle,
    /// Optional hook fired on a classified failure. `None` in tests and for
    /// providers that have nothing to learn.
    on_error: Option<ErrorObserver>,
}

impl WireClient {
    pub(crate) fn new(
        http: Client,
        dialect: Dialect,
        url: impl Into<String>,
        model: impl Into<String>,
        max_tokens: u32,
        auth: AuthStyle,
    ) -> Self {
        Self {
            http,
            dialect,
            url: url.into(),
            model: model.into(),
            max_tokens,
            auth,
            on_error: None,
        }
    }

    /// Attach a failure observer (builder-style). Used to record learned model
    /// capabilities without giving the wire layer a database.
    pub(crate) fn with_error_observer(mut self, obs: Option<ErrorObserver>) -> Self {
        self.on_error = obs;
        self
    }

    /// Stream a chat turn as normalized [`super::StreamEvent`]s. Builds the
    /// dialect-specific body, POSTs (inside the returned stream, so a request
    /// error is a terminal `Err` item), and drives the SSE → event mapping.
    pub(crate) fn stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        cancel: CancellationToken,
    ) -> EventStream {
        let http = self.http.clone();
        let dialect = self.dialect;
        let url = self.url.clone();
        let model = self.model.clone();
        let auth = self.auth.clone();
        let on_error = self.on_error.clone();

        // Build the wire body up-front (pure, no IO).
        let mut body = build_body(dialect, &model, self.max_tokens, messages, tools);
        // GROQ QUIRK: gpt-oss models sometimes mangle a tool NAME
        // ("web_tools.fetch", a leaked harmony channel marker), and Groq's
        // server-side validator then kills the whole turn. Their documented
        // escape hatch hands the call back to the client unvalidated — and the
        // coordinator normalizes recognizable manglings back to the intended
        // tool (see `coordinator::normalize_tool_call`), which turns a dead
        // turn into a working call. Groq-only: OpenAI proper rejects unknown
        // request fields.
        if self.url.contains("api.groq.com") && !tools.is_empty() {
            body["disable_tool_validation"] = json!(true);
        }
        // Request-weight observability: reported input_tokens routinely exceeds
        // what the visible payload suggests (provider chat templates re-render
        // tool schemas verbosely), and diagnosing "why was this turn N tokens"
        // needs the actual sizes, not estimates.
        tracing::debug!(
            body_bytes = body.to_string().len(),
            tools = tools.len(),
            messages = messages.len(),
            "[wire] request built"
        );
        // Whether this request carried images decides how a 400 is explained: a
        // shape complaint about `content` means "text-only model" only when we
        // actually sent image blocks. Captured here, before `messages` is dropped.
        let had_images = messages.iter().any(|m| m.has_images());

        async_stream::try_stream! {
            let build_req = || {
                let mut req = http.post(&url).header("Content-Type", "application/json");
                req = match &auth {
                    AuthStyle::Bearer(k) => req.header("Authorization", format!("Bearer {k}")),
                    AuthStyle::AnthropicKey(k) => req
                        .header("x-api-key", k)
                        .header("anthropic-version", "2023-06-01"),
                    AuthStyle::None => req,
                };
                req.json(&body)
            };

            // Send, with a bounded backoff-retry on a TRANSIENT status (rate
            // limit or overload — see `is_retriable_status`), generic to every
            // provider: a free tier throttling (429), Anthropic overloaded (529),
            // or the endpoint momentarily unavailable (503/502/504). A blip that
            // clears in a second or two shouldn't kill the turn. We retry the SAME
            // request up to MAX_RATE_LIMIT_RETRIES times, waiting the provider's
            // Retry-After (or our own backoff), and tell the user we're waiting via
            // a reasoning-channel notice so it reads as "working", not "frozen".
            // Only safe HERE, before any token has streamed — once the byte stream
            // starts, a failure can't be replayed. Every other error (auth,
            // too-large, unknown model, transport) surfaces immediately, unchanged.
            let resp = {
                let mut attempt: u32 = 0;
                loop {
                    if cancel.is_cancelled() { return; }
                    // Provider failures are classified into one actionable sentence
                    // (`providers::errors`) rather than raw JSON.
                    let resp = build_req().send().await.map_err(|e| {
                        let err = super::errors::classify(None, &e.to_string(), had_images);
                        if let Some(obs) = &on_error { obs(&err); }
                        LychiError::Ai(err.message)
                    })?;
                    let status = resp.status();
                    if status.is_success() {
                        break resp;
                    }
                    if is_retriable_status(status.as_u16()) && attempt < MAX_RATE_LIMIT_RETRIES {
                        attempt += 1;
                        let delay = retry_delay(resp.headers(), attempt);
                        // Word it by cause: a 429 is a rate limit; a 529/503/502/504
                        // is the provider being busy/unavailable. Same retry, honest
                        // label.
                        let cause = if status.as_u16() == 429 {
                            "Rate limited"
                        } else {
                            "Provider busy"
                        };
                        // Surface the wait as a typed NOTICE — the UI renders it
                        // beside the thinking indicator and ticks the countdown.
                        yield StreamEvent::Notice {
                            text: format!("{cause} — retry {attempt}/{MAX_RATE_LIMIT_RETRIES}"),
                            countdown_secs: Some(delay.as_secs().max(1)),
                        };
                        // Interruptible wait: Escape must not be stuck behind it.
                        tokio::select! {
                            _ = tokio::time::sleep(delay) => {}
                            _ = cancel.cancelled() => { return; }
                        }
                        continue;
                    }
                    // Terminal error (non-429, or 429 out of retries): read the
                    // body, classify, notify, diverge. The RAW body goes to the
                    // log — the user sees the friendly classification, but
                    // diagnosing a misclassification (or a provider quirk)
                    // needs the provider's actual words, which are otherwise
                    // discarded here.
                    let text = resp.text().await.unwrap_or_default();
                    let err = super::errors::classify(Some(status.as_u16()), &text, had_images);
                    tracing::warn!(
                        status = status.as_u16(),
                        kind = ?err.kind,
                        detail = %err.detail,
                        "[wire] provider rejected the request"
                    );
                    // Some retryable failures hide behind non-retriable
                    // statuses: Groq's TPM rejections are HTTP 413, so only
                    // the classified BODY reveals them as rate limits. That
                    // includes BudgetExceeded — Groq's pre-check ESTIMATE is
                    // crude (observed ~2x real tokens) and the same request
                    // can pass seconds later, so it earns the same bounded
                    // retries before its trim-your-input message surfaces.
                    if err.kind.is_retryable() && attempt < MAX_RATE_LIMIT_RETRIES {
                        attempt += 1;
                        // Headers are gone with the body read — use the flat
                        // interval; 10 probes still cross a full TPM window.
                        let delay = RETRY_INTERVAL;
                        yield StreamEvent::Notice {
                            text: format!("Rate limited — retry {attempt}/{MAX_RATE_LIMIT_RETRIES}"),
                            countdown_secs: Some(delay.as_secs()),
                        };
                        tokio::select! {
                            _ = tokio::time::sleep(delay) => {}
                            _ = cancel.cancelled() => { return; }
                        }
                        continue;
                    }
                    if let Some(obs) = &on_error { obs(&err); }
                    Err(LychiError::Ai(err.message))?;
                    return; // unreachable after `?`, explicit divergence.
                }
            };
            let byte_stream = resp.bytes_stream();
            let inner = match dialect {
                Dialect::Anthropic => anthropic_event_stream(byte_stream, model, cancel),
                Dialect::OpenAi => openai_event_stream(byte_stream, model, cancel),
            };
            for await ev in inner {
                yield ev?;
            }
        }
        .boxed()
    }
}

/// Build the dialect-specific request body (pure, no IO — testable). Anthropic
/// pulls the system prompt out-of-band; both attach tools only when non-empty.
fn build_body(
    dialect: Dialect,
    model: &str,
    max_tokens: u32,
    messages: &[ChatMessage],
    tools: &[ToolDef],
) -> Value {
    match dialect {
        Dialect::Anthropic => {
            let mut b = json!({
                "model": model, "max_tokens": max_tokens, "stream": true,
                "messages": anthropic_messages(messages),
            });
            let system = anthropic_system_blocks(messages);
            if !system.is_null() {
                b["system"] = system;
            }
            if !tools.is_empty() {
                b["tools"] = json!(anthropic_tools(tools));
            }
            b
        }
        Dialect::OpenAi => {
            let mut b = json!({
                "model": model, "max_tokens": max_tokens, "stream": true,
                // Token usage is OMITTED from a stream unless explicitly asked
                // for — the default is `null` on every chunk. With this set, one
                // extra final chunk carries the totals for the whole request.
                // Anthropic sends usage unprompted; OpenAI-compatible providers
                // (Groq, OpenRouter, …) do not, which is why the chat showed no
                // token count.
                "stream_options": { "include_usage": true },
                "messages": openai_messages(messages),
            });
            if !tools.is_empty() {
                b["tools"] = json!(openai_tools(tools));
            }
            b
        }
    }
}

// ── Request encoding ─────────────────────────────────────────────────────────

/// Serialize the message history to Anthropic wire format. System messages are
/// handled out-of-band by the caller (collected into the top-level `system`
/// field); here we emit user/assistant/tool turns. A `Tool`-role message becomes
/// a `tool_result` block inside a USER message; assistant turns that carried tool
/// calls re-emit their `tool_use` blocks so the turn round-trips.
pub(crate) fn anthropic_messages(messages: &[ChatMessage]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        match m.role {
            Role::System => {} // handled out-of-band
            Role::User => {
                // A text-only user turn can go as a bare string; anything with an
                // image attachment must use the content-block array form.
                if m.has_images() {
                    out.push(json!({ "role": "user", "content": anthropic_content_blocks(m) }));
                } else {
                    out.push(json!({ "role": "user", "content": m.content_text() }));
                }
            }
            Role::Assistant => {
                let text = m.content_text();
                if m.tool_calls.is_empty() {
                    out.push(json!({ "role": "assistant", "content": text }));
                } else {
                    let mut blocks: Vec<Value> = Vec::new();
                    if !text.is_empty() {
                        blocks.push(json!({ "type": "text", "text": text }));
                    }
                    for tc in &m.tool_calls {
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": rewrap_args(&tc.args),
                        }));
                    }
                    out.push(json!({ "role": "assistant", "content": blocks }));
                }
            }
            Role::Tool => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content_text(),
                    "is_error": m.is_error,
                });
                // Coalesce consecutive tool results into the previous user
                // message (Anthropic wants all results for a turn in ONE message).
                if let Some(last) = out.last_mut()
                    && last["role"] == "user"
                    && last["content"].is_array()
                {
                    last["content"].as_array_mut().unwrap().push(block);
                } else {
                    out.push(json!({ "role": "user", "content": [block] }));
                }
            }
        }
    }
    attach_history_breakpoint(&mut out);
    out
}

/// PROMPT CACHING, third breakpoint: mark the last content block of the FINAL
/// message so the next request reuses the whole conversation prefix, not just
/// tools+system (which carry their own breakpoints). History is append-only
/// (the session contract), so each step/turn extends the previous request's
/// prefix and reads it back at ~0.1× — this is what stops a 10-step tool loop
/// re-billing the transcript at full price every round-trip. Uses 3 of
/// Anthropic's 4 allowed breakpoints in total.
fn attach_history_breakpoint(out: &mut [Value]) {
    let Some(last) = out.last_mut() else { return };
    // A bare-string content must become a block array to carry cache_control.
    // An empty string stays as-is — an empty text block is a wire error.
    if let Some(s) = last["content"].as_str() {
        if s.is_empty() {
            return;
        }
        last["content"] = json!([{ "type": "text", "text": s }]);
    }
    if let Some(block) = last["content"]
        .as_array_mut()
        .and_then(|blocks| blocks.last_mut())
    {
        block["cache_control"] = json!({ "type": "ephemeral" });
    }
}

/// Collect all System messages into one string for Anthropic's top-level
/// `system` field (it takes the system prompt out-of-band, not as a turn).
pub(crate) fn anthropic_system(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content_text())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Build Anthropic's `system` field as a cache-aware text block.
///
/// The system prompt is now fully STABLE across turns (the per-turn relevance hint
/// rides a trailing message, not the system prompt — see the coordinator loop), so
/// the whole block gets `cache_control: ephemeral` and Anthropic caches it (reads
/// bill at 0.1×). Returns `Value::Null` for an empty prompt so the caller omits it.
fn anthropic_system_blocks(messages: &[ChatMessage]) -> Value {
    let full = anthropic_system(messages);
    if full.is_empty() {
        return Value::Null;
    }
    Value::Array(vec![json!({
        "type": "text",
        "text": full,
        "cache_control": { "type": "ephemeral" },
    })])
}

/// Encode a user message's content parts as Anthropic content blocks: `text`
/// blocks and `image` blocks (`source:{type:"base64", media_type, data}`).
fn anthropic_content_blocks(m: &ChatMessage) -> Vec<Value> {
    m.content
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => json!({ "type": "text", "text": text }),
            ContentPart::Image { source } => json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": source.media_type,
                    "data": source.data,
                },
            }),
        })
        .collect()
}

/// Serialize the message history to OpenAI wire format. Tool results are separate
/// `role:"tool"` messages; assistant tool calls go in the `tool_calls` array.
pub(crate) fn openai_messages(messages: &[ChatMessage]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        match m.role {
            Role::System => out.push(json!({ "role": "system", "content": m.content_text() })),
            Role::User => {
                // Text-only → bare string; with images → the content-parts array.
                if m.has_images() {
                    out.push(json!({ "role": "user", "content": openai_content_parts(m) }));
                } else {
                    out.push(json!({ "role": "user", "content": m.content_text() }));
                }
            }
            Role::Assistant => {
                let text = m.content_text();
                if m.tool_calls.is_empty() {
                    out.push(json!({ "role": "assistant", "content": text }));
                } else {
                    let calls: Vec<Value> = m
                        .tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": rewrap_args(&tc.args).to_string(),
                                },
                            })
                        })
                        .collect();
                    out.push(json!({
                        "role": "assistant",
                        "content": if text.is_empty() { Value::Null } else { json!(text) },
                        "tool_calls": calls,
                    }));
                }
            }
            Role::Tool => out.push(json!({
                "role": "tool",
                "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                "content": m.content_text(),
            })),
        }
    }
    out
}

/// Encode a user message's content parts as OpenAI content parts: `text` parts
/// and `image_url` parts whose URL is a `data:<mime>;base64,<data>` URI.
fn openai_content_parts(m: &ChatMessage) -> Vec<Value> {
    m.content
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => json!({ "type": "text", "text": text }),
            ContentPart::Image { source } => json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{};base64,{}", source.media_type, source.data),
                },
            }),
        })
        .collect()
}

/// The JSON Schema for a tool's input: the handler's typed schema for a BOUNDED
/// tool (`system`, …), else the uniform free-text `{ args: string }` every
/// open-ended tool uses. Shared by both dialects so they can't diverge.
/// Optional (non-required) properties in a tool schema — what Anthropic's
/// strict grammar budget counts. Our schemas are flat, so a top-level walk is
/// the whole story.
fn count_optional_params(schema: &Value) -> usize {
    let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else {
        return 0;
    };
    let required: std::collections::HashSet<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    props
        .keys()
        .filter(|k| !required.contains(k.as_str()))
        .count()
}

fn tool_input_schema(t: &ToolDef) -> Value {
    t.input_schema.clone().unwrap_or_else(|| {
        json!({
            "type": "object",
            "properties": { "args": { "type": "string", "description": "The command arguments." } },
            "required": ["args"],
        })
    })
}

/// Anthropic tool schema. Bounded tools carry a typed `input_schema` (enum verb);
/// the rest use the uniform `{ args: string }`.
///
/// A typed tool also gets `"strict": true`, which makes Anthropic grammar-
/// constrain sampling to schema-valid inputs — the cloud analogue of the local
/// llama.cpp grammar, so a valid verb is guaranteed on both paths. Not set on the
/// free-text tools: strict requires `additionalProperties:false`/all-required,
/// and there is nothing to constrain on an open string anyway.
///
/// PROMPT CACHING: the last tool carries `cache_control: {type: ephemeral}`, which
/// tells Anthropic to cache the entire tools-array prefix. Cache reads bill at 0.1×
/// input price, so after the first turn the (large, stable) catalog is ~10× cheaper
/// and does not re-consume the latency of re-processing. This is why the catalog
/// MUST be byte-stable turn-to-turn (any tool change invalidates the whole cache) —
/// the agent now sends the full catalog every turn instead of a per-turn-varying
/// filtered subset, and the "relevant now" hint that steers selection lives in the
/// message stream, never in the tools array. Caching is GA (no beta header).
pub(crate) fn anthropic_tools(tools: &[ToolDef]) -> Vec<Value> {
    // Anthropic grammar-compiles `strict` tools with a hard budget on OPTIONAL
    // parameters summed across the request's schemas; over it the API rejects
    // the request outright ("Schemas contains too many optional parameters
    // (28) … limit: 24"). Strict is a guarantee, not a requirement — Claude
    // follows typed schemas reliably and the executor validates args anyway —
    // so keep it while the request fits and drop it wholesale when it doesn't.
    const STRICT_OPTIONAL_BUDGET: usize = 24;
    let optional_params: usize = tools
        .iter()
        .filter_map(|t| t.input_schema.as_ref())
        .map(count_optional_params)
        .sum();
    let strict_ok = optional_params <= STRICT_OPTIONAL_BUDGET;

    let last = tools.len().saturating_sub(1);
    tools
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut tool = json!({
                "name": t.name,
                "description": t.description,
                "input_schema": tool_input_schema(t),
            });
            if t.input_schema.is_some() && strict_ok {
                tool["strict"] = json!(true);
            }
            // Mark the cache breakpoint on the final tool so the whole array is
            // cached as one prefix. Harmless on providers that ignore it; on
            // Anthropic it is the single highest-value cost lever for the agent.
            if i == last {
                tool["cache_control"] = json!({ "type": "ephemeral" });
            }
            tool
        })
        .collect()
}

/// OpenAI tool schema — same per-tool schema, wrapped in the function shape.
pub(crate) fn openai_tools(tools: &[ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": tool_input_schema(t),
                },
            })
        })
        .collect()
}

// ── Streaming state machines ─────────────────────────────────────────────────

/// Accumulates one streaming tool call across SSE fragments, and whether we've
/// already emitted its `ToolCallStart` (so we emit exactly one).
#[derive(Default)]
struct ToolAccum {
    id: String,
    name: String,
    args_buf: String,
    started: bool,
}

/// Map an Anthropic SSE byte-stream into a stream of normalized `StreamEvent`s.
/// Emits `TextDelta` live, `ToolCallStart` + `ToolCallArgsDelta` as `tool_use`
/// blocks stream, `ToolCallComplete` when each block closes, and a terminal
/// `Done`. `cancel` is honored between events AND while parked in a read (the
/// SSE reader races it), so Esc works even on a connection that went silent.
pub(crate) fn anthropic_event_stream<S, B>(
    byte_stream: S,
    model: String,
    cancel: CancellationToken,
) -> super::EventStream
where
    S: futures_util::Stream<Item = reqwest::Result<B>> + Unpin + Send + 'static,
    B: AsRef<[u8]> + Send + 'static,
{
    use futures_util::StreamExt as _;
    async_stream::try_stream! {
        yield StreamEvent::MessageStart { model };
        let mut tool_blocks: std::collections::HashMap<u64, ToolAccum> = std::collections::HashMap::new();
        let mut stop = StopReason::EndTurn;
        let mut usage = super::Usage::default();
        let mut sse = SseReader::new(byte_stream);

        loop {
            if cancel.is_cancelled() { break; }
            let evt = match sse.next_event(&cancel).await? { Some(e) => e, None => break };
            let data: Value = match serde_json::from_str(&evt) { Ok(v) => v, Err(_) => continue };
            match data["type"].as_str() {
                Some("message_start") => {
                    // input_tokens is reported here; output_tokens accrues in message_delta.
                    let u = &data["message"]["usage"];
                    if let Some(n) = u["input_tokens"].as_u64() {
                        usage.input_tokens = n as u32;
                    }
                    // Prompt-cache read (the two-tier caching, made observable).
                    // Anthropic bills `input_tokens` as the UNCACHED remainder and
                    // reports cache reads separately, so the true prompt size is the
                    // sum; fold the cache read into input_tokens and record it.
                    if let Some(n) = u["cache_read_input_tokens"].as_u64() {
                        usage.cached_input_tokens = n as u32;
                        usage.input_tokens += n as u32;
                    }
                    if let Some(n) = u["cache_creation_input_tokens"].as_u64() {
                        usage.input_tokens += n as u32;
                    }
                }
                Some("message_delta") => {
                    // The authoritative stop_reason (incl. "max_tokens") + running
                    // output token count arrive here.
                    if let Some(sr) = data["delta"]["stop_reason"].as_str() {
                        stop = match sr {
                            "max_tokens" => StopReason::MaxTokens,
                            "tool_use" => StopReason::ToolUse,
                            _ => StopReason::EndTurn,
                        };
                    }
                    if let Some(n) = data["usage"]["output_tokens"].as_u64() {
                        usage.output_tokens = n as u32;
                    }
                }
                Some("content_block_start") => {
                    let idx = data["index"].as_u64().unwrap_or(0);
                    let block = &data["content_block"];
                    if block["type"] == "tool_use" {
                        let id = block["id"].as_str().unwrap_or_default().to_string();
                        let name = block["name"].as_str().unwrap_or_default().to_string();
                        tool_blocks.insert(idx, ToolAccum { id: id.clone(), name: name.clone(), args_buf: String::new(), started: true });
                        stop = StopReason::ToolUse;
                        yield StreamEvent::ToolCallStart { id, name };
                    }
                }
                Some("content_block_delta") => {
                    let idx = data["index"].as_u64().unwrap_or(0);
                    let delta = &data["delta"];
                    match delta["type"].as_str() {
                        Some("text_delta") => {
                            if let Some(t) = delta["text"].as_str() {
                                yield StreamEvent::TextDelta(t.to_string());
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(j) = delta["partial_json"].as_str()
                                && let Some(acc) = tool_blocks.get_mut(&idx) {
                                    acc.args_buf.push_str(j);
                                    yield StreamEvent::ToolCallArgsDelta { id: acc.id.clone(), delta: j.to_string() };
                                }
                        }
                        _ => {}
                    }
                }
                Some("content_block_stop") => {
                    let idx = data["index"].as_u64().unwrap_or(0);
                    if let Some(acc) = tool_blocks.remove(&idx) {
                        yield StreamEvent::ToolCallComplete { id: acc.id, name: acc.name, args: unwrap_args(&acc.args_buf) };
                    }
                }
                Some("message_stop") => break,
                // Anthropic streams failures as an `error` event; treating it
                // as an unknown event ended the turn "successfully" empty.
                Some("error") => {
                    let msg = data["error"]["message"].as_str().unwrap_or("unspecified provider error");
                    tracing::warn!(detail = %data["error"].to_string(), "[wire] provider streamed an error");
                    Err(LychiError::Ai(format!("The AI provider reported an error: {msg}")))?;
                }
                _ => {}
            }
        }
        yield StreamEvent::Done { stop_reason: stop, usage: Some(usage) };
    }
    .boxed()
}

/// Map an OpenAI-compatible SSE byte-stream into normalized `StreamEvent`s.
/// Accumulates `tool_calls` fragments keyed by `index` (id/name may arrive on
/// any early chunk — key by index, never by id). Emits `ToolCallStart` the first
/// time a call's id+name are known, `ToolCallArgsDelta` per fragment, and
/// `ToolCallComplete` for each at `[DONE]`/finish.
pub(crate) fn openai_event_stream<S, B>(
    byte_stream: S,
    model: String,
    cancel: CancellationToken,
) -> super::EventStream
where
    S: futures_util::Stream<Item = reqwest::Result<B>> + Unpin + Send + 'static,
    B: AsRef<[u8]> + Send + 'static,
{
    use futures_util::StreamExt as _;
    async_stream::try_stream! {
        yield StreamEvent::MessageStart { model };
        let mut tool_blocks: std::collections::HashMap<u64, ToolAccum> = std::collections::HashMap::new();
        let mut stop = StopReason::EndTurn;
        let mut usage = super::Usage::default();
        let mut sse = SseReader::new(byte_stream);

        loop {
            if cancel.is_cancelled() { break; }
            let evt = match sse.next_event(&cancel).await? { Some(e) => e, None => break };
            if evt.trim() == "[DONE]" { break; }
            let data: Value = match serde_json::from_str(&evt) { Ok(v) => v, Err(_) => continue };
            // A provider can stream an ERROR chunk mid-stream (Groq does this
            // for failures that occur after headers went out). Skipping it as
            // an unrecognized chunk made the turn end "successfully" empty —
            // the user saw a finished tool call and then silence.
            if let Some(err) = data.get("error").filter(|e| !e.is_null()) {
                let msg = err["message"].as_str().unwrap_or("unspecified provider error");
                tracing::warn!(detail = %err.to_string(), "[wire] provider streamed an error");
                Err(LychiError::Ai(format!("The AI provider reported an error: {msg}")))?;
            }
            // Usage rides on ONE extra chunk at the end (requested via
            // `stream_options.include_usage`), whose `choices` array is empty.
            // Read it before touching `choices` so the empty-array case is a
            // no-op rather than a miss. OpenAI names the fields
            // prompt_/completion_tokens; Anthropic uses input_/output_tokens.
            if let Some(u) = data.get("usage").filter(|u| !u.is_null()) {
                if let Some(n) = u["prompt_tokens"].as_u64() {
                    usage.input_tokens = n as u32;
                }
                if let Some(n) = u["completion_tokens"].as_u64() {
                    usage.output_tokens = n as u32;
                }
                // Groq / OpenAI-dialect prompt-cache hits: how much of the input
                // was served from cache. Makes the stable-prefix optimization
                // visible (0 when the provider omits it).
                if let Some(n) = u["prompt_tokens_details"]["cached_tokens"].as_u64() {
                    usage.cached_input_tokens = n as u32;
                }
            }
            let delta = &data["choices"][0]["delta"];
            if let Some(t) = delta["content"].as_str()
                && !t.is_empty() {
                    yield StreamEvent::TextDelta(t.to_string());
                }
            if let Some(calls) = delta["tool_calls"].as_array() {
                for tc in calls {
                    let idx = tc["index"].as_u64().unwrap_or(0);
                    let acc = tool_blocks.entry(idx).or_default();
                    if let Some(id) = tc["id"].as_str() { acc.id = id.to_string(); }
                    if let Some(name) = tc["function"]["name"].as_str() { acc.name.push_str(name); }
                    if let Some(args) = tc["function"]["arguments"].as_str() {
                        acc.args_buf.push_str(args);
                        // Only emit deltas once we've announced the start.
                        if acc.started {
                            yield StreamEvent::ToolCallArgsDelta { id: acc.id.clone(), delta: args.to_string() };
                        }
                    }
                    // Announce start once id + name are both known.
                    if !acc.started && !acc.id.is_empty() && !acc.name.is_empty() {
                        acc.started = true;
                        stop = StopReason::ToolUse;
                        yield StreamEvent::ToolCallStart { id: acc.id.clone(), name: acc.name.clone() };
                        if !acc.args_buf.is_empty() {
                            yield StreamEvent::ToolCallArgsDelta { id: acc.id.clone(), delta: acc.args_buf.clone() };
                        }
                    }
                }
            }
        }
        // OpenAI has no per-call stop event; flush completes in index order.
        let mut indices: Vec<u64> = tool_blocks.keys().copied().collect();
        indices.sort_unstable();
        for idx in indices {
            let acc = tool_blocks.remove(&idx).unwrap();
            yield StreamEvent::ToolCallComplete { id: acc.id, name: acc.name, args: unwrap_args(&acc.args_buf) };
        }
        yield StreamEvent::Done { stop_reason: stop, usage: Some(usage) };
    }
    .boxed()
}

/// Normalize a tool call's argument JSON into the single string a handler's
/// `execute(args: &str)` receives.
///
/// Two shapes reach here: the uniform `{ "args": string }` (free-text tools) and,
/// for a BOUNDED tool with a typed `input_schema`, a structured object like
/// `{ "action": "volume", "value": "50" }`. For the uniform shape we hand back
/// the `args` string; for the typed shape there is no `args` key, so we pass the
/// whole object JSON through — the handler flattens it (see `system_args_to_flat`).
/// On a parse failure, best-effort return the raw buffer.
/// The OUTGOING mirror of [`unwrap_args`]: re-encode a stored `ToolCall.args`
/// into the wire `arguments` value. A typed call was stored as its bare JSON
/// object — echo it VERBATIM; a legacy flat string gets the uniform
/// `{args: "..."}` wrapper back. Asymmetry here is not cosmetic: gpt-oss saw
/// its own `{"action":"search",…}` call replayed as `{"args":"{\"action\"…}"}`,
/// concluded the interface must be namespaced, and started inventing
/// `web_tools.fetch` — rejected by the provider on every chained call.
pub(crate) fn rewrap_args(args: &str) -> Value {
    match serde_json::from_str::<Value>(args) {
        Ok(v) if v.is_object() => v,
        _ => json!({ "args": args }),
    }
}

pub(crate) fn unwrap_args(buf: &str) -> String {
    if buf.trim().is_empty() {
        return String::new();
    }
    match serde_json::from_str::<Value>(buf) {
        // Uniform `{args: "..."}` → the string. A typed object has no string
        // `args`, so fall through to the object JSON for the handler to parse.
        Ok(v) => v["args"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| buf.to_string()),
        Err(_) => buf.to_string(),
    }
}

// ── SSE reader ───────────────────────────────────────────────────────────────

/// How long a stream may go without delivering a single byte before the
/// connection is presumed dead.
///
/// Generous by design: providers keep long thinking pauses alive with SSE
/// keepalive comments, and even slow local inference emits chunks far more
/// often than this. What the deadline exists for is the connection that will
/// never speak again — NAT expiry, suspend/resume, a wifi switch — which
/// produces no error, ever: the read just parks. Without a deadline that
/// parked read WAS the failure mode: the turn never ended, Esc appeared to
/// work (the UI resets by generation) while the loop task and socket leaked,
/// and every retry parked another.
const SSE_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Minimal Server-Sent Events reader over a reqwest byte stream. Buffers across
/// chunk boundaries (a `data:` line can split mid-token between TCP frames) and
/// yields the payload after each event. Comment/keepalive lines and the
/// `event:`/`id:` fields are ignored — we only need the JSON `data` payloads.
struct SseReader<S> {
    stream: S,
    buf: String,
}

impl<S, B> SseReader<S>
where
    S: futures_util::Stream<Item = reqwest::Result<B>> + Unpin,
    B: AsRef<[u8]>,
{
    fn new(stream: S) -> Self {
        Self {
            stream,
            buf: String::new(),
        }
    }

    /// Return the next SSE `data:` payload, or `None` at end of stream (or on
    /// cancellation — the caller's break path is the same either way).
    ///
    /// The chunk read is RACED against the token and [`SSE_IDLE_TIMEOUT`], not
    /// checked before it: a sequential `is_cancelled()` guard can never fire
    /// while the read is parked on a silent connection, which is exactly where
    /// a dead peer parks it forever. The event loops' own between-events check
    /// still exists for the buffered-data case this method returns early from.
    async fn next_event(
        &mut self,
        cancel: &CancellationToken,
    ) -> Result<Option<String>, LychiError> {
        loop {
            if let Some(pos) = find_event_boundary(&self.buf) {
                let raw = self.buf[..pos].to_string();
                let advance = if self.buf[pos..].starts_with("\r\n\r\n") {
                    4
                } else {
                    2
                };
                self.buf.drain(..pos + advance);
                if let Some(data) = parse_sse_data(&raw) {
                    return Ok(Some(data));
                }
                continue; // comment/keepalive block — no data line
            }
            // Each iteration arms a fresh deadline, so it measures silence
            // since the last chunk — keepalive comments reset it.
            let chunk = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(None),
                _ = tokio::time::sleep(SSE_IDLE_TIMEOUT) => {
                    return Err(LychiError::Ai(format!(
                        "stream error: no data for {}s — connection presumed dead",
                        SSE_IDLE_TIMEOUT.as_secs()
                    )));
                }
                c = self.stream.next() => c,
            };
            match chunk {
                Some(Ok(chunk)) => {
                    self.buf.push_str(&String::from_utf8_lossy(chunk.as_ref()));
                }
                Some(Err(e)) => return Err(LychiError::Ai(format!("stream error: {e}"))),
                None => {
                    if self.buf.trim().is_empty() {
                        return Ok(None);
                    }
                    let raw = std::mem::take(&mut self.buf);
                    return Ok(parse_sse_data(&raw));
                }
            }
        }
    }
}

/// Find the byte index of the first event terminator (`\n\n` or `\r\n\r\n`).
fn find_event_boundary(buf: &str) -> Option<usize> {
    let a = buf.find("\n\n");
    let b = buf.find("\r\n\r\n");
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Extract and join the `data:` line(s) of one SSE event block. Returns None if
/// the block has no data line (bare `event:` or a `:` comment).
fn parse_sse_data(block: &str) -> Option<String> {
    let mut data = String::new();
    let mut found = false;
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            found = true;
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    found.then_some(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{CancellationToken, ContentPart, StopReason, StreamEvent, ToolCall};

    #[test]
    fn retry_delay_is_flat_and_only_shortened_by_the_header() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

        // Header present and sane → honoured verbatim (clamped to the ceiling).
        // A header LONGER than the flat interval is ignored — quick polite
        // probes beat one long obedient wait (see RETRY_INTERVAL's doc).
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("8"));
        assert_eq!(retry_delay(&h, 1), RETRY_INTERVAL);

        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("600"));
        assert_eq!(retry_delay(&h, 1), RETRY_INTERVAL);

        // A header promising a SOONER clear shortens the wait.
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("2"));
        assert_eq!(retry_delay(&h, 1), std::time::Duration::from_secs(2));

        // No header → the flat interval, regardless of attempt.
        let none = HeaderMap::new();
        assert_eq!(retry_delay(&none, 1), RETRY_INTERVAL);
        assert_eq!(retry_delay(&none, 7), RETRY_INTERVAL);

        // A non-numeric header (HTTP-date form we don't parse) falls back too.
        let mut h = HeaderMap::new();
        h.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT"),
        );
        assert_eq!(retry_delay(&h, 2), RETRY_INTERVAL);
    }

    #[test]
    fn retriable_statuses_are_transient_across_providers() {
        // Rate limit (all), Anthropic overloaded, and gateway/unavailable — the
        // provider-agnostic "back off and retry" set.
        for s in [429, 529, 503, 502, 504] {
            assert!(is_retriable_status(s), "{s} should be retriable");
        }
        // Hard errors must NOT be retried — retrying only delays the same failure.
        for s in [200, 400, 401, 403, 404, 413, 500] {
            assert!(!is_retriable_status(s), "{s} must not be retriable");
        }
    }

    #[test]
    fn sse_parse_data_joins_multiline_and_strips_prefix() {
        assert_eq!(parse_sse_data("data: hello").as_deref(), Some("hello"));
        assert_eq!(parse_sse_data("data:hello").as_deref(), Some("hello"));
        assert_eq!(
            parse_sse_data("data: line1\ndata: line2").as_deref(),
            Some("line1\nline2")
        );
        assert_eq!(parse_sse_data(": keepalive"), None);
        assert_eq!(parse_sse_data("event: ping"), None);
    }

    #[test]
    fn sse_boundary_finds_earliest_terminator() {
        assert_eq!(find_event_boundary("a\n\nb"), Some(1));
        assert_eq!(find_event_boundary("a\r\n\r\nb"), Some(1));
        assert_eq!(find_event_boundary("no boundary yet"), None);
    }

    #[tokio::test]
    async fn sse_reader_reassembles_across_chunk_boundaries() {
        let chunks: Vec<reqwest::Result<Vec<u8>>> = vec![
            Ok(b"data: {\"a\":".to_vec()),
            Ok(b"1}\n\ndata: {\"b\":2}\n\n".to_vec()),
            Ok(b": keepalive\n\n".to_vec()),
            Ok(b"data: [DONE]\n\n".to_vec()),
        ];
        let stream = futures_util::stream::iter(chunks);
        let mut sse = SseReader::new(stream);
        let cancel = CancellationToken::new();
        assert_eq!(
            sse.next_event(&cancel).await.unwrap().as_deref(),
            Some("{\"a\":1}")
        );
        assert_eq!(
            sse.next_event(&cancel).await.unwrap().as_deref(),
            Some("{\"b\":2}")
        );
        assert_eq!(
            sse.next_event(&cancel).await.unwrap().as_deref(),
            Some("[DONE]")
        );
        assert_eq!(sse.next_event(&cancel).await.unwrap(), None);
    }

    /// AI-2, half one: a connection that goes silent produces no error, ever —
    /// the read just parks. It must become a classified stream error at the
    /// idle deadline instead of a forever-hung turn. (`start_paused` lets the
    /// 90s deadline elapse instantly once nothing else can make progress.)
    #[tokio::test(start_paused = true)]
    async fn a_silent_connection_times_out_instead_of_parking_forever() {
        let stream = futures_util::stream::pending::<reqwest::Result<Vec<u8>>>();
        let mut sse = SseReader::new(stream);
        let cancel = CancellationToken::new();
        let err = sse.next_event(&cancel).await.unwrap_err();
        assert!(
            format!("{err:?}").contains("presumed dead"),
            "expected the idle-deadline error, got: {err:?}"
        );
    }

    /// AI-2, half two: Esc must reach a read parked on a silent connection.
    /// The old shape checked the token only BETWEEN reads, so cancel_ai_chat
    /// fired a token nothing would ever poll again.
    #[tokio::test(start_paused = true)]
    async fn cancel_unparks_a_read_on_a_silent_connection() {
        let stream = futures_util::stream::pending::<reqwest::Result<Vec<u8>>>();
        let mut sse = SseReader::new(stream);
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            c2.cancel();
        });
        assert_eq!(
            sse.next_event(&cancel).await.unwrap(),
            None,
            "cancellation must end the stream promptly, not wait out the idle deadline"
        );
    }

    // Collect a driver's events into a Vec for assertions.
    async fn collect(stream: super::super::EventStream) -> Vec<StreamEvent> {
        use futures_util::StreamExt as _;
        stream.filter_map(|r| async move { r.ok() }).collect().await
    }

    // Canned Anthropic SSE → text deltas + a tool call + Done.
    #[tokio::test]
    async fn anthropic_stream_yields_text_then_done() {
        let sse = concat!(
            "data: {\"type\":\"message_start\"}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let chunks: Vec<reqwest::Result<Vec<u8>>> = vec![Ok(sse.as_bytes().to_vec())];
        let stream = super::anthropic_event_stream(
            futures_util::stream::iter(chunks),
            "test-model".into(),
            CancellationToken::new(),
        );
        let events = collect(stream).await;
        // MessageStart, TextDelta(Hel), TextDelta(lo), Done{EndTurn}
        assert!(matches!(events[0], StreamEvent::MessageStart { .. }));
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello");
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                stop_reason: StopReason::EndTurn,
                ..
            })
        ));
    }

    // message_delta carries the authoritative stop_reason (max_tokens) + usage.
    #[tokio::test]
    async fn openai_stream_reports_usage_from_the_final_chunk() {
        // Groq/OpenAI send usage on ONE extra chunk after the content, whose
        // `choices` array is empty. Every earlier chunk carries `usage: null`.
        // Without parsing this the chat showed no token count at all.
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}],\"usage\":null}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":31,\"completion_tokens\":7,\"total_tokens\":38}}\n\n",
            "data: [DONE]\n\n",
        );
        let chunks: Vec<reqwest::Result<Vec<u8>>> = vec![Ok(sse.as_bytes().to_vec())];
        let stream = super::openai_event_stream(
            futures_util::stream::iter(chunks),
            "llama".into(),
            CancellationToken::new(),
        );
        let events = collect(stream).await;
        match events.last() {
            Some(StreamEvent::Done { usage, .. }) => {
                let u = usage.expect("usage reported");
                assert_eq!(u.input_tokens, 31);
                assert_eq!(u.output_tokens, 7);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn openai_stream_without_usage_still_completes() {
        // A provider that ignores `stream_options` (or a cancelled stream that
        // never reaches the final chunk) must still terminate cleanly, reporting
        // zeros rather than failing.
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let chunks: Vec<reqwest::Result<Vec<u8>>> = vec![Ok(sse.as_bytes().to_vec())];
        let stream = super::openai_event_stream(
            futures_util::stream::iter(chunks),
            "llama".into(),
            CancellationToken::new(),
        );
        match collect(stream).await.last() {
            Some(StreamEvent::Done { usage, .. }) => {
                let u = usage.expect("usage present but zeroed");
                assert_eq!(u.input_tokens, 0);
                assert_eq!(u.output_tokens, 0);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn openai_requests_ask_for_usage_explicitly() {
        // The parser above is useless unless the request opts in — usage is
        // omitted from OpenAI-compatible streams by default.
        let body = super::build_body(super::Dialect::OpenAi, "llama", 100, &[], &[]);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[tokio::test]
    async fn anthropic_stream_reports_truncation_and_usage() {
        let sse = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":42}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"cut off\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":300}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let chunks: Vec<reqwest::Result<Vec<u8>>> = vec![Ok(sse.as_bytes().to_vec())];
        let stream = super::anthropic_event_stream(
            futures_util::stream::iter(chunks),
            "m".into(),
            CancellationToken::new(),
        );
        let events = collect(stream).await;
        match events.last() {
            Some(StreamEvent::Done { stop_reason, usage }) => {
                assert_eq!(*stop_reason, StopReason::MaxTokens);
                let u = usage.expect("usage reported");
                assert_eq!(u.input_tokens, 42);
                assert_eq!(u.output_tokens, 300);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn anthropic_stream_yields_tool_call() {
        let sse = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"open\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"args\\\":\\\"fire\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"fox\\\"}\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let chunks: Vec<reqwest::Result<Vec<u8>>> = vec![Ok(sse.as_bytes().to_vec())];
        let events = collect(super::anthropic_event_stream(
            futures_util::stream::iter(chunks),
            "m".into(),
            CancellationToken::new(),
        ))
        .await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, StreamEvent::ToolCallStart { name, .. } if name == "open"))
        );
        let complete = events.iter().find_map(|e| match e {
            StreamEvent::ToolCallComplete { name, args, .. } => Some((name.clone(), args.clone())),
            _ => None,
        });
        assert_eq!(complete, Some(("open".into(), "firefox".into())));
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                stop_reason: StopReason::ToolUse,
                ..
            })
        ));
    }

    // Canned OpenAI SSE with fragmented tool-call args (id/name on first chunk).
    #[tokio::test]
    async fn openai_stream_accumulates_fragmented_tool_call() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"web\",\"arguments\":\"{\\\"args\\\":\\\"ru\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"st\\\"}\"}}]}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let chunks: Vec<reqwest::Result<Vec<u8>>> = vec![Ok(sse.as_bytes().to_vec())];
        let events = collect(super::openai_event_stream(
            futures_util::stream::iter(chunks),
            "m".into(),
            CancellationToken::new(),
        ))
        .await;
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "hi");
        let complete = events.iter().find_map(|e| match e {
            StreamEvent::ToolCallComplete { name, args, .. } => Some((name.clone(), args.clone())),
            _ => None,
        });
        assert_eq!(complete, Some(("web".into(), "rust".into())));
    }

    // Cancellation: a cancelled token stops the stream promptly (Done or earlier).
    #[tokio::test]
    async fn cancelled_token_stops_stream() {
        let sse = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n";
        let chunks: Vec<reqwest::Result<Vec<u8>>> = vec![Ok(sse.as_bytes().to_vec())];
        let cancel = CancellationToken::new();
        cancel.cancel(); // already cancelled
        let events = collect(super::anthropic_event_stream(
            futures_util::stream::iter(chunks),
            "m".into(),
            cancel,
        ))
        .await;
        // Should stop early — no text delta emitted (cancel checked before first read).
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, StreamEvent::TextDelta(_)))
        );
    }

    #[test]
    fn tool_calls_round_trip_in_their_original_shape() {
        // Typed call: the bare JSON object the model emitted must replay
        // VERBATIM — re-wrapping it taught gpt-oss a phantom interface and it
        // began inventing `web_tools.fetch`.
        let typed = r#"{"action":"search","query":"cm of tamil nadu"}"#;
        assert_eq!(
            rewrap_args(typed),
            serde_json::from_str::<Value>(typed).unwrap()
        );
        // Legacy flat args get the uniform wrapper back.
        assert_eq!(rewrap_args("ls -la"), json!({ "args": "ls -la" }));

        // And through the full message encoders, both dialects.
        let mut msg = ChatMessage::assistant("");
        msg.tool_calls = vec![ToolCall {
            id: "c1".into(),
            name: "web_tools".into(),
            args: typed.into(),
        }];
        let openai = openai_messages(std::slice::from_ref(&msg));
        assert_eq!(
            openai[0]["tool_calls"][0]["function"]["arguments"],
            serde_json::to_string(&serde_json::from_str::<Value>(typed).unwrap()).unwrap()
        );
        let anthropic = anthropic_messages(&[msg]);
        assert_eq!(
            anthropic[0]["content"][0]["input"]["action"], "search",
            "{anthropic:?}"
        );
    }

    #[test]
    fn unwrap_args_extracts_single_arg_and_tolerates_junk() {
        assert_eq!(unwrap_args("{\"args\":\"hello\"}"), "hello");
        assert_eq!(unwrap_args(""), "");
        assert_eq!(unwrap_args("not json"), "not json");
    }

    #[test]
    fn anthropic_encodes_user_image_as_base64_source_block() {
        let msg = ChatMessage::user_with_images(
            "what is this?",
            vec![super::super::ImageSource {
                media_type: "image/png".into(),
                data: "QUJD".into(),
            }],
        );
        let wire = anthropic_messages(&[msg]);
        assert_eq!(wire.len(), 1);
        let content = &wire[0]["content"];
        assert!(content.is_array());
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "what is this?");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], "QUJD");
    }

    #[test]
    fn openai_encodes_user_image_as_data_uri() {
        let msg = ChatMessage::user_with_images(
            "describe",
            vec![super::super::ImageSource {
                media_type: "image/jpeg".into(),
                data: "QUJD".into(),
            }],
        );
        let wire = openai_messages(&[msg]);
        assert_eq!(wire.len(), 1);
        let content = &wire[0]["content"];
        assert!(content.is_array());
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(
            content[1]["image_url"]["url"],
            "data:image/jpeg;base64,QUJD"
        );
    }

    #[test]
    fn text_only_user_still_encodes_as_bare_string() {
        // No images → the compact string form (no needless array) — except the
        // FINAL message, whose content becomes a block array to carry the
        // history cache breakpoint. Prove both: a non-final message stays a
        // bare string, the final one is tagged.
        let msgs = vec![ChatMessage::user("hello"), ChatMessage::assistant("hi")];
        let wire = anthropic_messages(&msgs);
        assert_eq!(wire[0]["content"], "hello");
        assert_eq!(
            wire[1]["content"][0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
        assert_eq!(wire[1]["content"][0]["text"], "hi");
        // OpenAI keeps the compact form everywhere (its caching is automatic).
        assert_eq!(
            openai_messages(&[ChatMessage::user("hello")])[0]["content"],
            "hello"
        );
    }

    #[test]
    fn history_breakpoint_rides_the_last_tool_result() {
        // The common agent-loop shape: the request ends on tool results. The
        // breakpoint must land on the LAST result block of that user message.
        let msgs = vec![
            ChatMessage::user("do things"),
            ChatMessage::tool_result("c1", "one".to_string(), false),
            ChatMessage::tool_result("c2", "two".to_string(), false),
        ];
        let wire = anthropic_messages(&msgs);
        let blocks = wire.last().unwrap()["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0]["cache_control"].is_null());
        assert_eq!(blocks[1]["cache_control"], json!({ "type": "ephemeral" }));
    }

    #[test]
    fn legacy_string_content_deserializes() {
        // History persisted BEFORE the multimodal migration stored `content` as a
        // bare JSON string. It must still load into a single Text part.
        let json = r#"{"role":"user","content":"old message"}"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.content, vec![ContentPart::text("old message")]);
        assert_eq!(msg.content_text(), "old message");
        assert!(!msg.has_images());
        // New serialization is the block-array form.
        let out = serde_json::to_string(&msg).unwrap();
        assert!(out.contains(r#""type":"text""#));
    }

    #[test]
    fn anthropic_messages_round_trip_tool_calls() {
        let msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("open firefox"),
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentPart::text("I'll open it")],
                tool_call_id: None,
                tool_calls: vec![ToolCall {
                    id: "t1".into(),
                    name: "open".into(),
                    args: "firefox".into(),
                }],
                is_error: false,
                display: None,
            },
            ChatMessage::tool_result("t1", "opened", false),
        ];
        let wire = anthropic_messages(&msgs);
        assert_eq!(wire.len(), 3);
        assert_eq!(wire[1]["content"][1]["type"], "tool_use");
        assert_eq!(wire[1]["content"][1]["input"]["args"], "firefox");
        assert_eq!(wire[2]["content"][0]["type"], "tool_result");
        assert_eq!(wire[2]["content"][0]["tool_use_id"], "t1");
        assert_eq!(anthropic_system(&msgs), "sys");
    }

    #[test]
    fn openai_messages_round_trip_tool_calls() {
        let msgs = vec![
            ChatMessage::user("open firefox"),
            ChatMessage {
                role: Role::Assistant,
                content: vec![],
                tool_call_id: None,
                tool_calls: vec![ToolCall {
                    id: "t1".into(),
                    name: "open".into(),
                    args: "firefox".into(),
                }],
                is_error: false,
                display: None,
            },
            ChatMessage::tool_result("t1", "opened", false),
        ];
        let wire = openai_messages(&msgs);
        assert_eq!(wire.len(), 3);
        assert_eq!(wire[1]["tool_calls"][0]["function"]["name"], "open");
        assert_eq!(
            wire[1]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap(),
            "{\"args\":\"firefox\"}"
        );
        assert_eq!(wire[2]["role"], "tool");
        assert_eq!(wire[2]["tool_call_id"], "t1");
    }

    #[test]
    fn tool_schemas_use_uniform_args_shape() {
        let tools = vec![ToolDef {
            name: "open".into(),
            description: "Open an app".into(),
            mutates: false,
            mutating_actions: Vec::new(),
            input_schema: None,
        }];
        assert_eq!(
            anthropic_tools(&tools)[0]["input_schema"]["required"][0],
            "args"
        );
        assert_eq!(
            openai_tools(&tools)[0]["function"]["parameters"]["properties"]["args"]["type"],
            "string"
        );
    }

    #[test]
    fn a_typed_tool_emits_its_schema_and_strict_on_anthropic() {
        let schema = json!({
            "type": "object",
            "properties": { "action": { "type": "string", "enum": ["volume", "mute"] } },
            "required": ["action"],
            "additionalProperties": false
        });
        let tools = vec![ToolDef {
            name: "system".into(),
            description: "System controls".into(),
            mutates: false,
            mutating_actions: Vec::new(),
            input_schema: Some(schema),
        }];
        let a = &anthropic_tools(&tools)[0];
        // The real schema flows through (not the uniform {args}).
        assert_eq!(
            a["input_schema"]["properties"]["action"]["enum"][0],
            "volume"
        );
        // Typed tools get strict:true (the cloud enforcement); free-text don't.
        assert_eq!(a["strict"], true);
        // OpenAI carries the schema under function.parameters too.
        let o = &openai_tools(&tools)[0];
        assert_eq!(
            o["function"]["parameters"]["properties"]["action"]["enum"][1],
            "mute"
        );
    }

    #[test]
    fn build_body_shapes_per_dialect() {
        let msgs = vec![ChatMessage::system("sys"), ChatMessage::user("hi")];
        let tools = vec![ToolDef {
            name: "open".into(),
            description: "d".into(),
            mutates: false,
            mutating_actions: Vec::new(),
            input_schema: None,
        }];

        // Anthropic: system out-of-band as cache-aware text blocks, tools present.
        let a = build_body(Dialect::Anthropic, "claude", 100, &msgs, &tools);
        assert_eq!(a["model"], "claude");
        assert_eq!(a["stream"], true);
        // system is now a block array; the single (stable) block carries the
        // prompt-cache breakpoint.
        assert_eq!(a["system"][0]["type"], "text");
        assert_eq!(a["system"][0]["text"], "sys");
        assert_eq!(a["system"][0]["cache_control"]["type"], "ephemeral");
        // tools present, and the LAST tool carries the cache breakpoint too.
        assert!(a["tools"].is_array());
        let last = a["tools"].as_array().unwrap().last().unwrap();
        assert_eq!(last["cache_control"]["type"], "ephemeral");
        // system message is NOT in the messages array (out-of-band).
        assert_eq!(a["messages"].as_array().unwrap().len(), 1);

        // OpenAI: system stays in messages, tools present.
        let o = build_body(Dialect::OpenAi, "gpt", 100, &msgs, &tools);
        assert_eq!(o["messages"].as_array().unwrap().len(), 2);
        assert!(o["tools"].is_array());
        assert!(o["system"].is_null());

        // No tools → no `tools` key.
        let no_tools = build_body(Dialect::OpenAi, "gpt", 100, &msgs, &[]);
        assert!(no_tools["tools"].is_null());
    }

    #[test]
    fn system_is_one_cached_block() {
        // The system prompt is fully stable (the volatile hint rides a trailing
        // message, not the system prompt), so it is a single cached block.
        let msgs = vec![
            ChatMessage::system("stable persona"),
            ChatMessage::user("hi"),
        ];
        let a = build_body(Dialect::Anthropic, "claude", 100, &msgs, &[]);
        let blocks = a["system"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["text"], "stable persona");
        assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");

        // OpenAI: plain system message.
        let o = build_body(Dialect::OpenAi, "gpt", 100, &msgs, &[]);
        assert_eq!(o["messages"][0]["content"], "stable persona");
    }

    #[test]
    fn anthropic_strict_drops_when_optional_params_exceed_the_budget() {
        // 5 schemas x 5 optionals = 25 > 24 → strict must be absent on ALL.
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string"},
                "a": {"type": "string"}, "b": {"type": "string"},
                "c": {"type": "string"}, "d": {"type": "string"},
                "e": {"type": "string"}
            },
            "required": ["action"],
            "additionalProperties": false
        });
        let big: Vec<ToolDef> = (0..5)
            .map(|i| ToolDef {
                name: format!("t{i}"),
                description: "x".into(),
                mutates: false,
                mutating_actions: vec![],
                input_schema: Some(schema.clone()),
            })
            .collect();
        for tool in anthropic_tools(&big) {
            assert!(tool.get("strict").is_none(), "over budget → no strict");
        }
        // A small set stays strict.
        let small = vec![big[0].clone()];
        assert_eq!(
            anthropic_tools(&small)[0]["strict"],
            serde_json::json!(true)
        );
    }
}
