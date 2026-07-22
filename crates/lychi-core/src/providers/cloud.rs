use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

use crate::error::LychiError;
use crate::intent::prompt;

use super::{
    AiProvider, AiResponse, AiRoute, CancellationToken, ChatMessage, EventStream, ToolDef,
};

/// A source of Firebase ID tokens — implemented by the src-tauri layer
/// using the OS keyring. This trait lives in core so CloudClient can be
/// tested without Tauri dependencies.
#[async_trait]
pub trait TokenProvider: Send + Sync {
    /// Returns a valid, non-expired Firebase ID token.
    /// Refreshes the token automatically if close to expiry.
    async fn get_token(&self) -> Result<String, LychiError>;
}

/// Credit balance response from the cloud credits endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct CreditBalance {
    pub balance: i64,
    pub used_this_month: i64,
    pub plan: String,
    pub bonus_pool: i64,
    pub resets_at: String,
}

/// Lychi Cloud AI provider — proxies requests through the cloud backend
/// (api.lychi.app) which handles auth, credit deduction, and AI inference.
pub struct CloudClient {
    base_url: String,
    token_provider: Arc<dyn TokenProvider>,
    http: Client,
}

impl CloudClient {
    pub fn new(base_url: String, token_provider: Arc<dyn TokenProvider>) -> Self {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token_provider,
            http,
        }
    }

    /// POST to a cloud endpoint with Bearer auth. Returns parsed JSON.
    async fn authed_post(&self, path: &str, body: Value) -> Result<Value, LychiError> {
        let token = self.token_provider.get_token().await?;
        let url = format!("{}{}", self.base_url, path);

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| LychiError::Ai(format!("Cloud request failed: {e}")))?;

        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();

        if status.as_u16() == 402 {
            return Err(LychiError::Ai(
                "No cloud credits remaining — upgrade your plan at lychi.app".to_string(),
            ));
        }
        if status.as_u16() == 401 {
            return Err(LychiError::Ai(
                "Cloud auth expired — please sign in again".to_string(),
            ));
        }
        if !status.is_success() {
            return Err(LychiError::Ai(format!("Cloud error {status}: {body_text}")));
        }

        serde_json::from_str::<Value>(&body_text)
            .map_err(|e| LychiError::Ai(format!("Failed to parse cloud response: {e}")))
    }

    /// Fetch the user's credit balance from the cloud.
    pub async fn get_credits(&self) -> Result<CreditBalance, LychiError> {
        let token = self.token_provider.get_token().await?;
        let url = format!("{}/v1/credits", self.base_url);

        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .map_err(|e| LychiError::Ai(format!("Credits request failed: {e}")))?;

        if !resp.status().is_success() {
            return Err(LychiError::Ai(format!(
                "Credits fetch failed: {}",
                resp.status()
            )));
        }

        resp.json::<CreditBalance>()
            .await
            .map_err(|e| LychiError::Ai(format!("Failed to parse credits: {e}")))
    }
}

#[async_trait]
impl AiProvider for CloudClient {
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
        let body = json!({
            "input": input,
            "known_actions": known_actions,
            "context_hint": context_hint.unwrap_or(""),
        });

        let json = self.authed_post("/v1/route", body).await?;

        let content = json["content"]
            .as_str()
            .ok_or_else(|| LychiError::Ai("No content in cloud response".to_string()))?;

        tracing::debug!(
            prompt_version = prompt::PROMPT_VERSION,
            provider = "cloud",
            "[ai] raw response: {content}"
        );
        prompt::parse_ai_response(content, known_actions, input)
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}/health", self.base_url);
        let result = self
            .http
            .get(&url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        tracing::debug!("Health check for cloud ({}): {result}", self.base_url);
        result
    }

    fn name(&self) -> &str {
        "cloud"
    }

    async fn answer_question(
        &self,
        system_prompt: &str,
        question: &str,
    ) -> Result<String, LychiError> {
        let body = json!({
            "system_prompt": system_prompt,
            "question": question,
        });

        let json = self.authed_post("/v1/ask", body).await?;

        json["answer"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| LychiError::Ai("No answer in cloud response".to_string()))
    }

    /// Cloud streaming chat is not available until the lychi-cloud proxy exposes
    /// an SSE `/v1/chat` endpoint (separate closed-source repo). Per the
    /// no-fallback rule we do NOT add a buffered client-side shim — cloud mode
    /// stays disabled for launch (see the AI-rewrite plan). Returns a stream that
    /// yields a single terminal error.
    fn chat(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolDef],
        _cancel: CancellationToken,
    ) -> EventStream {
        use futures_util::{StreamExt as _, stream};
        stream::once(async {
            Err(LychiError::Ai(
                "Cloud AI streaming is not available yet — use a local model or your own API key."
                    .to_string(),
            ))
        })
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticTokenProvider(String);

    #[async_trait]
    impl TokenProvider for StaticTokenProvider {
        async fn get_token(&self) -> Result<String, LychiError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn test_base_url_trailing_slash_stripped() {
        let provider = Arc::new(StaticTokenProvider("test".to_string()));
        let client = CloudClient::new("https://api.lychi.app/".to_string(), provider);
        assert_eq!(client.base_url, "https://api.lychi.app");
    }

    #[test]
    fn test_credit_balance_deserialize() {
        let json = r#"{"balance":2500,"used_this_month":0,"plan":"pro","bonus_pool":0,"resets_at":"2026-04-01T00:00:00Z"}"#;
        let credit: CreditBalance = serde_json::from_str(json).unwrap();
        assert_eq!(credit.balance, 2500);
        assert_eq!(credit.plan, "pro");
    }
}
