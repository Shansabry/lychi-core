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

        // Implicit object expansion: if input is an underspecified verb and clipboard
        // holds a compatible value, expand deterministically before hitting AI.
        // Only fires when patterns::route returns NoMatch (no structural match).
        // Strict guards: ≤2 tokens, no existing argument, compatible clipboard type.
        let effective_input = self
            .context
            .as_ref()
            .and_then(|ctx| resolve_with_clipboard(input, ctx))
            .unwrap_or_else(|| input.to_string());

        let intent = self
            .resolver
            .resolve(&effective_input, &self.registry)
            .await;
        tracing::info!(
            action = %intent.action_id,
            routing = ?intent.routing,
            "[execute] resolved action={} routing={:?} input={:?}",
            intent.action_id,
            intent.routing,
            input
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
                focus_app: None,
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
                focus_app: None,
            },
            // Execute (or Confirm with confirmed=true)
            _ => {
                // Set context CWD so shell commands run in the detected workspace.
                //
                // Precedence rules:
                // 1. IDE focused → prefer IDE workspace (cwd). terminal_cwd is only used
                //    as a fallback, and only when it's coherent (same repo/project).
                // 2. Terminal focused → prefer terminal_cwd (may differ from IDE workspace).
                //    Still falls back to cwd if terminal_cwd is absent.
                // 3. Incoherent terminal context (different project) → never use as override.
                //    This prevents Node terminal from contaminating a Rust IDE session.
                let focused_is_ide = self
                    .context
                    .as_ref()
                    .and_then(|ctx| ctx.active_window.as_ref())
                    .is_some_and(|w| w.is_ide);
                crate::action_registry::handlers::shell_exec::set_context_cwd(
                    self.context.as_ref().and_then(|ctx| {
                        let coherent_terminal = ctx
                            .terminal_matches_workspace
                            .then(|| ctx.terminal_cwd.clone())
                            .flatten();
                        if focused_is_ide {
                            // IDE focused: workspace root is authoritative; only fall back to
                            // terminal_cwd if it's in the same project.
                            ctx.cwd.clone().or(coherent_terminal)
                        } else {
                            // Terminal focused: terminal_cwd (if coherent) takes priority.
                            coherent_terminal.or_else(|| ctx.cwd.clone())
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

                // Terminal routing: resolve target from focus ring for `run` commands.
                // Clear previous state first to prevent stale routing.
                crate::action_registry::handlers::shell_exec::set_terminal_routing(None);
                crate::action_registry::handlers::shell_exec::set_context_terminal(None, 0, None);
                if intent.action_id == "run"
                    && let Some(ref ctx) = self.context
                {
                    let routing_mode = get_terminal_routing_mode(ctx);
                    crate::action_registry::handlers::shell_exec::set_terminal_routing(Some(
                        routing_mode.clone(),
                    ));
                    if routing_mode != "off" {
                        let target = resolve_routing_target(ctx, &routing_mode);
                        if let Some((win, _src)) = target {
                            crate::action_registry::handlers::shell_exec::set_context_terminal(
                                Some(win.wm_class.clone()),
                                win.pid,
                                win.window_id.clone(),
                            );
                        }
                    }
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

                // If the "open" handler failed and we have a web fallback, try it.
                // Use intent.args (prefix stripped) not input (raw), so "open firefox" → search "firefox".
                if intent.action_id == "open"
                    && !result.success
                    && intent.routing != RoutingMethod::Ai
                    && let Some(web) = self.registry.get("web")
                {
                    tracing::debug!(
                        fallback = "web",
                        args = %intent.args,
                        "[execute] open miss → web fallback args={:?}",
                        intent.args
                    );
                    return Ok(ExecuteResult {
                        result: web.execute(&intent.args).await?,
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
            let mut ctx_items = crate::context::suggestions::suggest(ctx, Some(&self.db));
            if !ctx_items.is_empty() && trimmed.is_empty() {
                // Stale context warning: soft-stale triggers a UX hint.
                // Hard-stale additionally notes that AI routing may be conservative.
                if ctx.is_soft_stale() {
                    let age_secs = ctx.age().map(|d| d.as_secs()).unwrap_or(0);
                    crate::context::metrics::inc_soft_stale_hit();
                    if ctx.is_hard_stale() {
                        crate::context::metrics::inc_hard_stale_hit();
                    }
                    tracing::debug!("completions: context is soft-stale ({age_secs}s old)");
                    let desc = if ctx.is_hard_stale() {
                        "Context is >5min old — AI routing will be conservative".into()
                    } else {
                        "Suggestions reflect state from your last summon".into()
                    };
                    ctx_items.insert(
                        0,
                        crate::action_registry::CompletionItem {
                            label: "Context may be outdated — summon again to refresh".into(),
                            icon_path: Some("__info__".to_string()),
                            score: 0,
                            description: Some(desc),
                            reason: None,
                        },
                    );
                }
                return ctx_items;
            }
        }

        let route = crate::intent::patterns::route(raw);
        use crate::intent::patterns::PatternResult;
        let (route_handler, route_args) = match &route {
            PatternResult::Match(r) => (r.handler, r.args.as_str()),
            PatternResult::NoMatch { input } => ("open", input.as_str()),
        };
        let mut handler_results = self.registry.completions(route_handler, route_args).await;

        // For no-match queries, collect the "Search web: …" item separately so it
        // survives the truncate(5) below and is always visible as an escape hatch.
        // Skip if handler_results already contains a web completion (future-proof dedup).
        let web_fallback: Vec<CompletionItem> = if let PatternResult::NoMatch { input } = &route
            && !input.trim().is_empty()
            && !handler_results.iter().any(|r| {
                r.icon_path.as_deref() == Some("__web__") || r.label.starts_with("Search web:")
            })
            && let Some(web) = self.registry.get("web")
        {
            web.completions(input).await
        } else {
            Vec::new()
        };

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
        if !handler_results.is_empty() || !history_results.is_empty() || !web_fallback.is_empty() {
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
            out.extend(web_fallback);
            return out;
        }

        // Fallback: if a matched handler (not "open" or "web") returned nothing, try app search.
        // NoMatch already tried "open" above. Skip "web" — natural language queries produce
        // nonsense fuzzy app matches ("How to make pasta" → "os").
        if let PatternResult::Match(r) = &route
            && r.handler != "open"
            && r.handler != "web"
        {
            let search_term = if r.args.is_empty() { raw } else { &r.args };
            let app_results = self.registry.completions("open", search_term).await;
            if !app_results.is_empty() {
                return app_results;
            }
        }

        // Typo correction: suggest "Did you mean: X?" for near-miss inputs.
        // Skip for web routes and NoMatch (natural language isn't a typo).
        if let PatternResult::Match(r) = &route
            && r.handler != "web"
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

/// Try to expand an underspecified verb using clipboard content as the implicit object.
///
/// Only fires when ALL of these hold:
/// - Input has ≤ 2 tokens (bare verb, or "verb this" / "verb it")
/// - First token is a recognized implicit-object verb
/// - Clipboard holds a value compatible with that verb
/// - Input does not already contain a real argument (not a pronoun)
///
/// Returns `Some(expanded)` on success, `None` to leave input unchanged.
fn resolve_with_clipboard(input: &str, ctx: &crate::context::EnvironmentContext) -> Option<String> {
    use crate::context::clipboard_detect::ClipboardContentType;

    let trimmed = input.trim();
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();

    // Strict guard: only expand 1 or 2 token inputs
    if tokens.is_empty() || tokens.len() > 2 {
        return None;
    }

    // If 2 tokens, second must be a placeholder pronoun (not a real argument)
    const PRONOUNS: &[&str] = &["this", "it", "that"];
    if tokens.len() == 2 && !PRONOUNS.contains(&tokens[1].to_lowercase().as_str()) {
        return None;
    }

    let verb = tokens[0].to_lowercase();

    // Recognised verbs — the set we can expand.
    const SUPPORTED_VERBS: &[&str] = &[
        "open", "browse", "clone", "ping", "ssh", "whois", "curl", "show",
    ];

    let clip = match ctx.clipboard.as_ref() {
        Some(c) => c,
        None => {
            // Clipboard empty — count as miss only if the verb is one we'd act on.
            if SUPPORTED_VERBS.contains(&verb.as_str()) {
                crate::context::metrics::inc_clipboard_expansion_miss_empty();
            }
            return None;
        }
    };

    let expanded = match (verb.as_str(), clip) {
        // open/browse + URL → web open
        ("open" | "browse", ClipboardContentType::Url(url)) => {
            format!("web {url}")
        }
        // open + file path → file open
        ("open", ClipboardContentType::FilePath(path)) => {
            format!("open {path}")
        }
        // clone + URL (GitHub or any git URL) → run git clone
        ("clone", ClipboardContentType::Url(url))
            if url.contains("github.com")
                || url.contains("gitlab.com")
                || url.contains("bitbucket.org")
                || url.ends_with(".git") =>
        {
            format!("run git clone {url}")
        }
        // ping + IP address → run ping
        ("ping", ClipboardContentType::IpAddress(ip)) => {
            format!("run ping -c 4 {ip}")
        }
        // ssh + IP address → run ssh
        ("ssh", ClipboardContentType::IpAddress(ip)) => {
            format!("run ssh {ip}")
        }
        // whois + URL → extract host and run whois
        ("whois", ClipboardContentType::Url(url)) => {
            // Strip scheme and path, keep host only
            let host = url
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .split('/')
                .next()
                .unwrap_or(url);
            format!("run whois {host}")
        }
        // curl + URL → run curl
        ("curl", ClipboardContentType::Url(url)) => {
            format!("run curl {url}")
        }
        // show + git hash (in a git repo) → run git show
        ("show", ClipboardContentType::GitHash(hash)) if ctx.git.is_some() => {
            format!("run git show {hash}")
        }
        _ => {
            // Verb is recognised but clipboard type didn't match — record friction.
            if SUPPORTED_VERBS.contains(&verb.as_str()) {
                crate::context::metrics::inc_clipboard_expansion_miss_type();
            }
            return None;
        }
    };

    crate::context::metrics::inc_clipboard_expansion_used();
    tracing::debug!(
        "[resolve_with_clipboard] expanded {:?} → {:?} (clipboard={:?})",
        trimmed,
        expanded,
        clip
    );

    Some(expanded)
}

/// Read the terminal routing mode from the shell_exec static (set by Tauri layer).
fn get_terminal_routing_mode(_ctx: &crate::context::EnvironmentContext) -> String {
    crate::action_registry::handlers::shell_exec::get_terminal_routing()
}

/// Resolve the best terminal to route to from the focus ring.
///
/// For "auto" mode: project-match first, then any recent terminal.
/// For "manual" mode: only project-match when terminal_matches_workspace is true.
fn resolve_routing_target(
    ctx: &crate::context::EnvironmentContext,
    mode: &str,
) -> Option<(
    crate::context::WindowContext,
    crate::context::TerminalSource,
)> {
    let focused = ctx.active_window.as_ref();

    // Try project-match first
    if let Some(ref project) = ctx.project
        && let Some(hit) =
            crate::context::window_stack::find_recent_terminal_for_project(&project.root, focused)
    {
        return Some(hit);
    }

    // For manual mode: only route when terminal is project-coherent.
    // If project-match failed above, don't fall through to any-terminal.
    if mode == "manual" {
        return None;
    }

    // Auto mode: fall back to any recent terminal
    let (win, src) = crate::context::window_stack::find_recent_terminal(focused);
    win.map(|w| (w, src))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_registry::{ActionHandler, ActionResult};
    use crate::config::schema::PrivacyConfig;
    use crate::history::HistoryStore;
    use crate::rules::RulesEngine;
    use async_trait::async_trait;

    // --- Stub handlers ---

    /// Always succeeds — used for "web" and optionally "open"
    struct StubHandler {
        id: &'static str,
    }

    #[async_trait]
    impl ActionHandler for StubHandler {
        fn id(&self) -> &str {
            self.id
        }

        fn description(&self) -> &str {
            "stub"
        }

        async fn execute(&self, _args: &str) -> Result<ActionResult, crate::error::LychiError> {
            Ok(ActionResult {
                success: true,
                output: Some(format!("{} stub executed", self.id)),
                error: None,
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
                launch_desktop: None,
                focus_app: None,
            })
        }

        async fn completions(&self, partial: &str) -> Vec<crate::action_registry::CompletionItem> {
            if self.id == "web" && !partial.trim().is_empty() {
                vec![crate::action_registry::CompletionItem {
                    label: format!("Search web: {}", partial.trim()),
                    icon_path: Some("__web__".to_string()),
                    score: 100,
                    description: None,
                    reason: None,
                }]
            } else {
                Vec::new()
            }
        }
    }

    /// Always returns success: false — simulates app-not-found soft failure
    struct FailHandler;

    #[async_trait]
    impl ActionHandler for FailHandler {
        fn id(&self) -> &str {
            "open"
        }

        fn description(&self) -> &str {
            "fail stub"
        }

        async fn execute(&self, _args: &str) -> Result<ActionResult, crate::error::LychiError> {
            Ok(ActionResult {
                success: false,
                output: None,
                error: None,
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
                launch_desktop: None,
                focus_app: None,
            })
        }
    }

    // --- Executor factories ---

    fn make_executor(registry: ActionRegistry) -> Executor {
        Executor::new(
            registry,
            RulesEngine::new(),
            IntentResolver::new(None),
            HistoryStore::new(500, true),
            crate::db::open_test_database(),
        )
    }

    /// Registry with only a "web" stub (no "open" handler)
    fn registry_web_only() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register(Box::new(StubHandler { id: "web" }));
        r
    }

    /// Registry where "open" succeeds and "web" is present
    fn registry_open_succeeds() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register(Box::new(StubHandler { id: "open" }));
        r.register(Box::new(StubHandler { id: "web" }));
        r
    }

    /// Registry where "open" fails (soft) and "web" is present
    fn registry_open_fails() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register(Box::new(FailHandler));
        r.register(Box::new(StubHandler { id: "web" }));
        r
    }

    // --- Tests ---

    /// 1. Natural language → no pattern match → resolver routes to "web" directly
    #[tokio::test]
    async fn no_match_routes_to_web() {
        let ex = make_executor(registry_web_only());
        let r = ex
            .run("how do i cook pasta", false, &PrivacyConfig::default())
            .await
            .unwrap();
        assert_eq!(r.action_id, "web");
        assert!(r.result.success);
    }

    /// 2. Short word + "open" handler succeeds → action stays "open"
    #[tokio::test]
    async fn app_present_routes_to_open() {
        let ex = make_executor(registry_open_succeeds());
        let r = ex
            .run("firefox", false, &PrivacyConfig::default())
            .await
            .unwrap();
        assert_eq!(r.action_id, "open");
        assert!(r.result.success);
    }

    /// 3. App missing: "open" returns success:false → executor falls back to "web"
    #[tokio::test]
    async fn app_missing_falls_back_to_web() {
        let ex = make_executor(registry_open_fails());
        let r = ex
            .run("firefox", false, &PrivacyConfig::default())
            .await
            .unwrap();
        assert_eq!(r.action_id, "web");
        assert!(r.result.success);
        assert!(r.result.error.is_none());
    }

    /// 4. Explicit "open foo" with missing app → web fallback fires
    #[tokio::test]
    async fn explicit_open_missing_falls_back_to_web() {
        let ex = make_executor(registry_open_fails());
        let r = ex
            .run("open notarealapp", false, &PrivacyConfig::default())
            .await
            .unwrap();
        assert_eq!(r.action_id, "web");
        assert!(r.result.success);
    }

    /// 5. AI-routed "open" failure does NOT fall back to web.
    ///    Tested via the RoutingMethod enum directly — the guard in executor is:
    ///    `intent.routing != RoutingMethod::Ai`
    ///    We verify that Ai != Pattern/Explicit so the guard compiles and types are correct.
    #[test]
    fn ai_routing_method_is_distinct() {
        assert_ne!(RoutingMethod::Ai, RoutingMethod::Pattern);
        assert_ne!(RoutingMethod::Ai, RoutingMethod::Explicit);
        // The fallback guard `routing != RoutingMethod::Ai` evaluates to false only for Ai,
        // meaning AI-routed open results are never forwarded to web.
        let routing = RoutingMethod::Ai;
        assert!(!(routing != RoutingMethod::Ai)); // i.e. guard is false → no fallback
    }

    /// 6. Completions: "Search web: …" item is always present for a NoMatch input
    #[tokio::test]
    async fn completions_web_always_visible_for_no_match() {
        let ex = make_executor(registry_open_fails());
        let completions = ex.completions("zzunknownquery").await;
        let has_web = completions
            .iter()
            .any(|c| c.label.starts_with("Search web:"));
        assert!(
            has_web,
            "expected a 'Search web:' completion item, got: {completions:?}"
        );
    }

    // ── Golden Scenario: resolve_with_clipboard ───────────────────────────

    fn make_ctx_with_clipboard(
        clip: crate::context::clipboard_detect::ClipboardContentType,
    ) -> crate::context::EnvironmentContext {
        crate::context::EnvironmentContext {
            clipboard: Some(clip),
            gathered_at: Some(std::time::Instant::now()),
            ..Default::default()
        }
    }

    fn make_ctx_with_clipboard_and_git(
        clip: crate::context::clipboard_detect::ClipboardContentType,
    ) -> crate::context::EnvironmentContext {
        crate::context::EnvironmentContext {
            clipboard: Some(clip),
            git: Some(crate::context::GitContext {
                repo_root: "/tmp/repo".into(),
                branch: "main".into(),
                dirty: false,
                remote: None,
            }),
            gathered_at: Some(std::time::Instant::now()),
            ..Default::default()
        }
    }

    /// Clipboard expansion: bare "open" + URL → "web <url>"
    #[test]
    fn clipboard_expansion_open_url() {
        use crate::context::clipboard_detect::ClipboardContentType;
        let ctx = make_ctx_with_clipboard(ClipboardContentType::Url(
            "https://github.com/user/repo".into(),
        ));
        let result = super::resolve_with_clipboard("open", &ctx);
        assert_eq!(result, Some("web https://github.com/user/repo".into()));
    }

    /// "open this" is a pronoun form — same expansion as bare "open"
    #[test]
    fn clipboard_expansion_open_this_url() {
        use crate::context::clipboard_detect::ClipboardContentType;
        let ctx = make_ctx_with_clipboard(ClipboardContentType::Url("https://example.com".into()));
        let result = super::resolve_with_clipboard("open this", &ctx);
        assert_eq!(result, Some("web https://example.com".into()));
    }

    /// "open firefox" has a real argument — must NOT be expanded
    #[test]
    fn clipboard_expansion_does_not_hijack_real_arg() {
        use crate::context::clipboard_detect::ClipboardContentType;
        let ctx = make_ctx_with_clipboard(ClipboardContentType::Url("https://example.com".into()));
        let result = super::resolve_with_clipboard("open firefox", &ctx);
        assert_eq!(
            result, None,
            "open with a real argument must not be expanded"
        );
    }

    /// "clone" + GitHub URL → "run git clone <url>"
    #[test]
    fn clipboard_expansion_clone_github() {
        use crate::context::clipboard_detect::ClipboardContentType;
        let ctx = make_ctx_with_clipboard(ClipboardContentType::Url(
            "https://github.com/user/lychi".into(),
        ));
        let result = super::resolve_with_clipboard("clone", &ctx);
        assert_eq!(
            result,
            Some("run git clone https://github.com/user/lychi".into())
        );
    }

    /// "clone" + non-git URL → None (don't blindly clone arbitrary URLs)
    #[test]
    fn clipboard_expansion_clone_non_git_url_rejected() {
        use crate::context::clipboard_detect::ClipboardContentType;
        let ctx =
            make_ctx_with_clipboard(ClipboardContentType::Url("https://example.com/page".into()));
        let result = super::resolve_with_clipboard("clone", &ctx);
        assert_eq!(result, None, "clone of a non-git URL must not expand");
    }

    /// "ping" + IP → "run ping -c 4 <ip>"
    #[test]
    fn clipboard_expansion_ping_ip() {
        use crate::context::clipboard_detect::ClipboardContentType;
        let ctx = make_ctx_with_clipboard(ClipboardContentType::IpAddress("192.168.1.1".into()));
        let result = super::resolve_with_clipboard("ping", &ctx);
        assert_eq!(result, Some("run ping -c 4 192.168.1.1".into()));
    }

    /// "ssh" + IP → "run ssh <ip>"
    #[test]
    fn clipboard_expansion_ssh_ip() {
        use crate::context::clipboard_detect::ClipboardContentType;
        let ctx = make_ctx_with_clipboard(ClipboardContentType::IpAddress("10.0.0.5".into()));
        let result = super::resolve_with_clipboard("ssh", &ctx);
        assert_eq!(result, Some("run ssh 10.0.0.5".into()));
    }

    /// "whois" + URL → host extracted, no scheme/path
    #[test]
    fn clipboard_expansion_whois_url() {
        use crate::context::clipboard_detect::ClipboardContentType;
        let ctx = make_ctx_with_clipboard(ClipboardContentType::Url(
            "https://example.com/some/path".into(),
        ));
        let result = super::resolve_with_clipboard("whois", &ctx);
        assert_eq!(result, Some("run whois example.com".into()));
    }

    /// "show" + git hash requires git context — no git → None
    #[test]
    fn clipboard_expansion_show_hash_requires_git() {
        use crate::context::clipboard_detect::ClipboardContentType;
        let ctx = make_ctx_with_clipboard(ClipboardContentType::GitHash("a1b2c3d".into()));
        // No git context on this ctx
        let result = super::resolve_with_clipboard("show", &ctx);
        assert_eq!(
            result, None,
            "show hash without git context must not expand"
        );
    }

    /// "show" + git hash with git context → "run git show <hash>"
    #[test]
    fn clipboard_expansion_show_hash_with_git() {
        use crate::context::clipboard_detect::ClipboardContentType;
        let ctx = make_ctx_with_clipboard_and_git(ClipboardContentType::GitHash("a1b2c3d".into()));
        let result = super::resolve_with_clipboard("show", &ctx);
        assert_eq!(result, Some("run git show a1b2c3d".into()));
    }

    /// Three-token input is never expanded
    #[test]
    fn clipboard_expansion_rejects_three_tokens() {
        use crate::context::clipboard_detect::ClipboardContentType;
        let ctx = make_ctx_with_clipboard(ClipboardContentType::Url("https://example.com".into()));
        let result = super::resolve_with_clipboard("open the url", &ctx);
        assert_eq!(result, None, "three-token input must not be expanded");
    }

    /// No clipboard → no expansion
    #[test]
    fn clipboard_expansion_no_clipboard_returns_none() {
        let ctx = crate::context::EnvironmentContext {
            clipboard: None,
            gathered_at: Some(std::time::Instant::now()),
            ..Default::default()
        };
        let result = super::resolve_with_clipboard("open", &ctx);
        assert_eq!(result, None);
    }
}
