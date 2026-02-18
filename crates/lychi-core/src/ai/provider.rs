use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::LychiError;

use super::agent::AiResponse;

/// The result of AI intent routing — a structured command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiRoute {
    pub command: String,
    pub args: String,
}

/// Trait for AI providers (BYO, Ollama, Cloud).
///
/// Each provider implements the same interface so the router
/// can swap between them based on config.
#[async_trait]
pub trait AiProvider: Send + Sync {
    /// Parse natural language input into a structured command route (single-shot only).
    async fn route_intent(
        &self,
        input: &str,
        known_commands: &[&str],
    ) -> Result<AiRoute, LychiError>;

    /// Route input, returning either a single route or a multi-step plan.
    async fn route_or_plan(
        &self,
        input: &str,
        known_commands: &[&str],
    ) -> Result<AiResponse, LychiError>;

    /// Check if the provider is reachable and functional.
    async fn health_check(&self) -> bool;

    /// Human-readable provider name (e.g. "anthropic", "openai", "ollama").
    fn name(&self) -> &str;
}
