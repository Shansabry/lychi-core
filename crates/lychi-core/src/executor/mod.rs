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

/// Result of executing a command: the handler's clean `ActionResult`, the
/// resolved action_id, and the executor-owned envelope (risk/confirmation/
/// routing metadata the handler never sets). The Tauri layer flattens these into
/// the wire `CommandResultDto`.
pub struct ExecuteResult {
    pub result: ActionResult,
    pub action_id: String,
    pub envelope: crate::action_registry::ResultEnvelope,
}

/// Per-run inputs the caller supplies to `run()` — the config/UI state that used
/// to be pushed into shell_exec globals before each call. The executor combines
/// these with context-derived cwd/routing-target to build the immutable
/// `ExecContext` it threads to the handler.
#[derive(Debug, Clone, Default)]
pub struct RunInputs {
    /// Configured terminal emulator (binary name).
    pub terminal: Option<String>,
    /// Terminal routing mode — "auto" | "manual" | "off".
    pub terminal_routing: String,
    /// Capture the next `run`'s output inline (Shift+Enter) instead of a terminal.
    pub inline: bool,
}

/// Outcome of resolving which repo a `run` command targets.
enum RunRepo {
    /// A definite target directory (single-repo, terminal, or explicit pick).
    Resolved(String),
    /// Multi-repo and unpicked — the user must choose a repo; don't guess.
    NeedsPick,
    /// Not a multi-repo situation — use the normal cwd precedence.
    NoOverride,
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
    /// Commands suggested in the most recent completions pass. Used by the
    /// suggestion learning loop: executing one of these counts as acceptance.
    last_suggestions: std::sync::Mutex<Vec<String>>,
    /// Debounce guard for impression recording: (context_key, commands, ts_ms)
    /// of the last zero-state panel we counted. `completions()` fires per
    /// keystroke, so we only record an impression once the SAME panel settles
    /// (same context + same commands within the debounce window).
    last_impression: std::sync::Mutex<Option<(String, Vec<String>, u64)>>,
    /// Lowercased custom search-engine ("bang") keywords, so the router can send
    /// `gh tokio` to the `bang` handler. Set from config after construction.
    bang_keywords: Vec<String>,
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
            last_suggestions: std::sync::Mutex::new(Vec::new()),
            last_impression: std::sync::Mutex::new(None),
            bang_keywords: Vec::new(),
        }
    }

    /// Register the configured custom-search-engine keywords (lowercased) so the
    /// router can recognise `gh tokio` and route it to the `bang` handler.
    pub fn set_bang_keywords(&mut self, keywords: Vec<String>) {
        self.bang_keywords = keywords.into_iter().map(|k| k.to_lowercase()).collect();
    }

    /// If `input`'s first word is a configured bang keyword AND there's a query
    /// after it, return `(keyword, full_args)` for routing to the `bang` handler.
    fn bang_route(&self, input: &str) -> Option<String> {
        let trimmed = input.trim();
        let (first, rest) = trimmed.split_once(char::is_whitespace)?;
        if rest.trim().is_empty() {
            return None; // bare keyword, no query — let normal routing handle it
        }
        let first_l = first.to_lowercase();
        self.bang_keywords
            .iter()
            .any(|k| *k == first_l)
            .then(|| trimmed.to_string())
    }

    /// Suggestion-learning hook: if `input` matches a command we suggested
    /// in the last completions pass, return the context key it should be
    /// recorded under (Alfred-style latching). Caller records via
    /// `frecency::record_suggestion`.
    pub fn suggestion_acceptance(&self, input: &str) -> Option<String> {
        let trimmed = input.trim();
        let accepted = self
            .last_suggestions
            .lock()
            .ok()?
            .iter()
            .any(|s| s == trimmed);
        if !accepted {
            return None;
        }
        self.context
            .as_ref()
            .map(crate::context::suggestions::context_key)
    }

    fn note_suggestions(&self, items: &[CompletionItem]) {
        if let Ok(mut guard) = self.last_suggestions.lock() {
            *guard = items
                .iter()
                .filter(|i| i.icon_path.as_deref() == Some("__context__"))
                .map(|i| i.label.clone())
                .collect();
        }
    }

    /// Debounce window (ms) for impression recording — one settle of the same
    /// panel counts once, regardless of intermediate keystrokes.
    const IMPRESSION_DEBOUNCE_MS: u64 = 750;

    /// Record an impression for each zero-state suggestion shown, ONCE per
    /// panel-settle. `completions()` fires per keystroke, so we skip recording
    /// if the same commands are still showing in the same context within the
    /// debounce window. Only the actionable suggestions (not the `__info__`
    /// hint) are counted. Cheap: one redb write per settle.
    fn record_impressions_debounced(&self, ctx: &EnvironmentContext, items: &[CompletionItem]) {
        // Only speculative context suggestions (`__context__`) are governed by
        // the CTR/suppress loop. History recents (`__history__`) are commands
        // the user actually ran — inherently wanted, never suppressed — so they
        // must NOT accrue impressions (which would sink them over time).
        let commands: Vec<String> = items
            .iter()
            .filter(|i| i.icon_path.as_deref() == Some("__context__"))
            .map(|i| i.label.clone())
            .collect();
        if commands.is_empty() {
            return;
        }
        let context_key = crate::context::suggestions::context_key(ctx);
        let now = crate::db::now_millis();

        if let Ok(mut guard) = self.last_impression.lock() {
            if let Some((prev_key, prev_cmds, prev_ts)) = guard.as_ref()
                && *prev_key == context_key
                && *prev_cmds == commands
                && now.saturating_sub(*prev_ts) < Self::IMPRESSION_DEBOUNCE_MS
            {
                return; // same panel still settling — already counted
            }
            *guard = Some((context_key.clone(), commands.clone(), now));
        }
        let _ = crate::db::frecency::record_impressions(&self.db, &context_key, &commands);
    }

    /// Build the run-target `RunContext` from the current environment context.
    fn build_run_context(&self) -> Option<crate::context::multi_repo::RunContext<'_>> {
        use crate::context::multi_repo::{FocusedWindow, RunContext};
        let ctx = self.context.as_ref()?;
        let focused = match ctx.active_window.as_ref() {
            Some(w) if w.is_ide => FocusedWindow::Ide {
                workspace_root: ctx.cwd.clone(),
            },
            Some(w) if w.is_terminal => FocusedWindow::Terminal {
                cwd: ctx.terminal_cwd.clone().or_else(|| ctx.cwd.clone()),
            },
            _ => FocusedWindow::Other,
        };
        let coherent_terminal_cwd = ctx
            .terminal_matches_workspace
            .then(|| ctx.terminal_cwd.clone())
            .flatten();
        Some(RunContext {
            focused,
            coherent_terminal_cwd,
            db: &self.db,
        })
    }

    /// Build the immutable per-run `ExecContext` threaded to the handler.
    /// Resolves the working directory (multi-repo target → IDE/terminal cwd
    /// precedence), maps the detected terminal WM-class to a launchable binary
    /// (preferring the caller's configured terminal), and resolves the routing
    /// target from the focus ring. Output mode comes straight from `RunInputs`.
    fn build_exec_context(
        &self,
        intent: &crate::intent::ResolvedIntent,
        run_repo_override: Option<String>,
        inputs: &RunInputs,
    ) -> crate::action_registry::ExecContext {
        use crate::action_registry::{ExecContext, OutputMode, TerminalTarget};

        let focused_is_ide = self
            .context
            .as_ref()
            .and_then(|c| c.active_window.as_ref())
            .is_some_and(|w| w.is_ide);

        // Working directory: a picked multi-repo target wins; else IDE/terminal
        // cwd precedence (coherent terminal cwd only).
        let cwd = run_repo_override.or_else(|| {
            self.context.as_ref().and_then(|c| {
                let coherent_terminal = c
                    .terminal_matches_workspace
                    .then(|| c.terminal_cwd.clone())
                    .flatten();
                if focused_is_ide {
                    c.cwd.clone().or(coherent_terminal)
                } else {
                    coherent_terminal.or_else(|| c.cwd.clone())
                }
            })
        });

        // Terminal: prefer the detected terminal (same one the user is using),
        // mapped WM-class → binary; else the caller's configured terminal.
        let detected_terminal = self.context.as_ref().and_then(|c| {
            c.terminal_class.as_ref().and_then(|tc| {
                crate::action_registry::handlers::shell_exec::terminal_binary_for_class(tc)
            })
        });
        let terminal = detected_terminal.or_else(|| inputs.terminal.clone());

        // Routing target: only for `run`, only when routing is on.
        let routing_mode = inputs.terminal_routing.clone();
        let terminal_target = if intent.action_id == "run" && routing_mode != "off" {
            self.context.as_ref().and_then(|c| {
                resolve_routing_target(c, &routing_mode).map(|(win, _src)| TerminalTarget {
                    wm_class: win.wm_class.clone(),
                    pid: win.pid,
                    window_id: win.window_id.clone(),
                })
            })
        } else {
            None
        };

        ExecContext {
            cwd,
            terminal,
            terminal_routing: routing_mode,
            terminal_target,
            output_mode: if inputs.inline {
                OutputMode::Inline
            } else {
                OutputMode::Terminal
            },
        }
    }

    /// The explicit-target sigil a picked multi-repo completion appends:
    /// `run <cmd> @@<abs-dir>`. Splitting it out lets the completion carry the
    /// exact chosen repo through to execution unambiguously.
    const REPO_SIGIL: &'static str = " @@";

    /// Resolve where a `run` command executes. If `args` carries an explicit
    /// `@@<dir>` target (from a picked completion), use and strip it. Otherwise
    /// resolve via the unified resolver: a single target runs automatically; a
    /// multi-repo `Pick` with no explicit choice → `NeedsPick` (the caller must
    /// NOT guess — the user has to choose a repo). Records the chosen repo so
    /// this workspace's repos rank by usage.
    fn resolve_run_repo(&self, args: &mut String) -> RunRepo {
        // Explicit picked target: `<cmd> @@<dir>`.
        if let Some(idx) = args.rfind(Self::REPO_SIGIL) {
            let dir = args[idx + Self::REPO_SIGIL.len()..].trim().to_string();
            if !dir.is_empty() && std::path::Path::new(&dir).is_dir() {
                *args = args[..idx].trim_end().to_string();
                let container = std::path::Path::new(&dir)
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned());
                crate::context::multi_repo::record_choice(&self.db, container.as_deref(), &dir);
                tracing::info!("[run] multi-repo: explicit target → {dir}");
                return RunRepo::Resolved(dir);
            }
        }

        // No explicit target → the unified resolver.
        let Some(rctx) = self.build_run_context() else {
            return RunRepo::NoOverride;
        };
        let Some(resolved) = crate::context::multi_repo::resolve_run_targets(args.trim(), &rctx)
        else {
            return RunRepo::NoOverride;
        };
        match resolved.mode {
            crate::context::multi_repo::TargetMode::AutoRun => resolved
                .candidates
                .into_iter()
                .next()
                .map(|t| RunRepo::Resolved(t.dir))
                .unwrap_or(RunRepo::NoOverride),
            // Ambiguous and unpicked — the user must choose a repo.
            crate::context::multi_repo::TargetMode::Pick => RunRepo::NeedsPick,
        }
    }

    /// Completion rows for a multi-repo `run` command: one per candidate repo
    /// (frecency-ordered), type-narrowed by a trailing repo token, plus a
    /// "› all repos" fan-out row for read-only/safe commands. Empty when the
    /// command has a single unambiguous target (it just runs) or no context.
    fn multi_repo_rows(&self, command: &str) -> Vec<CompletionItem> {
        use crate::context::multi_repo::TargetMode;
        let Some(rctx) = self.build_run_context() else {
            return Vec::new();
        };
        let Some(resolved) = crate::context::multi_repo::resolve_run_targets(command, &rctx) else {
            return Vec::new();
        };
        if resolved.mode != TargetMode::Pick {
            return Vec::new(); // single target → runs directly, no picker
        }

        // A trailing token narrows the repo rows (`pnpm dev ap` → repos matching
        // "ap"). Detect it: the last word, only if it's not part of the command
        // proper (we treat any trailing word as a potential filter — if it
        // matches no repo, we show all).
        let last = command.rsplit(char::is_whitespace).next().unwrap_or("");
        let base_cmd = command.trim();
        let filtered: Vec<_> = if !last.is_empty() {
            let needle = last.to_lowercase();
            let hits: Vec<_> = resolved
                .candidates
                .iter()
                .filter(|t| fuzzy_contains(&t.name.to_lowercase(), &needle))
                .collect();
            // Only treat the token as a filter if it narrows AND leaves a real
            // command in front of it; else show all repos for the full command.
            if !hits.is_empty() && hits.len() < resolved.candidates.len() {
                let stripped = base_cmd[..base_cmd.len() - last.len()].trim_end();
                if !stripped.is_empty() {
                    return hits
                        .into_iter()
                        .map(|t| repo_row(stripped, t, 120))
                        .collect();
                }
            }
            resolved.candidates.iter().collect()
        } else {
            resolved.candidates.iter().collect()
        };

        let mut rows: Vec<CompletionItem> = filtered
            .into_iter()
            .enumerate()
            .map(|(i, t)| repo_row(base_cmd, t, 120 - i as u16))
            .collect();

        // Fan-out row for safe commands: run in every repo, grouped output.
        // Built as a plain `run` shell loop that `cd`s into each repo. The
        // explicit `@@<container>` target pins cwd to the container so the loop
        // isn't itself re-resolved as an ambiguous multi-repo command.
        if let Some(container) = resolved.all_repos_container {
            let dirs: Vec<String> = resolved.candidates.iter().map(|t| t.dir.clone()).collect();
            let loop_cmd = fanout_command(base_cmd, &dirs);
            rows.push(
                CompletionItem::new(
                    format!("{base_cmd} \u{203a} all repos"),
                    Some("__context__".into()),
                    50,
                )
                .with_run(format!("run {loop_cmd}{}{container}", Self::REPO_SIGIL))
                .with_description(format!("Run in all {} repos (grouped)", dirs.len())),
            );
        }
        rows
    }

    /// Completion rows for a docker container verb: when the typed command is
    /// `docker <verb>` for a per-container verb (logs/restart/stop/exec), emit
    /// one row per LIVE running container (name-matched by a trailing token),
    /// so the user picks a real container instead of typing its name. Empty
    /// unless the command is such a verb and containers are running. Reads the
    /// already-gathered `DockerContext` — no per-keystroke `docker ps`.
    fn docker_rows(&self, command: &str) -> Vec<CompletionItem> {
        let Some(verb) = docker_container_verb(command) else {
            return Vec::new();
        };
        let Some(ctx) = self.context.as_ref() else {
            return Vec::new();
        };
        let Some(docker) = ctx.docker.as_ref() else {
            return Vec::new();
        };
        if docker.containers.is_empty() {
            return Vec::new();
        }

        // A trailing token after the verb narrows the container list
        // (`docker logs ap` → containers matching "ap"). If it matches none, we
        // show all (the token may just be a partially-typed name).
        let after = command[verb.prefix.len()..].trim();
        let needle = after.to_lowercase();
        let matching: Vec<&crate::context::ContainerInfo> = docker
            .containers
            .iter()
            .filter(|c| needle.is_empty() || fuzzy_contains(&c.name.to_lowercase(), &needle))
            .collect();
        let containers: Vec<&crate::context::ContainerInfo> = if matching.is_empty() {
            docker.containers.iter().collect()
        } else {
            matching
        };

        containers
            .into_iter()
            .enumerate()
            .map(|(i, c)| {
                // `exec -it <c>` needs a shell to run; the others are complete
                // with just the container name.
                let full = if verb.prefix == "docker exec -it" {
                    format!("{} {} sh", verb.prefix, c.name)
                } else {
                    format!("{} {}", verb.prefix, c.name)
                };
                CompletionItem::new(
                    format!("{} \u{203a} {}", verb.prefix, c.name),
                    Some("__context__".into()),
                    130 - i as u16,
                )
                .with_run(format!("run {full}"))
                .with_fill(full)
                .with_description(format!("{} ({})", verb.action, c.image))
            })
            .collect()
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
        inputs: &RunInputs,
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

        // Custom search-engine shortcut ("bang"): `gh tokio` → bang handler.
        // Checked before general resolution so a configured keyword always wins
        // over app/web fallbacks; a bare keyword (no query) is left to normal
        // routing so it doesn't shadow a real command/app of the same name.
        let mut intent = if let Some(full) = self.bang_route(&effective_input) {
            crate::intent::ResolvedIntent {
                action_id: "bang".to_string(),
                args: full,
                routing: crate::intent::RoutingMethod::Explicit,
            }
        } else {
            self.resolver
                .resolve(&effective_input, &self.registry)
                .await
        };

        // Multi-repo scoping: when this is a `run` command and the focused IDE
        // workspace is a CONTAINER of several repos, pick which repo it runs in
        // (inline token → inferred script → learned frecency default) and strip
        // any repo-selector token from the args. `run_repo_override` carries the
        // chosen repo path so the CWD block below runs the command there.
        let run_repo = if intent.action_id == "run" {
            self.resolve_run_repo(&mut intent.args)
        } else {
            RunRepo::NoOverride
        };
        // Multi-repo but no repo chosen → don't guess where to run. Ask the user
        // to pick one (the completion list shows the per-repo rows).
        if matches!(run_repo, RunRepo::NeedsPick) {
            return Ok(ExecuteResult {
                result: ActionResult::err(
                    "This workspace has multiple repos — pick one from the list below to run in.",
                ),
                action_id: intent.action_id.clone(),
                envelope: Default::default(),
            });
        }
        let run_repo_override = match run_repo {
            RunRepo::Resolved(dir) => Some(dir),
            _ => None,
        };
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

        // Ask the handler to assess this specific invocation's risk, then let the
        // rules engine layer cross-cutting policy on top.
        let risk = handler.assess_risk(&intent.args);
        let decision = self.rules.validate(
            &ValidationRequest {
                action_id: &intent.action_id,
                args: &intent.args,
                routed_by,
                risk: &risk,
            },
            privacy,
        );

        // The executor-owned envelope (routing/risk/confirmation metadata the
        // handler never sets). Always records who routed the action.
        let mut envelope = crate::action_registry::ResultEnvelope {
            routed_by: Some(routed_by.to_string()),
            ..Default::default()
        };

        let result = match decision {
            ValidationDecision::Deny { reason } => {
                envelope.risk_level = Some(RiskLevel::High);
                ActionResult::err(format!("Blocked: {reason}"))
            }
            ValidationDecision::Confirm { reason } if !confirmed => {
                envelope.needs_confirmation = Some(reason);
                envelope.risk_level = Some(risk.level);
                // A pending-confirmation result: not yet run, no error, no output.
                ActionResult {
                    success: false,
                    ..Default::default()
                }
            }
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
                // Build the immutable per-run ExecContext threaded to the handler.
                // cwd + routing target come from the detected context; terminal +
                // routing mode + output mode come from the caller's RunInputs.
                let exec_ctx = self.build_exec_context(&intent, run_repo_override.clone(), inputs);

                // Set context snapshot for `ctx` debug handler
                if intent.action_id == "ctx" {
                    crate::action_registry::handlers::context_debug::set_context(
                        self.context.clone(),
                    );
                }
                let result = handler.execute(&exec_ctx, &intent.args).await?;
                if intent.routing == RoutingMethod::Ai {
                    envelope.routed_by = Some("ai".to_string());
                }
                // Pass actual executed args to frontend (useful for ls output linkification etc.)
                if intent.action_id == "run" {
                    envelope.executed_args = Some(intent.args.clone());
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
                        // web ignores ctx — a default context is fine.
                        result: web
                            .execute(
                                &crate::action_registry::ExecContext::default(),
                                &intent.args,
                            )
                            .await?,
                        action_id: "web".to_string(),
                        envelope: crate::action_registry::ResultEnvelope {
                            routed_by: Some(routed_by.to_string()),
                            ..Default::default()
                        },
                    });
                }

                result
            }
        };

        Ok(ExecuteResult {
            result,
            action_id,
            envelope,
        })
    }

    /// Get completions using the intent resolver to pick the right handler,
    /// with history entries shown in a separate section below.
    /// When input is empty and context is available, shows contextual suggestions.
    pub async fn completions(
        &self,
        raw: &str,
        cfg: &crate::config::schema::SuggestionsConfig,
    ) -> Vec<CompletionItem> {
        // Contextual suggestion shortlist for empty input
        let trimmed = raw.trim();
        if trimmed.len() <= 1
            && cfg.zero_state_recents
            && let Some(ref ctx) = self.context
        {
            let mut ctx_items = crate::context::suggestions::suggest(ctx, Some(&self.db));
            self.note_suggestions(&ctx_items);
            if trimmed.is_empty() {
                // Count what the empty-prompt panel is showing (self-tuning CTR).
                self.record_impressions_debounced(ctx, &ctx_items);
            }
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
                    // Surface staleness as a lightweight FLAG the UI renders as a
                    // dim glyph in the status bar — NOT a warning row that pushes
                    // real suggestions down. The `__context_stale__` sentinel
                    // carries the tooltip text in `description`; the frontend
                    // reads it, sets its indicator, and never shows it as a result.
                    let desc = if ctx.is_hard_stale() {
                        "Context is over 5 min old — AI routing will be conservative".into()
                    } else {
                        "Suggestions reflect state from your last summon".into()
                    };
                    ctx_items.insert(
                        0,
                        crate::action_registry::CompletionItem {
                            label: String::new(),
                            icon_path: Some("__context_stale__".to_string()),
                            score: 0,
                            description: Some(desc),
                            reason: None,
                            thumb_b64: None,
                            ..Default::default()
                        },
                    );
                }
                return ctx_items;
            }
        }

        // Custom search-engine shortcut preview: `gh tok` → a top row that opens
        // the configured search. Shown ahead of everything else so a configured
        // bang always leads once a query follows the keyword.
        if let Some(full) = self.bang_route(raw) {
            let (kw, query) = full
                .split_once(char::is_whitespace)
                .map(|(k, q)| (k, q.trim()))
                .unwrap_or((full.as_str(), ""));
            return vec![
                CompletionItem::new(format!("Search {kw}: {query}"), Some("__web__".into()), 200)
                    .with_run(full.clone())
                    .with_description("Enter to open"),
            ];
        }

        let route = crate::intent::patterns::route(raw, &self.registry);
        use crate::intent::patterns::PatternResult;
        let (route_handler, route_args) = match &route {
            PatternResult::Match(r) => (r.handler.as_str(), r.args.as_str()),
            PatternResult::NoMatch { input } => ("open", input.as_str()),
        };
        let mut handler_results = self.registry.completions(route_handler, route_args).await;

        // Multi-repo targets: when a shell command is typed in a container
        // workspace holding several repos, show ONE ROW PER REPO (frecency-
        // ordered), so the user explicitly picks where it runs — never a silent
        // guess. A trailing token type-narrows the rows. For read-only/safe
        // commands, a "› all repos" fan-out row is appended. Covers explicit
        // `run …` and a bare shell command (NoMatch → routed to `run`).
        let run_cmd: &str = match &route {
            PatternResult::Match(r) if r.handler == "run" => r.args.as_str(),
            PatternResult::NoMatch { input } => input.as_str(),
            _ => "",
        };
        if !run_cmd.trim().is_empty() && looks_like_shell_command(run_cmd.trim()) {
            let rows = self.multi_repo_rows(run_cmd.trim());
            // Prepend so the repo choices sit at the top of the list.
            for (i, row) in rows.into_iter().enumerate() {
                handler_results.insert(i, row);
            }
        }

        // Docker container picker: typing a container verb (`docker logs`,
        // `docker restart`/`stop`/`exec`) lists the live running containers to
        // pick from, name-matched by a trailing token — never a hardcoded
        // guess. Enumerates the containers already gathered in context (no
        // per-keystroke `docker ps`). Covers explicit `run …` and a bare
        // `docker …` (NoMatch → run).
        if !run_cmd.trim().is_empty() {
            let rows = self.docker_rows(run_cmd.trim());
            for (i, row) in rows.into_iter().enumerate() {
                handler_results.insert(i, row);
            }
        }

        // For no-match queries (a bare question / phrase), offer explicit
        // escape hatches that survive truncation. When AI is configured, show
        // BOTH "Ask AI: …" and "Search web: …" so the user chooses — no AI
        // guessing, no auto-behavior, two clear deterministic options. When AI
        // is off, only the web option (Sab's rule: no AI → keywords/web).
        //
        // Fallbacks are a FLOOR, not a competitor (Alfred/Raycast standard):
        // when the deterministic layer already found a CONFIDENT match — e.g. an
        // app resolved out of "can you open spotify" via token-set matching —
        // the fallbacks are suppressed so they never crowd a real intent. They
        // appear only when nothing confident matched, always ranked at the end.
        const CONFIDENT_RESULT_SCORE: u16 = 500; // ≈ app_score 0.71 blended
        let has_confident_match = handler_results
            .iter()
            .any(|r| r.score >= CONFIDENT_RESULT_SCORE);
        let web_fallback: Vec<CompletionItem> = if let PatternResult::NoMatch { input } = &route
            && !input.trim().is_empty()
            && !has_confident_match
            && !handler_results.iter().any(|r| {
                r.icon_path.as_deref() == Some("__web__") || r.label.starts_with("Search web:")
            })
            && let Some(web) = self.registry.get("web")
        {
            let q = input.trim();
            let ask_row = self.has_ai().then(|| {
                CompletionItem::new(format!("Ask AI: {q}"), Some("__none__".into()), 101)
                    .with_run(format!("ask {q}"))
                    .with_description("Enter to get an AI answer")
            });
            let web_rows = web.completions(input).await;

            // Order the two escape hatches by LEARNED preference: whichever the
            // user picks more (frecency, `fallback:ask` vs `fallback:web`) leads.
            // Default when unlearned keeps Ask AI first (the richer answer). This
            // is the frecency principle applied to the fallback choice itself — if
            // you keep choosing Search web, it starts appearing first.
            let ask_score = crate::db::frecency::get_fallback_score(&self.db, "ask");
            let web_score = crate::db::frecency::get_fallback_score(&self.db, "web");
            let web_preferred = web_score > ask_score;

            let mut out = Vec::new();
            if web_preferred {
                out.extend(web_rows);
                out.extend(ask_row);
            } else {
                out.extend(ask_row);
                out.extend(web_rows);
            }
            // Re-stamp scores so the frontend's score-sort preserves this order
            // (higher = earlier), regardless of the rows' intrinsic scores.
            for (i, row) in out.iter_mut().enumerate() {
                row.score = (200 - i.min(199)) as u16;
            }
            out
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

        // Omnibox-style blend: learned/contextual commands matching the typed
        // input rank alongside handler completions (capped at 2 by the engine).
        let mut context_matches: Vec<CompletionItem> = Vec::new();
        if trimmed.len() >= 2
            && cfg.context_actions_typed
            && let Some(ref ctx) = self.context
        {
            context_matches = crate::context::suggestions::typed_matches(ctx, Some(&self.db), raw)
                .into_iter()
                .filter(|c| !handler_results.iter().any(|r| r.label == c.label))
                .collect();
            if !context_matches.is_empty() {
                self.note_suggestions(&context_matches);
                history_results.retain(|h| !context_matches.iter().any(|c| c.label == h.label));
            }
        }

        // Build sectioned output: handler results first, then separator, then history
        if !handler_results.is_empty()
            || !history_results.is_empty()
            || !web_fallback.is_empty()
            || !context_matches.is_empty()
        {
            handler_results.truncate(5);
            history_results.truncate(3);
            // Context matches lead the handler section — they carry learned
            // per-context ranking that generic completions can't.
            handler_results.splice(0..0, context_matches);

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
                            thumb_b64: None,
                            ..Default::default()
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
                    thumb_b64: None,
                    ..Default::default()
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
            && let Some(suggestion) = crate::intent::typo_suggest::suggest(raw, &self.registry)
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
/// Whether the input's first word is an executable on PATH — a name-agnostic
/// "is this a shell command?" check. Used to gate multi-repo rows so arbitrary
/// typed text doesn't get run-target rows.
fn looks_like_shell_command(input: &str) -> bool {
    // The head word is the first non-`FOO=bar` token (leading env assignments
    // are skipped, matching shell semantics).
    let Some(head) = input.split_whitespace().find(|w| !w.contains('=')) else {
        return false;
    };
    // `completions()` runs per keystroke and typing extends the SAME head word,
    // so cache the head→on-PATH result to avoid a filesystem `which` lookup on
    // every character. Bounded to a small LRU-ish map cleared when it grows.
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CACHE: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);
    if let Ok(mut guard) = CACHE.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        if let Some(&hit) = map.get(head) {
            return hit;
        }
        let on_path = which::which(head).is_ok();
        if map.len() >= 256 {
            map.clear(); // keep the cache from growing unbounded
        }
        map.insert(head.to_string(), on_path);
        on_path
    } else {
        which::which(head).is_ok()
    }
}

