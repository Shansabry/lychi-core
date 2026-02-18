use std::collections::HashMap;

use crate::ai::agent::{AgentPlan, AiResponse};
use crate::ai::router::AiRouter;
use crate::command::{CommandHandler, CommandResult, CompletionItem};
use crate::error::LychiError;
use crate::intent;

pub struct CommandRegistry {
    handlers: HashMap<String, Box<dyn CommandHandler>>,
    ai_router: Option<AiRouter>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            ai_router: None,
        }
    }

    pub fn register(&mut self, handler: Box<dyn CommandHandler>) {
        self.handlers.insert(handler.prefix().to_string(), handler);
    }

    /// Set the AI router for natural language intent routing.
    pub fn set_ai_router(&mut self, router: AiRouter) {
        self.ai_router = Some(router);
    }

    /// Whether AI routing is available.
    pub fn has_ai(&self) -> bool {
        self.ai_router.is_some()
    }

    /// Dispatch directly to a handler by prefix.
    pub async fn execute_handler(
        &self,
        handler: &str,
        args: &str,
    ) -> Result<CommandResult, LychiError> {
        if let Some(h) = self.handlers.get(handler) {
            return h.execute(args).await;
        }
        Err(LychiError::UnknownCommand(handler.to_string()))
    }

    /// Route raw input through the intent router and dispatch.
    ///
    /// Decision flow:
    /// 1. Explicit prefix/trigger → dispatch immediately (no AI)
    /// 2. Pattern-detected (file/URL/math) → dispatch normally (no AI)
    /// 3. Default "open" fallback + AI enabled → ask AI before app search
    /// 4. AI disabled or failed → Phase 1 behavior (try app, fallback web)
    pub async fn execute_routed(&self, raw: &str) -> Result<CommandResult, LychiError> {
        let route = intent::route(raw);

        // Explicit routes and pattern-detected routes: dispatch directly
        if route.explicit || route.handler != "open" {
            return self.execute_handler(route.handler, &route.args).await;
        }

        // Default "open" fallback — try AI first if available
        if let Some(ai) = &self.ai_router {
            let known: Vec<&str> = self.handlers.keys().map(|s| s.as_str()).collect();
            if let Ok(Some(ai_route)) = ai.try_route(raw, &known).await
                && let Some(handler) = self.handlers.get(&ai_route.command) {
                    let mut result = handler.execute(&ai_route.args).await?;
                    result.routed_by = Some("ai".to_string());
                    return Ok(result);
                }
            // AI failed or returned unknown command — fall through to heuristic
        }

        // Heuristic fallback: try app search, then web
        match self.execute_handler("open", &route.args).await {
            Ok(result) if result.success => Ok(result),
            _ => {
                if let Some(web) = self.handlers.get("web") {
                    return web.execute(raw).await;
                }
                Err(LychiError::UnknownCommand(raw.to_string()))
            }
        }
    }

    /// Get completions using the intent router to pick the right handler.
    pub async fn completions_routed(&self, raw: &str) -> Vec<CompletionItem> {
        let route = intent::route(raw);

        if let Some(handler) = self.handlers.get(route.handler) {
            let results = handler.completions(&route.args).await;
            if !results.is_empty() {
                return results;
            }
        }

        // Fallback: if the routed handler returned nothing and it's not explicit,
        // try app search
        if !route.explicit && route.handler != "open"
            && let Some(open) = self.handlers.get("open") {
                return open.completions(raw).await;
            }

        Vec::new()
    }

    pub fn prefixes(&self) -> Vec<&str> {
        self.handlers.keys().map(|s| s.as_str()).collect()
    }

    pub fn has(&self, prefix: &str) -> bool {
        self.handlers.contains_key(prefix)
    }

    /// Ask the AI for a plan. Returns `Some(plan)` if the AI suggests
    /// a multi-step plan, `None` if the input resolves to a single-shot
    /// route or if AI is unavailable.
    pub async fn try_plan(&self, raw: &str) -> Option<AgentPlan> {
        let route = intent::route(raw);

        // Explicit routes and pattern-detected routes never produce plans
        if route.explicit || route.handler != "open" {
            return None;
        }

        let ai = self.ai_router.as_ref()?;
        let known: Vec<&str> = self.handlers.keys().map(|s| s.as_str()).collect();

        match ai.try_route_or_plan(raw, &known).await {
            Ok(Some(AiResponse::Plan(plan))) => Some(plan),
            Ok(Some(AiResponse::SingleRoute(_))) => None,
            _ => None,
        }
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandHandler;
    use async_trait::async_trait;

    struct DummyHandler;

    #[async_trait]
    impl CommandHandler for DummyHandler {
        fn prefix(&self) -> &str {
            "test"
        }
        fn description(&self) -> &str {
            "A test command"
        }
        async fn execute(&self, args: &str) -> Result<CommandResult, LychiError> {
            Ok(CommandResult {
                success: true,
                output: Some(format!("executed with: {args}")),
                error: None,
                duration_ms: 0,
                routed_by: None,
                open_url: None,
            })
        }
    }

    #[tokio::test]
    async fn execute_handler_direct() {
        let mut registry = CommandRegistry::new();
        registry.register(Box::new(DummyHandler));

        let result = registry.execute_handler("test", "hello").await.unwrap();
        assert!(result.success);
        assert_eq!(result.output.unwrap(), "executed with: hello");
    }

    #[tokio::test]
    async fn execute_handler_unknown() {
        let registry = CommandRegistry::new();
        let err = registry.execute_handler("nope", "").await.unwrap_err();
        assert!(matches!(err, LychiError::UnknownCommand(_)));
    }
}
