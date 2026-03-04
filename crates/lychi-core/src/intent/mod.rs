pub mod ai_router;
pub mod patterns;
pub mod prompt;
pub mod typo_suggest;

use crate::action_registry::registry::ActionRegistry;
use crate::providers::{AgentPlan, AiResponse};
use ai_router::AiRouter;
use patterns::PatternResult;

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
    /// Three-phase pipeline:
    /// 1. Deterministic match (explicit prefix, URL, file, math, etc.) → dispatch immediately
    /// 2. No match + AI available → ask AI
    /// 3. No match + AI unavailable → try "open" (executor falls back to web if app not found)
    pub async fn resolve(&self, raw: &str, registry: &ActionRegistry) -> ResolvedIntent {
        // Phase 1: Deterministic match — patterns.rs is confident, dispatch immediately
        let no_match_input = match patterns::route(raw) {
            PatternResult::Match(route) => {
                tracing::debug!(
                    phase = "pattern",
                    action = route.handler,
                    explicit = route.explicit,
                    confidence = ?route.confidence,
                    "[resolve] phase=pattern action={} explicit={} confidence={:?}",
                    route.handler,
                    route.explicit,
                    route.confidence
                );
                return ResolvedIntent {
                    action_id: route.handler.to_string(),
                    args: route.args,
                    routing: if route.explicit {
                        RoutingMethod::Explicit
                    } else {
                        RoutingMethod::Pattern
                    },
                };
            }
            PatternResult::NoMatch { input } => input,
        };

        // Phase 2: No deterministic match — try AI
        if let Some(ai) = &self.ai_router {
            // Exclude "open" from known IDs — it's the no-match fallback, not a real intent.
            let known: Vec<&str> = registry
                .list_ids()
                .into_iter()
                .filter(|id| *id != "open")
                .collect();
            if let Ok(Some(ai_route)) = ai.try_route(raw, &known).await
                && registry.has(&ai_route.action_id)
                && ai_route.action_id != "open"
            {
                tracing::debug!(
                    phase = "ai",
                    action = %ai_route.action_id,
                    "[resolve] phase=ai action={}",
                    ai_route.action_id
                );
                return ResolvedIntent {
                    action_id: ai_route.action_id,
                    args: ai_route.args,
                    routing: RoutingMethod::Ai,
                };
            }
        }

        // Phase 3: No match, AI unavailable or inconclusive.
        // Ask AppIndex for a confident app match (score ≥ AUTO_LAUNCH_THRESHOLD).
        // If found → route to "open" with the stable desktop_path as args (fast-path launch).
        // Otherwise → route directly to "web" (skip the open→web detour).
        let app_match = crate::desktop_apps::app_index().best_match(&no_match_input);
        match app_match {
            Some((id, score)) if score >= crate::desktop_apps::AUTO_LAUNCH_THRESHOLD => {
                let index = crate::desktop_apps::app_index();
                let entry = index.entry(id);
                tracing::debug!(
                    phase = "fallback",
                    action = "open",
                    app_score = score,
                    desktop = %entry.desktop_path,
                    "[resolve] phase=fallback action=open score={:.2} desktop={}",
                    score,
                    entry.desktop_path
                );
                ResolvedIntent {
                    action_id: "open".to_string(),
                    args: entry.desktop_path.clone(),
                    routing: RoutingMethod::Pattern,
                }
            }
            Some((_, score)) => {
                tracing::debug!(
                    phase = "fallback",
                    action = "web",
                    app_score = score,
                    "[resolve] phase=fallback action=web (best app score={:.2} below threshold)",
                    score
                );
                ResolvedIntent {
                    action_id: "web".to_string(),
                    args: no_match_input,
                    routing: RoutingMethod::Pattern,
                }
            }
            None => {
                tracing::debug!(
                    phase = "fallback",
                    action = "web",
                    "[resolve] phase=fallback action=web (no app candidates)"
                );
                ResolvedIntent {
                    action_id: "web".to_string(),
                    args: no_match_input,
                    routing: RoutingMethod::Pattern,
                }
            }
        }
    }

    /// Ask AI for a multi-step plan. Returns `None` if AI is unavailable
    /// or the input resolves to a single-shot route.
    pub async fn try_plan(&self, raw: &str, registry: &ActionRegistry) -> Option<AgentPlan> {
        // Only unmatched inputs can produce plans — deterministic matches are final
        if let PatternResult::Match(_) = patterns::route(raw) {
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
