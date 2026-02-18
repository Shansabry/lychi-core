use crate::action_registry::registry::ActionRegistry;
use crate::action_registry::{ActionResult, CompletionItem, RiskLevel};
use crate::error::LychiError;
use crate::intent::{IntentResolver, RoutingMethod};
use crate::providers::AgentPlan;
use crate::rules::{RulesEngine, ValidationDecision, ValidationRequest};

/// Executor — the single orchestrator that wires all bricks together.
///
/// Pipeline: input → IntentResolver.resolve() → RulesEngine.validate() → ActionHandler.execute()
pub struct Executor {
    pub registry: ActionRegistry,
    pub rules: RulesEngine,
    pub resolver: IntentResolver,
}

impl Executor {
    pub fn new(registry: ActionRegistry, rules: RulesEngine, resolver: IntentResolver) -> Self {
        Self {
            registry,
            rules,
            resolver,
        }
    }

    /// Run the full pipeline: resolve → validate → execute.
    ///
    /// If `confirmed` is true, `Confirm` decisions are treated as `Execute`.
    /// `Deny` decisions are always enforced regardless of `confirmed`.
    pub async fn run(&self, input: &str, confirmed: bool) -> Result<ActionResult, LychiError> {
        let intent = self.resolver.resolve(input, &self.registry).await;
        tracing::info!(
            "Resolved '{}' → action={}, args='{}', routing={:?}",
            input, intent.action_id, intent.args, intent.routing
        );

        let handler = self
            .registry
            .get(&intent.action_id)
            .ok_or_else(|| LychiError::UnknownCommand(intent.action_id.clone()))?;

        let routed_by = match &intent.routing {
            RoutingMethod::Explicit => "explicit",
            RoutingMethod::Pattern => "pattern",
            RoutingMethod::Ai => "ai",
        };

        // Validate through rules engine
        let decision = self.rules.validate(&ValidationRequest {
            action_id: &intent.action_id,
            args: &intent.args,
            routed_by,
            default_risk: handler.default_risk(),
        });

        match decision {
            ValidationDecision::Deny { reason } => Ok(ActionResult {
                success: false,
                output: None,
                error: Some(format!("Blocked: {reason}")),
                duration_ms: 0,
                routed_by: Some(routed_by.to_string()),
                open_url: None,
                needs_confirmation: None,
                risk_level: Some(RiskLevel::High),
                output_type: None,
                executed_args: None,
            }),
            ValidationDecision::Confirm { reason } if !confirmed => Ok(ActionResult {
                success: false,
                output: None,
                error: None,
                duration_ms: 0,
                routed_by: Some(routed_by.to_string()),
                open_url: None,
                needs_confirmation: Some(reason),
                risk_level: Some(handler.default_risk()),
                output_type: None,
                executed_args: None,
            }),
            // Execute (or Confirm with confirmed=true)
            _ => {
                let mut result = handler.execute(&intent.args).await?;
                if intent.routing == RoutingMethod::Ai {
                    result.routed_by = Some("ai".to_string());
                }
                // Pass actual executed args to frontend (useful for ls output linkification etc.)
                if intent.action_id == "run" {
                    result.executed_args = Some(intent.args.clone());
                }

                // If the "open" handler failed and we have a web fallback, try it
                if intent.action_id == "open"
                    && !result.success
                    && intent.routing != RoutingMethod::Ai
                    && let Some(web) = self.registry.get("web")
                {
                    return web.execute(input).await;
                }

                Ok(result)
            }
        }
    }

    /// Get completions using the intent resolver to pick the right handler.
    pub async fn completions(&self, raw: &str) -> Vec<CompletionItem> {
        let route = crate::intent::patterns::route(raw);

        let results = self.registry.completions(route.handler, &route.args).await;
        if !results.is_empty() {
            return results;
        }

        // Fallback: if the routed handler returned nothing and it's not explicit,
        // try app search
        if !route.explicit && route.handler != "open" {
            return self.registry.completions("open", raw).await;
        }

        Vec::new()
    }

    /// Ask AI for a plan.
    pub async fn try_plan(&self, raw: &str) -> Option<AgentPlan> {
        self.resolver.try_plan(raw, &self.registry).await
    }

    /// Whether AI is available.
    pub fn has_ai(&self) -> bool {
        self.resolver.has_ai()
    }
}