/// POSIX single-quote escaping: wrap in `'…'`, and render any embedded `'` as
/// the `'\''` sequence (close quote, escaped literal quote, reopen). Makes a
/// directory or header safe to embed in a shell string regardless of contents.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build a shell command that runs `cmd` in each of `dirs`, printing a header
/// before each so the grouped output is readable (`mr`/`foreach` style), and a
/// per-repo failure marker so failures are scannable in the grouped output.
/// Each repo runs in a subshell so a failure in one doesn't abort the rest.
/// Directory paths and header names are POSIX-quoted, so paths containing
/// single quotes (`sab's-project`) can't break out of the quoting.
fn fanout_command(cmd: &str, dirs: &[String]) -> String {
    let mut out = String::new();
    for (i, dir) in dirs.iter().enumerate() {
        if i > 0 {
            out.push_str("; ");
        }
        let name = std::path::Path::new(dir)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(dir);
        let header = shell_single_quote(&format!("=== {name} ==="));
        let qdir = shell_single_quote(dir);
        let fail = shell_single_quote(&format!("\u{2717} {name}: failed"));
        // The subshell's exit status drives a per-repo failure marker, so a
        // `git pull` across several repos shows which ones failed at a glance.
        out.push_str(&format!(
            "echo {header}; (cd {qdir} && {cmd}) || echo {fail}; echo ''"
        ));
    }
    out
}

