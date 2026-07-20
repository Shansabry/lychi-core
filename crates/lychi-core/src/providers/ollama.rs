use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

use crate::error::LychiError;
use crate::intent::prompt;

use super::{AiProvider, AiResponse, AiRoute};

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

    /// Send a chat completion request to Ollama's OpenAI-compatible endpoint.
    async fn call_chat(&self, system_prompt: &str, user_input: &str) -> Result<String, LychiError> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let body = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": user_input }
            ],
            "max_tokens": self.max_tokens,
            "temperature": 0.0,
            "stream": false
        });

        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| LychiError::Ai(format!("Ollama request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LychiError::Ai(format!("Ollama returned {status}: {body}")));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| LychiError::Ai(format!("Failed to parse Ollama response: {e}")))?;

        json["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| LychiError::Ai("No content in Ollama response".to_string()))
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
    async fn route_intent(
        &self,
        input: &str,
        known_actions: &[&str],
    ) -> Result<AiRoute, LychiError> {
        match self.route_or_plan(input, known_actions, None).await? {
            AiResponse::SingleRoute(route) => Ok(route),
            AiResponse::Plan(_) => Err(LychiError::Ai(
                "AI returned a plan but single route was expected".to_string(),
            )),
        }
    }

    async fn route_or_plan(
        &self,
        input: &str,
        known_actions: &[&str],
        context_hint: Option<&str>,
    ) -> Result<AiResponse, LychiError> {
        let sys_prompt = prompt::system_prompt(known_actions, context_hint);
        let response = self.call_chat(&sys_prompt, input).await?;

        tracing::debug!(
            prompt_version = prompt::PROMPT_VERSION,
            provider = "ollama",
            model = %self.model,
            "[ai] raw response: {response}"
        );
        prompt::parse_ai_response(&response, known_actions, input)
    }

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

    async fn answer_question(
        &self,
        system_prompt: &str,
        question: &str,
    ) -> Result<String, LychiError> {
        self.call_chat(system_prompt, question).await
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
