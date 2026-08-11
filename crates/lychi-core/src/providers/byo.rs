use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;

use super::wire::{AuthStyle, Dialect, WireClient};
use super::{AiProvider, CancellationToken, ChatMessage, EventStream, ToolDef};

/// Request/response wire format spoken to the endpoint.
///
/// This is intentionally decoupled from any specific vendor: OpenAI, Groq,
/// Grok, Gemini (OpenAI-compat mode) and OpenRouter all speak `OpenAi`; only
/// Anthropic's native Messages API differs. Adding a new OpenAI-compatible
/// provider needs NO code change — just a base URL + this format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireFormat {
    OpenAi,
    Anthropic,
}

impl WireFormat {
    /// Parse a wire-format string. Unknown values fall back to OpenAI, which is
    /// the de-facto standard for third-party endpoints.
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "anthropic" => Self::Anthropic,
            // "openai", "gemini" (openai-compat), "groq", "" and anything else
            _ => Self::OpenAi,
        }
    }
}

/// BYO API key provider — provider-agnostic. Speaks either the OpenAI-compatible
/// chat-completions API or the Anthropic Messages API against an arbitrary base
/// URL, with a user-supplied model string. No hardcoded provider or model list.
pub struct BYOClient {
    /// Human-readable provider id (preset id, e.g. "openai", "grok", "custom").
    /// Display/logging only — behavior is driven by `wire` + `base_url`.
    provider_id: String,
    wire: WireFormat,
    base_url: String,
    model: String,
    api_key: String,
    max_tokens: u32,
    http: Client,
    /// Optional store for LEARNED model capabilities. When present, a rejection
    /// that proves this model can't read images is recorded so the next attach
    /// is warned before a request is wasted (see `providers::capability`).
    /// `None` keeps the client usable in tests and anywhere without a DB.
    caps_db: Option<std::sync::Arc<redb::Database>>,
}

impl BYOClient {
    /// Construct a BYO client.
    ///
    /// - `provider_id`: preset id for display/logging (e.g. "openai", "grok").
    /// - `wire`: which API dialect to speak.
    /// - `base_url`: full endpoint URL (chat-completions or messages endpoint).
    /// - `model`: free-form model identifier the endpoint accepts.
    pub fn new(
        provider_id: impl Into<String>,
        wire: WireFormat,
        base_url: impl Into<String>,
        model: String,
        api_key: String,
        max_tokens: u32,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            wire,
            base_url: base_url.into(),
            model,
            api_key,
            max_tokens,
            // connect_timeout only — never a total request timeout, which
            // would sever legitimate long streams mid-answer. A peer that
            // connects and then goes silent is the SSE layer's problem
            // (SSE_IDLE_TIMEOUT in wire.rs); a peer we can't reach at all
            // must fail in seconds, not whenever the OS gives up. A bare
            // Client::new() here meant an unroutable endpoint hung the turn
            // for the kernel's own connect patience.
            http: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            caps_db: None,
        }
    }

    /// Attach the capability store so failures teach us about this model.
    pub fn with_capability_store(mut self, db: std::sync::Arc<redb::Database>) -> Self {
        self.caps_db = Some(db);
        self
    }

    /// Build the shared streaming client for this provider's dialect + auth. The
    /// whole HTTP→SSE→event mechanism lives in `WireClient`; this just maps the
    /// BYO config (wire format + key) onto it.
    fn wire_client(&self) -> WireClient {
        let (dialect, auth) = match self.wire {
            WireFormat::Anthropic => (
                Dialect::Anthropic,
                AuthStyle::AnthropicKey(self.api_key.clone()),
            ),
            WireFormat::OpenAi => (Dialect::OpenAi, AuthStyle::Bearer(self.api_key.clone())),
        };
        // When a store is attached, a failure that proves this model can't read
        // images is recorded against `<provider>/<model>` — so the NEXT attach
        // is warned up-front instead of spending another rejected request.
        let observer = self.caps_db.clone().map(|db| {
            let provider = self.provider_id.clone();
            let model = self.model.clone();
            let obs: super::wire::ErrorObserver = std::sync::Arc::new(move |err| {
                if err.kind == super::errors::AiErrorKind::VisionUnsupported {
                    let _ = super::capability::record(
                        &db,
                        &provider,
                        &model,
                        super::capability::Vision::Unsupported,
                        /* from_metadata */ false,
                    );
                }
            });
            obs
        });
        WireClient::new(
            self.http.clone(),
            dialect,
            self.base_url.clone(),
            self.model.clone(),
            self.max_tokens,
            auth,
        )
        .with_error_observer(observer)
    }
}

