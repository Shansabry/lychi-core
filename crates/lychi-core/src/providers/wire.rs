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

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};

use crate::error::LychiError;

use super::{CancellationToken, ChatMessage, EventStream, Role, StopReason, StreamEvent, ToolDef};

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
        }
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

        // Build the wire body up-front (pure, no IO).
        let body = build_body(dialect, &model, self.max_tokens, messages, tools);

        async_stream::try_stream! {
            let mut req = http.post(&url).header("Content-Type", "application/json");
            req = match &auth {
                AuthStyle::Bearer(k) => req.header("Authorization", format!("Bearer {k}")),
                AuthStyle::AnthropicKey(k) => req
                    .header("x-api-key", k)
                    .header("anthropic-version", "2023-06-01"),
                AuthStyle::None => req,
            };
            let resp = req
                .json(&body)
                .send()
                .await
                .map_err(|e| LychiError::Ai(format!("HTTP request failed: {e}")))?;
            let status = resp.status();
            if !status.is_success() {
                // Error path consumes `resp` (reads the body) and diverges.
                let text = resp.text().await.unwrap_or_default();
                Err(LychiError::Ai(format!("API returned {status}: {text}")))?;
                return; // unreachable after `?`, but makes the divergence explicit
            }
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
            let system = anthropic_system(messages);
            let mut b = json!({
                "model": model, "max_tokens": max_tokens, "stream": true,
                "messages": anthropic_messages(messages),
            });
            if !system.is_empty() {
                b["system"] = json!(system);
            }
            if !tools.is_empty() {
                b["tools"] = json!(anthropic_tools(tools));
            }
            b
        }
        Dialect::OpenAi => {
            let mut b = json!({
                "model": model, "max_tokens": max_tokens, "stream": true,
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
            Role::User => out.push(json!({ "role": "user", "content": m.content })),
            Role::Assistant => {
                if m.tool_calls.is_empty() {
                    out.push(json!({ "role": "assistant", "content": m.content }));
                } else {
                    let mut blocks: Vec<Value> = Vec::new();
                    if !m.content.is_empty() {
                        blocks.push(json!({ "type": "text", "text": m.content }));
                    }
                    for tc in &m.tool_calls {
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": { "args": tc.args },
                        }));
                    }
                    out.push(json!({ "role": "assistant", "content": blocks }));
                }
            }
            Role::Tool => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content,
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
    out
}

/// Collect all System messages into one string for Anthropic's top-level
/// `system` field (it takes the system prompt out-of-band, not as a turn).
pub(crate) fn anthropic_system(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Serialize the message history to OpenAI wire format. Tool results are separate
/// `role:"tool"` messages; assistant tool calls go in the `tool_calls` array.
pub(crate) fn openai_messages(messages: &[ChatMessage]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        match m.role {
            Role::System => out.push(json!({ "role": "system", "content": m.content })),
            Role::User => out.push(json!({ "role": "user", "content": m.content })),
            Role::Assistant => {
                if m.tool_calls.is_empty() {
                    out.push(json!({ "role": "assistant", "content": m.content }));
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
                                    "arguments": json!({ "args": tc.args }).to_string(),
                                },
                            })
                        })
                        .collect();
                    out.push(json!({
                        "role": "assistant",
                        "content": if m.content.is_empty() { Value::Null } else { json!(m.content) },
                        "tool_calls": calls,
                    }));
                }
            }
            Role::Tool => out.push(json!({
                "role": "tool",
                "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                "content": m.content,
            })),
        }
    }
    out
}

/// Anthropic tool schema — uniform `{ args: string }` input for every Lychi tool.
pub(crate) fn anthropic_tools(tools: &[ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": {
                    "type": "object",
                    "properties": { "args": { "type": "string", "description": "The command arguments." } },
                    "required": ["args"],
                },
            })
        })
        .collect()
}

