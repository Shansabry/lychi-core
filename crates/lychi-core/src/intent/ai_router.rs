use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::LychiError;
use crate::providers::{AiProvider, AiResponse, AiRoute};

/// Wraps an active AI provider with timeout and error handling.
pub struct AiRouter {
    provider: Arc<dyn AiProvider>,
    timeout: Duration,
    /// Optional context hint for AI routing (set by Executor before resolve).
    context_hint: Mutex<Option<String>>,
}

impl AiRouter {
    pub fn new(provider: Box<dyn AiProvider>) -> Self {
        Self {
            provider: Arc::from(provider),
            timeout: Duration::from_secs(8),
            context_hint: Mutex::new(None),
        }
    }

    pub fn new_shared(provider: Arc<dyn AiProvider>) -> Self {
        Self {
            provider,
            timeout: Duration::from_secs(8),
            context_hint: Mutex::new(None),
        }
    }

    /// Set the context hint for the next AI routing call.
    pub fn set_context_hint(&self, hint: Option<String>) {
        if let Ok(mut guard) = self.context_hint.lock() {
            *guard = hint;
        }
    }

    /// Take the current context hint (consuming it).
    fn take_context_hint(&self) -> Option<String> {
        self.context_hint.lock().ok().and_then(|mut g| g.take())
    }

    /// Route natural language input through the AI provider (single-shot only).
    /// Returns `Ok(Some(route))` on success, `Ok(None)` on timeout/failure
    /// (allowing the caller to fall back to heuristics).
    pub async fn try_route(
        &self,
        input: &str,
        known_actions: &[&str],
    ) -> Result<Option<AiRoute>, LychiError> {
        match self.try_route_or_plan(input, known_actions).await? {
            Some(AiResponse::SingleRoute(route)) => Ok(Some(route)),
            Some(AiResponse::Plan(_)) => {
                tracing::debug!("AI returned plan but single route expected, falling back");
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// Route input, returning either a single route or a multi-step plan.
    /// Returns `Ok(None)` on timeout/failure.
    pub async fn try_route_or_plan(
        &self,
        input: &str,
        known_actions: &[&str],
    ) -> Result<Option<AiResponse>, LychiError> {
        let hint = self.take_context_hint();
        match tokio::time::timeout(
            self.timeout,
            self.provider
                .route_or_plan(input, known_actions, hint.as_deref()),
        )
        .await
        {
            Ok(Ok(response)) => {
                match &response {
                    AiResponse::SingleRoute(route) => {
                        tracing::info!(
                            "AI routed '{}' → {} {}",
                            input,
                            route.action_id,
                            route.args
                        );
                    }
                    AiResponse::Plan(plan) => {
                        tracing::info!("AI planned '{}' → {} steps", input, plan.steps.len());
                    }
                }
                Ok(Some(response))
            }
            Ok(Err(e)) => {
                tracing::warn!("AI routing failed: {e}");
                Ok(None)
            }
            Err(_) => {
                tracing::warn!("AI routing timed out after {:?}", self.timeout);
                Ok(None)
            }
        }
    }

    /// Check if the underlying provider is healthy.
    pub async fn health_check(&self) -> bool {
        self.provider.health_check().await
    }

    /// Provider name for display.
    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }
}
