use std::sync::Arc;

use redb::Database;

use crate::action_registry::registry::ActionRegistry;
use crate::action_registry::{ActionResult, CompletionItem, RiskLevel};
use crate::config::schema::PrivacyConfig;
use crate::context::EnvironmentContext;
use crate::error::LychiError;
use crate::history::HistoryStore;
use crate::intent::{IntentResolver, RoutingMethod};
use crate::providers::AgentPlan;
use crate::rules::{RulesEngine, ValidationDecision, ValidationRequest};

/// Result of executing a command, including the resolved action_id.
pub struct ExecuteResult {
    pub result: ActionResult,
    pub action_id: String,
}

/// Executor — the single orchestrator that wires all bricks together.
///
/// Pipeline: input → IntentResolver.resolve() → RulesEngine.validate() → ActionHandler.execute()
pub struct Executor {
    pub registry: ActionRegistry,
    pub rules: RulesEngine,
    pub resolver: IntentResolver,
    pub history: HistoryStore,
    pub db: Arc<Database>,
    /// Current environment context, refreshed on each summon.
    pub context: Option<EnvironmentContext>,
}

impl Executor {
    pub fn new(
        registry: ActionRegistry,
        rules: RulesEngine,
        resolver: IntentResolver,
        history: HistoryStore,
        db: Arc<Database>,
    ) -> Self {
        Self {
            registry,
            rules,
            resolver,
            history,
            db,
            context: None,
        }
    }

    /// Run the full pipeline: resolve → validate → execute.
    ///
    /// If `confirmed` is true, `Confirm` decisions are treated as `Execute`.
    /// `Deny` decisions are always enforced regardless of `confirmed`.
    pub async fn run(
        &self,
        input: &str,
        confirmed: bool,
        privacy: &PrivacyConfig,
    ) -> Result<ExecuteResult, LychiError> {
        // Set context hint on AI router so it's included in the prompt
        if let Some(ai) = self.resolver.ai_router() {
            let hint = self.context.as_ref().and_then(|ctx| ctx.ai_hint());
            ai.set_context_hint(hint);
        }

        let intent = self.resolver.resolve(input, &self.registry).await;
        tracing::info!(
            "Resolved '{}' → action={}, args='{}', routing={:?}",
            input,
            intent.action_id,
            intent.args,
            intent.routing
        );

        let action_id = intent.action_id.clone();

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
        let decision = self.rules.validate(
            &ValidationRequest {
                action_id: &intent.action_id,
                args: &intent.args,
                routed_by,
                default_risk: handler.default_risk(),
            },
            privacy,
        );

        let result = match decision {
            ValidationDecision::Deny { reason } => ActionResult {
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
                launch_desktop: None,
            },
            ValidationDecision::Confirm { reason } if !confirmed => ActionResult {
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
                launch_desktop: None,
            },
            // Execute (or Confirm with confirmed=true)
            _ => {
                // Set context CWD so shell commands run in the detected workspace.
                // When IDE is focused, prefer IDE workspace (cwd) over background terminal.
                // Otherwise prefer terminal_cwd (from window stack) over cwd.
                let focused_is_ide = self
                    .context
                    .as_ref()
                    .and_then(|ctx| ctx.active_window.as_ref())
                    .is_some_and(|w| w.is_ide);
                crate::action_registry::handlers::shell_exec::set_context_cwd(
                    self.context.as_ref().and_then(|ctx| {
                        if focused_is_ide {
                            ctx.cwd.clone().or_else(|| ctx.terminal_cwd.clone())
                        } else {
                            ctx.terminal_cwd.clone().or_else(|| ctx.cwd.clone())
                        }
                    }),
                );
                // If context detected a terminal emulator, use it for `run` commands
                // (so commands open in the same terminal the user already has).
                if let Some(ref ctx) = self.context
                    && let Some(ref tc) = ctx.terminal_class
                    && which::which(tc).is_ok()
                {
                    crate::action_registry::handlers::shell_exec::set_terminal(Some(tc.clone()));
                }

                // Set context snapshot for `ctx` debug handler
                if intent.action_id == "ctx" {
                    crate::action_registry::handlers::context_debug::set_context(
                        self.context.clone(),
                    );
                }
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
                    return Ok(ExecuteResult {
                        result: web.execute(input).await?,
                        action_id: "web".to_string(),
                    });
                }

                result
            }
        };