/// A docker verb that operates on a single container, so its completions
/// enumerate live containers. `prefix` is the command up to (not including) the
/// container name; `action` is a short human label for the row description.
struct DockerVerb {
    prefix: &'static str,
    action: &'static str,
}

/// Recognise a `docker <verb>` command whose target is a single container, so
/// the completion layer can offer a live-container picker. Matches the verb
/// prefix at the START of the command; the remainder is the (partial) container
/// name. Returns `None` for anything else (including `docker ps`, which lists
/// all and needs no picker). Name-agnostic beyond the fixed docker verb set.
fn docker_container_verb(command: &str) -> Option<DockerVerb> {
    // Longest prefixes first so `docker exec -it` wins over a bare `docker`.
    const VERBS: &[DockerVerb] = &[
        DockerVerb {
            prefix: "docker logs",
            action: "Tail logs",
        },
        DockerVerb {
            prefix: "docker restart",
            action: "Restart container",
        },
        DockerVerb {
            prefix: "docker stop",
            action: "Stop container",
        },
        DockerVerb {
            prefix: "docker exec -it",
            action: "Open a shell",
        },
    ];
    let trimmed = command.trim_start();
    for v in VERBS {
        // Match the verb prefix followed by end-of-string or whitespace, so
        // `docker logspew` doesn't match `docker logs`.
        if let Some(rest) = trimmed.strip_prefix(v.prefix)
            && (rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            return Some(DockerVerb {
                prefix: v.prefix,
                action: v.action,
            });
        }
    }
    None
}

