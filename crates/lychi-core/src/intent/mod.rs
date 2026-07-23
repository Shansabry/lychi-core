pub mod ai_router;
pub mod classify;
pub mod patterns;
pub mod prompt;
pub mod typo_suggest;

use crate::action_registry::registry::ActionRegistry;
use crate::providers::{AgentPlan, AiResponse};
use ai_router::AiRouter;
use patterns::{Confidence, PatternResult};

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

    /// Remove the AI router (switch AI off at runtime).
    pub fn clear_ai_router(&mut self) {
        self.ai_router = None;
    }

    /// Resolve raw input into a structured intent.
    ///
    /// Four-phase pipeline:
    /// 1. Explicit/Strong match → dispatch immediately
    /// 2. Weak match → try AI first, use weak match as fallback
    /// 3. No match + AI available → ask AI
    /// 4. No match + AI unavailable → AppIndex → web search
    pub async fn resolve(&self, raw: &str, registry: &ActionRegistry) -> ResolvedIntent {
        // Phase 1: Deterministic match
        let (no_match_input, weak_fallback) = match patterns::route(raw, registry) {
            PatternResult::Match(route) if route.confidence != Confidence::Weak => {
                // Explicit or Strong — dispatch immediately, no AI needed
                tracing::debug!(
                    phase = "pattern",
                    action = %route.handler,
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
            PatternResult::Match(route) => {
                // Weak match — try AI first, keep this as fallback
                tracing::debug!(
                    phase = "pattern",
                    action = %route.handler,
                    confidence = ?route.confidence,
                    "[resolve] phase=pattern action={} confidence=Weak (deferring to AI)",
                    route.handler,
                );
                let fallback = ResolvedIntent {
                    action_id: route.handler.to_string(),
                    args: route.args.clone(),
                    routing: RoutingMethod::Pattern,
                };
                (raw.trim().to_string(), Some(fallback))
            }
            PatternResult::NoMatch { input } => (input, None),
        };

        // Phase 1b: A CONFIDENT local app match short-circuits AI. When the app
        // index resolves the input to an app at auto-launch confidence (≥0.90,
        // e.g. "spotify" or "can you open spotify" via token-set matching), we
        // KNOW the intent — launching it is instant and certain, so we never pay
        // a network round-trip to have AI re-derive it. AI still handles genuinely
        // fuzzy input below this bar (see Phase 2). Certainty beats a guess.
        {
            let app_match = crate::desktop_apps::app_index().best_match(&no_match_input);
            if let Some((id, score)) = app_match
                && score >= crate::desktop_apps::AUTO_LAUNCH_THRESHOLD
            {
                let index = crate::desktop_apps::app_index();
                let entry = index.entry(id);
                tracing::debug!(
                    phase = "app-confident",
                    action = "open",
                    app_score = score,
                    desktop = %entry.desktop_path,
                    "[resolve] phase=app-confident action=open score={score:.2} (pre-AI short-circuit)"
                );
                return ResolvedIntent {
                    action_id: "open".to_string(),
                    args: entry.desktop_path.clone(),
                    routing: RoutingMethod::Pattern,
                };
            }
        }

        // NOTE: The old AI intent-routing phase (route_intent/route_or_plan →
        // an `ask` handler) has been REMOVED. Natural language is now owned
        // entirely by the streaming tool-calling agent (coordinator/), invoked
        // from the launcher input box — NOT by the executor. The executor's
        // resolver is purely deterministic: pattern match → weak fallback →
        // web search. There is ONE AI path, and it is the agent. Anything that
        // reaches here without a deterministic match falls through to web
        // search (a safe, instant default), never a hidden second AI call.

        // Phase 2b: use a weak pattern match if we have one.
        if let Some(fallback) = weak_fallback {
            tracing::debug!(
                phase = "weak-fallback",
                action = %fallback.action_id,
                "[resolve] phase=weak-fallback action={} (AI unavailable/inconclusive)",
                fallback.action_id
            );
            return fallback;
        }

        // Phase 3: Web fallback. A confident app match (≥ AUTO_LAUNCH_THRESHOLD)
        // was already handled in Phase 1b before AI, so anything reaching here is
        // either a below-threshold app hint or no app at all — both go to web.
        let app_match = crate::desktop_apps::app_index().best_match(&no_match_input);
        match app_match {
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
        if let PatternResult::Match(_) = patterns::route(raw, registry) {
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
