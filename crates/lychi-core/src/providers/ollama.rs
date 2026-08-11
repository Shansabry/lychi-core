use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

use crate::error::LychiError;

use super::wire::{AuthStyle, Dialect, WireClient};
use super::{AiProvider, CancellationToken, ChatMessage, EventStream, ToolDef};

/// Model info returned by Ollama's `/api/tags` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct OllamaModelInfo {
    pub name: String,
    pub size: u64,
    pub modified_at: String,
}

/// Local Ollama AI provider — connects to a running Ollama instance.
///
/// Uses the OpenAI-compatible `/v1/chat/completions` endpoint for inference
/// and `/api/tags` for model discovery and health checks.
pub struct OllamaClient {
    base_url: String,
    model: String,
    max_tokens: u32,
    http: Client,
}

impl OllamaClient {
    pub fn new(base_url: String, model: String, max_tokens: u32) -> Self {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .build()
            .unwrap_or_default();
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            max_tokens,
            http,
        }
    }

    /// List available models from a running Ollama instance.
    /// Static method — doesn't require a fully constructed client.
    pub async fn list_models(base_url: &str) -> Result<Vec<OllamaModelInfo>, LychiError> {
        let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .build()
            .unwrap_or_default();

        let resp = http
            .get(&url)
            .send()
            .await
            .map_err(|e| LychiError::Ai(format!("Cannot connect to Ollama: {e}")))?;

        if !resp.status().is_success() {
            return Err(LychiError::Ai(format!("Ollama returned {}", resp.status())));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| LychiError::Ai(format!("Failed to parse Ollama response: {e}")))?;

        let models = json["models"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        Some(OllamaModelInfo {
                            name: m["name"].as_str()?.to_string(),
                            size: m["size"].as_u64().unwrap_or(0),
                            modified_at: m["modified_at"].as_str().unwrap_or("").to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(models)
    }
}

#[async_trait]
impl AiProvider for OllamaClient {
    async fn health_check(&self) -> bool {
        let url = format!("{}/api/tags", self.base_url);
        let result = self
            .http
            .get(&url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        tracing::debug!("Health check for ollama ({}): {result}", self.base_url);
        result
    }

    fn name(&self) -> &str {
        "ollama"
    }

    /// Streaming chat: Ollama is just the OpenAI dialect against its local
    /// endpoint, with no auth. The whole flow lives in `WireClient`.
    fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        cancel: CancellationToken,
    ) -> EventStream {
        WireClient::new(
            self.http.clone(),
            Dialect::OpenAi,
            format!("{}/v1/chat/completions", self.base_url),
            self.model.clone(),
            self.max_tokens,
            AuthStyle::None,
        )
        .stream(messages, tools, cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_url_trailing_slash_stripped() {
        let client = OllamaClient::new(
            "http://localhost:11434/".to_string(),
            "mistral:latest".to_string(),
            300,
        );
        assert_eq!(client.base_url, "http://localhost:11434");
    }

    #[test]
    fn test_model_info_deserialize() {
        let json =
            r#"{"name":"mistral:latest","size":4109856768,"modified_at":"2024-01-15T10:30:00Z"}"#;
        let info: OllamaModelInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.name, "mistral:latest");
        assert_eq!(info.size, 4109856768);
    }
}