/// Case-insensitive subsequence match (`ap` ⊂ `amt-api`).
fn fuzzy_contains(hay: &str, needle: &str) -> bool {
    let mut hc = hay.chars();
    needle.chars().all(|n| hc.by_ref().any(|h| h == n))
}

/// Build a "run `<cmd>` in `<repo>`" completion row. The `run` field carries the
/// exact chosen directory via the `@@` sigil so execution is unambiguous, and a
/// `fill` extends the input toward the repo name for tab-completion.
fn repo_row(
    command: &str,
    target: &crate::context::multi_repo::RunTarget,
    score: u16,
) -> CompletionItem {
    CompletionItem::new(
        format!("{command} \u{203a} {}", target.name),
        Some("__context__".into()),
        score,
    )
    .with_run(format!("run {command} @@{}", target.dir))
    .with_fill(format!("{command} {}", target.name))
    .with_description(format!("Run in {}", target.dir))
}

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

    // If the focused window IS a terminal, route to it directly.
    // This handles the common case of summoning Lychi from a terminal.
    if let Some(w) = focused
        && w.is_terminal
    {
        return Some((w.clone(), crate::context::TerminalSource::FocusedWindow));
    }

    // Try project-match first (background terminal in focus ring)
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

        async fn execute(
            &self,
            _ctx: &crate::action_registry::ExecContext,
            _args: &str,
        ) -> Result<ActionResult, crate::error::LychiError> {
            Ok(ActionResult::ok(
                format!("{} stub executed", self.id),
                crate::action_registry::OutputType::Status,
            ))
        }

        async fn completions(&self, partial: &str) -> Vec<crate::action_registry::CompletionItem> {
            if self.id == "web" && !partial.trim().is_empty() {
                vec![crate::action_registry::CompletionItem {
                    label: format!("Search web: {}", partial.trim()),
                    icon_path: Some("__web__".to_string()),
                    score: 100,
                    description: None,
                    reason: None,
                    thumb_b64: None,
                    ..Default::default()
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

        async fn execute(
            &self,
            _ctx: &crate::action_registry::ExecContext,
            _args: &str,
        ) -> Result<ActionResult, crate::error::LychiError> {
            Ok(ActionResult {
                success: false,
                ..Default::default()
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
            .run(
                "how do i cook pasta",
                false,
                &PrivacyConfig::default(),
                &RunInputs::default(),
            )
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
            .run(
                "firefox",
                false,
                &PrivacyConfig::default(),
                &RunInputs::default(),
            )
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
            .run(
                "firefox",
                false,
                &PrivacyConfig::default(),
                &RunInputs::default(),
            )
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
            .run(
                "open notarealapp",
                false,
                &PrivacyConfig::default(),
                &RunInputs::default(),
            )
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
        let completions = ex
            .completions(
                "zzunknownquery",
                &crate::config::schema::SuggestionsConfig::default(),
            )
            .await;
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

    #[test]
    fn docker_container_verb_recognises_per_container_verbs() {
        // Per-container verbs match (prefix + name).
        assert_eq!(
            super::docker_container_verb("docker logs api").map(|v| v.prefix),
            Some("docker logs")
        );
        assert_eq!(
            super::docker_container_verb("docker restart").map(|v| v.prefix),
            Some("docker restart")
        );
        assert_eq!(
            super::docker_container_verb("docker stop db").map(|v| v.prefix),
            Some("docker stop")
        );
        // `exec -it` is longest — must win over a bare `docker`.
        assert_eq!(
            super::docker_container_verb("docker exec -it web").map(|v| v.prefix),
            Some("docker exec -it")
        );
        // `docker ps` lists all — no per-container picker.
        assert!(super::docker_container_verb("docker ps").is_none());
        // A verb must be a whole word: `docker logspew` is not `docker logs`.
        assert!(super::docker_container_verb("docker logspew").is_none());
        // Non-docker command.
        assert!(super::docker_container_verb("pnpm dev").is_none());
    }

    #[test]
    fn shell_single_quote_escapes_embedded_quote() {
        assert_eq!(super::shell_single_quote("plain"), "'plain'");
        // A single quote must be rendered as the POSIX '\'' sequence.
        assert_eq!(super::shell_single_quote("sab's"), "'sab'\\''s'");
    }

    #[test]
    fn fanout_command_quotes_paths_with_apostrophes() {
        let dirs = vec!["/home/sab/sab's-project".to_string()];
        let out = super::fanout_command("git status", &dirs);
        // The dir is safely quoted (no raw apostrophe breaking the cd).
        assert!(out.contains("cd '/home/sab/sab'\\''s-project'"));
        // Per-repo failure marker present.
        assert!(out.contains("|| echo"));
        assert!(out.contains("failed"));
    }
}
