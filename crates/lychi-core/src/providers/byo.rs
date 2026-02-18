use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};

use std::str::FromStr;

use crate::error::LychiError;

use super::{AiProvider, AiResponse, AiRoute};
use crate::intent::prompt;

/// Supported BYO API providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BYOProvider {
    OpenAI,
    Anthropic,
    Groq,
}

impl FromStr for BYOProvider {
    type Err = LychiError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "openai" => Ok(Self::OpenAI),
            "anthropic" => Ok(Self::Anthropic),
            "groq" => Ok(Self::Groq),
            _ => Err(LychiError::Ai(format!("Unknown BYO provider: {s}"))),
        }
    }
}

impl BYOProvider {
    fn endpoint(&self) -> &'static str {
        match self {
            Self::OpenAI => "https://api.openai.com/v1/chat/completions",
            Self::Anthropic => "https://api.anthropic.com/v1/messages",
            Self::Groq => "https://api.groq.com/openai/v1/chat/completions",
        }
    }
}

/// BYO API key provider — sends requests directly to OpenAI, Anthropic, or Groq.
pub struct BYOClient {
    provider: BYOProvider,
    model: String,
    api_key: String,
    http: Client,
}

impl BYOClient {
    pub fn new(provider: BYOProvider, model: String, api_key: String) -> Self {
        Self {
            provider,
            model,
            api_key,
            http: Client::new(),
        }
    }

    async fn call_openai_compatible(
        &self,
        system_prompt: &str,
        user_input: &str,
    ) -> Result<String, LychiError> {
        let body = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": user_input }
            ],
            "max_tokens": 300,
            "temperature": 0.0
        });

        let resp = self
            .http
            .post(self.provider.endpoint())
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| LychiError::Ai(format!("HTTP request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LychiError::Ai(format!("API returned {status}: {body}")));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| LychiError::Ai(format!("Failed to parse API response: {e}")))?;

        json["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| LychiError::Ai("No content in API response".to_string()))
    }

    async fn call_anthropic(
        &self,
        system_prompt: &str,
        user_input: &str,
    ) -> Result<String, LychiError> {
        let body = json!({
            "model": self.model,
            "system": system_prompt,
            "messages": [
                { "role": "user", "content": user_input }
            ],
            "max_tokens": 300,
            "temperature": 0.0
        });

        let resp = self
            .http
            .post(BYOProvider::Anthropic.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| LychiError::Ai(format!("HTTP request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LychiError::Ai(format!("API returned {status}: {body}")));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| LychiError::Ai(format!("Failed to parse API response: {e}")))?;

        json["content"][0]["text"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| LychiError::Ai("No content in API response".to_string()))
    }
}

#[async_trait]
impl AiProvider for BYOClient {
    async fn route_intent(
        &self,
        input: &str,
        known_actions: &[&str],
    ) -> Result<AiRoute, LychiError> {
        match self.route_or_plan(input, known_actions).await? {
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
    ) -> Result<AiResponse, LychiError> {
        let sys_prompt = prompt::system_prompt(known_actions);

        let response = match self.provider {
            BYOProvider::OpenAI | BYOProvider::Groq => {
                self.call_openai_compatible(&sys_prompt, input).await?
            }
            BYOProvider::Anthropic => self.call_anthropic(&sys_prompt, input).await?,
        };

        tracing::debug!("AI response: {response}");
        prompt::parse_ai_response(&response, known_actions, input)
    }

    async fn health_check(&self) -> bool {
        let result = match self.provider {
            BYOProvider::Anthropic => {
                let res = self
                    .http
                    .post("https://api.anthropic.com/v1/messages")
                    .header("x-api-key", &self.api_key)
                    .header("anthropic-version", "2023-06-01")
                    .header("content-type", "application/json")
                    .body(r#"{"model":"claude-haiku-4-5-20251001","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#)
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
            BYOProvider::OpenAI | BYOProvider::Groq => {
                let url = match self.provider {
                    BYOProvider::OpenAI => "https://api.openai.com/v1/models",
                    BYOProvider::Groq => "https://api.groq.com/openai/v1/models",
                    _ => return false,
                };
                self.http
                    .get(url)
                    .header("Authorization", format!("Bearer {}", self.api_key))
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false)
            }
        };
        tracing::debug!("Health check for {}: {result}", self.name());
        result
    }

    fn name(&self) -> &str {
        match self.provider {
            BYOProvider::OpenAI => "openai",
            BYOProvider::Anthropic => "anthropic",
            BYOProvider::Groq => "groq",
        }
    }

    async fn answer_question(
        &self,
        system_prompt: &str,
        question: &str,
    ) -> Result<String, LychiError> {
        match self.provider {
            BYOProvider::OpenAI | BYOProvider::Groq => {
                self.call_openai_compatible(system_prompt, question).await
            }
            BYOProvider::Anthropic => self.call_anthropic(system_prompt, question).await,
        }
    }
}
