use std::time::Duration;

use crate::error::LychiError;

use super::agent::AiResponse;
use super::provider::{AiProvider, AiRoute};

/// Wraps an active AI provider with timeout and error handling.
pub struct AiRouter {
    provider: Box<dyn AiProvider>,
    timeout: Duration,
}

impl AiRouter {
    pub fn new(provider: Box<dyn AiProvider>) -> Self {
        Self {
            provider,
            timeout: Duration::from_secs(8),
        }
    }

    /// Route natural language input through the AI provider (single-shot only).
    /// Returns `Ok(Some(route))` on success, `Ok(None)` on timeout/failure
    /// (allowing the caller to fall back to heuristics).
    pub async fn try_route(
        &self,
        input: &str,
        known_commands: &[&str],
    ) -> Result<Option<AiRoute>, LychiError> {
        match self.try_route_or_plan(input, known_commands).await? {
            Some(AiResponse::SingleRoute(route)) => Ok(Some(route)),
            Some(AiResponse::Plan(_)) => {
                // Got a plan but caller only wants a route — treat as single-shot failure
                // so the caller falls back to heuristics
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
        known_commands: &[&str],
    ) -> Result<Option<AiResponse>, LychiError> {
        match tokio::time::timeout(
            self.timeout,
            self.provider.route_or_plan(input, known_commands),
        )
        .await
        {
            Ok(Ok(response)) => {
                match &response {
                    AiResponse::SingleRoute(route) => {
                        tracing::info!("AI routed '{}' → {} {}", input, route.command, route.args);
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