#[async_trait]
impl AiProvider for BYOClient {
    async fn health_check(&self) -> bool {
        let result = match self.wire {
            WireFormat::Anthropic => {
                let res = self
                    .http
                    .post(&self.base_url)
                    .header("x-api-key", &self.api_key)
                    .header("anthropic-version", "2023-06-01")
                    .header("content-type", "application/json")
                    .body(format!(
                        r#"{{"model":"{}","max_tokens":1,"messages":[{{"role":"user","content":"hi"}}]}}"#,
                        self.model
                    ))
                    .send()
                    .await;
                match res {
                    Ok(r) => {
                        let status = r.status().as_u16();
                        status != 401 && status != 403
                    }
                    Err(_) => false,
                }
            }
            WireFormat::OpenAi => {
                if let Some(models_url) = openai_models_url(&self.base_url) {
                    self.http
                        .get(models_url)
                        .header("Authorization", format!("Bearer {}", self.api_key))
                        .send()
                        .await
                        .map(|r| r.status().is_success())
                        .unwrap_or(false)
                } else {
                    let res = self
                        .http
                        .post(&self.base_url)
                        .header("Authorization", format!("Bearer {}", self.api_key))
                        .header("Content-Type", "application/json")
                        .json(&json!({
                            "model": self.model,
                            "messages": [{ "role": "user", "content": "hi" }],
                            "max_tokens": 1
                        }))
                        .send()
                        .await;
                    match res {
                        Ok(r) => {
                            let s = r.status().as_u16();
                            s != 401 && s != 403
                        }
                        Err(_) => false,
                    }
                }
            }
        };
        tracing::debug!("Health check for {}: {result}", self.provider_id);
        result
    }

    fn name(&self) -> &str {
        &self.provider_id
    }

    fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        cancel: CancellationToken,
    ) -> EventStream {
        self.wire_client().stream(messages, tools, cancel)
    }
}

/// Derive the `/models` listing URL from an OpenAI-compatible chat endpoint.
/// e.g. ".../v1/chat/completions" -> ".../v1/models". Returns None if the URL
/// doesn't contain a recognizable "/chat/completions" segment.
fn openai_models_url(chat_url: &str) -> Option<String> {
    chat_url
        .rsplit_once("/chat/completions")
        .map(|(base, _)| format!("{base}/models"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_parses_known_and_falls_back() {
        assert_eq!(WireFormat::parse("anthropic"), WireFormat::Anthropic);
        assert_eq!(WireFormat::parse("Anthropic"), WireFormat::Anthropic);
        assert_eq!(WireFormat::parse("openai"), WireFormat::OpenAi);
        assert_eq!(WireFormat::parse("gemini"), WireFormat::OpenAi);
        assert_eq!(WireFormat::parse("groq"), WireFormat::OpenAi);
        assert_eq!(WireFormat::parse(""), WireFormat::OpenAi);
        assert_eq!(WireFormat::parse("something-new"), WireFormat::OpenAi);
    }

    #[test]
    fn models_url_derived_from_chat_endpoint() {
        assert_eq!(
            openai_models_url("https://api.openai.com/v1/chat/completions").as_deref(),
            Some("https://api.openai.com/v1/models")
        );
        assert_eq!(
            openai_models_url("https://openrouter.ai/api/v1/chat/completions").as_deref(),
            Some("https://openrouter.ai/api/v1/models")
        );
        assert_eq!(
            openai_models_url("https://api.groq.com/openai/v1/chat/completions").as_deref(),
            Some("https://api.groq.com/openai/v1/models")
        );
    }

    #[test]
    fn models_url_none_for_unrecognized_endpoint() {
        assert_eq!(openai_models_url("https://example.com/v1/responses"), None);
    }

    #[test]
    fn name_reflects_provider_id() {
        let c = BYOClient::new(
            "grok",
            WireFormat::OpenAi,
            "https://api.x.ai/v1/chat/completions",
            "grok-2".into(),
            "sk-test".into(),
            300,
        );
        assert_eq!(c.name(), "grok");
    }
}
