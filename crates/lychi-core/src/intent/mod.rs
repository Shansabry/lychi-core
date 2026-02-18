pub mod ai_router;
pub mod patterns;
pub mod prompt;

use crate::action_registry::registry::ActionRegistry;
use crate::providers::{AgentPlan, AiResponse};
use ai_router::AiRouter;

/// How the intent was resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingMethod {
    Explicit,
    Pattern,
    Ai,
}

/// A resolved intent — the result of converting raw input into a structured action.
#[derive(Debug, Clone)]
pub struct ResolvedIntent {
    pub action_id: String,
    pub args: String,
    pub routing: RoutingMethod,
}

/// Check if input looks like a natural language question.
/// Used as a no-AI fallback — routes questions to web search instead of app launcher.
fn looks_like_question(input: &str) -> bool {
    let lower = input.trim_start().to_lowercase();
    const QUESTION_WORDS: &[&str] = &[
        "what ", "who ", "how ", "why ", "when ", "where ", "which ", "is ", "are ", "can ",
        "does ", "do ", "will ", "should ", "explain ", "define ",
    ];
    QUESTION_WORDS.iter().any(|w| lower.starts_with(w)) || lower.ends_with('?')
}

/// Intent Resolver — converts raw user input into structured intents.
///
/// Combines deterministic pattern matching with optional AI routing.
pub struct IntentResolver {
    ai_router: Option<AiRouter>,
}

impl IntentResolver {
    pub fn new(ai_router: Option<AiRouter>) -> Self {
        Self { ai_router }
    }

    /// Whether AI routing is available.
    pub fn has_ai(&self) -> bool {
        self.ai_router.is_some()
    }

    /// Get the AI router reference (for health checks etc).
    pub fn ai_router(&self) -> Option<&AiRouter> {
        self.ai_router.as_ref()
    }

    /// Set or replace the AI router.
    pub fn set_ai_router(&mut self, router: AiRouter) {
        self.ai_router = Some(router);
    }

    /// Resolve raw input into a structured intent.
    ///
    /// Priority:
    /// 1. Explicit prefix / trigger character → dispatch immediately
    /// 2. Pattern detection (file path, URL, math) → dispatch
    /// 3. Default "open" + AI available → ask AI
    /// 4. Question fallback (no AI) → web search
    /// 5. Fallback: "open" handler
    pub async fn resolve(&self, raw: &str, registry: &ActionRegistry) -> ResolvedIntent {
        let route = patterns::route(raw);

        // Explicit or pattern-detected: no AI needed
        if route.explicit {
            return ResolvedIntent {
                action_id: route.handler.to_string(),
                args: route.args,
                routing: RoutingMethod::Explicit,
            };
        }

        if route.handler != "open" {
            return ResolvedIntent {
                action_id: route.handler.to_string(),
                args: route.args,
                routing: RoutingMethod::Pattern,
            };
        }

        // Default "open" fallback — try AI first if available
        if let Some(ai) = &self.ai_router {
            let known: Vec<&str> = registry.list_ids();
            if let Ok(Some(ai_route)) = ai.try_route(raw, &known).await
                && registry.has(&ai_route.action_id)
            {
                return ResolvedIntent {
                    action_id: ai_route.action_id,
                    args: ai_route.args,
                    routing: RoutingMethod::Ai,
                };
            }
        }

        // AI unavailable or failed — if it looks like a question, fall back to
        // web search instead of trying to open it as an app name.
        if looks_like_question(raw) {
            return ResolvedIntent {
                action_id: "web".to_string(),
                args: route.args,
                routing: RoutingMethod::Pattern,
            };
        }

        // Fallback to "open"
        ResolvedIntent {
            action_id: route.handler.to_string(),
            args: route.args,
            routing: RoutingMethod::Pattern,
        }
    }

    /// Ask AI for a multi-step plan. Returns `None` if AI is unavailable
    /// or the input resolves to a single-shot route.
    pub async fn try_plan(&self, raw: &str, registry: &ActionRegistry) -> Option<AgentPlan> {
        let route = patterns::route(raw);

        // Explicit routes never produce plans
        if route.explicit || route.handler != "open" {
            return None;
        }

        let ai = self.ai_router.as_ref()?;
        let known: Vec<&str> = registry.list_ids();

        match ai.try_route_or_plan(raw, &known).await {
            Ok(Some(AiResponse::Plan(plan))) => Some(plan),
            _ => None,
        }
    }
}