/// OpenAI tool schema — same uniform `{ args: string }` shape.
pub(crate) fn openai_tools(tools: &[ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": {
                        "type": "object",
                        "properties": { "args": { "type": "string", "description": "The command arguments." } },
                        "required": ["args"],
                    },
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
/// `Done`. Honors `cancel` between reads (HTTP drop-cancel also applies).
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
            let evt = match sse.next_event().await? { Some(e) => e, None => break };
            let data: Value = match serde_json::from_str(&evt) { Ok(v) => v, Err(_) => continue };
            match data["type"].as_str() {
                Some("message_start") => {
                    // input_tokens is reported here; output_tokens accrues in message_delta.
                    if let Some(n) = data["message"]["usage"]["input_tokens"].as_u64() {
                        usage.input_tokens = n as u32;
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
                            if let Some(j) = delta["partial_json"].as_str() {
                                if let Some(acc) = tool_blocks.get_mut(&idx) {
                                    acc.args_buf.push_str(j);
                                    yield StreamEvent::ToolCallArgsDelta { id: acc.id.clone(), delta: j.to_string() };
                                }
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
        let mut sse = SseReader::new(byte_stream);

        loop {
            if cancel.is_cancelled() { break; }
            let evt = match sse.next_event().await? { Some(e) => e, None => break };
            if evt.trim() == "[DONE]" { break; }
            let data: Value = match serde_json::from_str(&evt) { Ok(v) => v, Err(_) => continue };
            let delta = &data["choices"][0]["delta"];
            if let Some(t) = delta["content"].as_str() {
                if !t.is_empty() {
                    yield StreamEvent::TextDelta(t.to_string());
                }
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
        yield StreamEvent::Done { stop_reason: stop, usage: None };
    }
    .boxed()
}

/// Extract the single `args` string from a tool call's argument JSON. Every Lychi
/// tool has the uniform schema `{ "args": string }`; on parse failure or a
/// different shape, fall back to the raw buffer (best-effort).
pub(crate) fn unwrap_args(buf: &str) -> String {
    if buf.trim().is_empty() {
        return String::new();
    }
    match serde_json::from_str::<Value>(buf) {
        Ok(v) => v["args"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| buf.to_string()),
        Err(_) => buf.to_string(),
    }
}

// ── SSE reader ───────────────────────────────────────────────────────────────

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

    /// Return the next SSE `data:` payload, or `None` at end of stream.
    async fn next_event(&mut self) -> Result<Option<String>, LychiError> {
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
            match self.stream.next().await {
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
    use crate::providers::{CancellationToken, StopReason, StreamEvent, ToolCall};

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
        assert_eq!(
            sse.next_event().await.unwrap().as_deref(),
            Some("{\"a\":1}")
        );
        assert_eq!(
            sse.next_event().await.unwrap().as_deref(),
            Some("{\"b\":2}")
        );
        assert_eq!(sse.next_event().await.unwrap().as_deref(), Some("[DONE]"));
        assert_eq!(sse.next_event().await.unwrap(), None);
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
    fn unwrap_args_extracts_single_arg_and_tolerates_junk() {
        assert_eq!(unwrap_args("{\"args\":\"hello\"}"), "hello");
        assert_eq!(unwrap_args(""), "");
        assert_eq!(unwrap_args("not json"), "not json");
    }

    #[test]
    fn anthropic_messages_round_trip_tool_calls() {
        let msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("open firefox"),
            ChatMessage {
                role: Role::Assistant,
                content: "I'll open it".into(),
                tool_call_id: None,
                tool_calls: vec![ToolCall {
                    id: "t1".into(),
                    name: "open".into(),
                    args: "firefox".into(),
                }],
                is_error: false,
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
                content: String::new(),
                tool_call_id: None,
                tool_calls: vec![ToolCall {
                    id: "t1".into(),
                    name: "open".into(),
                    args: "firefox".into(),
                }],
                is_error: false,
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
    fn build_body_shapes_per_dialect() {
        let msgs = vec![ChatMessage::system("sys"), ChatMessage::user("hi")];
        let tools = vec![ToolDef {
            name: "open".into(),
            description: "d".into(),
        }];

        // Anthropic: system out-of-band, tools present, stream:true.
        let a = build_body(Dialect::Anthropic, "claude", 100, &msgs, &tools);
        assert_eq!(a["model"], "claude");
        assert_eq!(a["stream"], true);
        assert_eq!(a["system"], "sys");
        assert!(a["tools"].is_array());
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
}