        Ok(ExecuteResult { result, action_id })
    }

    /// Get completions using the intent resolver to pick the right handler,
    /// with history entries shown in a separate section below.
    /// When input is empty and context is available, shows contextual suggestions.
    pub async fn completions(&self, raw: &str) -> Vec<CompletionItem> {
        // Contextual suggestions for empty/very short input
        let trimmed = raw.trim();
        if trimmed.len() <= 1
            && let Some(ref ctx) = self.context
        {
            let ctx_items = crate::context::suggestions::suggest(ctx);
            if !ctx_items.is_empty() && trimmed.is_empty() {
                return ctx_items;
            }
        }

        let route = crate::intent::patterns::route(raw);
        let mut handler_results = self.registry.completions(route.handler, &route.args).await;

        // Get history completions (deduplicated against handler results)
        let trimmed = raw.trim();
        let mut history_results = Vec::new();
        if !trimmed.is_empty() {
            for hist in self.history.fuzzy_search(&self.db, trimmed) {
                if !handler_results.iter().any(|r| r.label == hist.label) {
                    history_results.push(hist);
                }
            }
        }

        // Build sectioned output: handler results first, then separator, then history
        if !handler_results.is_empty() || !history_results.is_empty() {
            handler_results.truncate(5);
            history_results.truncate(3);

            // Dirty project guard: warn if git is dirty and user is typing a destructive action
            if let Some(ref ctx) = self.context
                && let Some(ref git) = ctx.git
                && git.dirty
            {
                let lower = trimmed.to_ascii_lowercase();
                // Check both truly destructive actions and suspend (reversible but risks unsaved work)
                const DIRTY_GUARD_ACTIONS: &[&str] =
                    &["shutdown", "reboot", "hibernate", "logout", "suspend"];
                let is_destructive = DIRTY_GUARD_ACTIONS
                    .iter()
                    .any(|&name| name.starts_with(&lower) || lower.starts_with(name));
                if is_destructive {
                    let project_name = ctx
                        .project
                        .as_ref()
                        .and_then(|p| p.root.rsplit('/').next())
                        .unwrap_or("repo");
                    handler_results.insert(
                        0,
                        CompletionItem {
                            label: format!("⚠ {project_name} has uncommitted changes"),
                            icon_path: Some("__warning__".to_string()),
                            score: 200,
                            description: Some(format!(
                                "Branch '{}' is dirty — consider committing first",
                                git.branch
                            )),
                            reason: None,
                        },
                    );
                }
            }

            let mut out = handler_results;

            if !history_results.is_empty() && !out.is_empty() {
                // Insert separator between sections
                out.push(CompletionItem {
                    label: "history".to_string(),
                    icon_path: Some("__separator__".to_string()),
                    score: 0,
                    description: None,
                    reason: None,
                });
            }

            out.extend(history_results);
            return out;
        }

        // Fallback: if the routed handler returned nothing, try app search.
        // Use the args (not raw) so trigger-char prefixes like ">" are stripped.
        // Skip for "web" — multi-word natural language was intentionally routed there,
        // falling through to app search produces nonsense fuzzy matches ("How to make pasta" → "os").
        if route.handler != "open" && route.handler != "web" {
            let search_term = if route.args.is_empty() {
                raw
            } else {
                &route.args
            };
            let app_results = self.registry.completions("open", search_term).await;
            if !app_results.is_empty() {
                return app_results;
            }
        }

        // Typo correction: suggest "Did you mean: X?" for near-miss inputs.
        // Skip for web routes — natural language queries aren't typos.
        if route.handler != "web"
            && let Some(suggestion) = crate::intent::typo_suggest::suggest(raw)
        {
            return vec![suggestion];
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

    /// Refresh environment context (call on summon).
    pub fn refresh_context(&mut self, pre_window: Option<crate::context::WindowContext>) {
        self.context = Some(crate::context::gather(pre_window));
    }
}
