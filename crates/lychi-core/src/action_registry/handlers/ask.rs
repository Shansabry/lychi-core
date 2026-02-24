use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::action_registry::{ActionHandler, ActionResult, OutputType};
use crate::error::LychiError;
use crate::providers::AiProvider;

const ASK_SYSTEM_PROMPT: &str = r#"You are a concise knowledge assistant in a desktop launcher. Answer the user's question in 2-3 sentences maximum. Be direct and factual. Do not use markdown formatting. If you're unsure, say so briefly."#;

const ASK_TIMEOUT: Duration = Duration::from_secs(10);

pub struct AskHandler {
    ai_provider: Option<Arc<dyn AiProvider>>,
    search_url: String,
}

impl AskHandler {
    pub fn new(ai_provider: Option<Arc<dyn AiProvider>>, search_url: String) -> Self {
        Self {
            ai_provider,
            search_url,
        }
    }
}

#[async_trait]
impl ActionHandler for AskHandler {
    fn id(&self) -> &str {
        "ask"
    }

    fn description(&self) -> &str {
        "Ask a question and get an AI-powered answer inline"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let query = args.trim();
        if query.is_empty() {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some("Usage: ask <question>".to_string()),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: Some(OutputType::Text),
                executed_args: None,
            });
        }

        let search_url = format!("{}{}", self.search_url, urlencoding::encode(query));

        // If no AI provider, fall back to web search
        let Some(provider) = &self.ai_provider else {
            return Ok(ActionResult {
                success: true,
                output: Some(format!("AI not configured — searching: {query}")),
                error: None,
                duration_ms: 0,
                routed_by: None,
                open_url: Some(search_url),
                needs_confirmation: None,
                risk_level: None,
                output_type: Some(OutputType::Text),
                executed_args: None,
            });
        };

        let start = Instant::now();

        let answer = tokio::time::timeout(
            ASK_TIMEOUT,
            provider.answer_question(ASK_SYSTEM_PROMPT, query),
        )
        .await;
        let duration = start.elapsed().as_millis() as u64;

        match answer {
            Ok(Ok(text)) => Ok(ActionResult {
                success: true,
                output: Some(text),
                error: None,
                duration_ms: duration,
                routed_by: Some("ai".to_string()),
                open_url: Some(search_url),
                needs_confirmation: None,
                risk_level: None,
                output_type: Some(OutputType::Text),
                executed_args: None,
            }),
            Ok(Err(e)) => {
                tracing::warn!("Ask AI failed: {e}, falling back to web search");
                Ok(ActionResult {
                    success: true,
                    output: Some(format!("AI unavailable — searching: {query}")),
                    error: None,
                    duration_ms: duration,
                    routed_by: None,
                    open_url: Some(search_url),
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: Some(OutputType::Text),
                    executed_args: None,
                })
            }
            Err(_) => {
                tracing::warn!(
                    "Ask AI timed out after {ASK_TIMEOUT:?}, falling back to web search"
                );
                Ok(ActionResult {
                    success: true,
                    output: Some(format!("AI timed out — searching: {query}")),
                    error: None,
                    duration_ms: duration,
                    routed_by: None,
                    open_url: Some(search_url),
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: Some(OutputType::Text),
                    executed_args: None,
                })
            }
        }
    }
}
