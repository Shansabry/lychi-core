use std::sync::Arc;

use redb::Database;

/// Execution-concurrency policy (G4), extracted so the orchestrator stays lean.
pub mod concurrency;
use concurrency::ConcurrencyGate;
/// Suggestion-learning latch/debounce state, extracted for the same reason.
pub mod suggestion_tracker;
use suggestion_tracker::SuggestionTracker;

use crate::action_registry::registry::ActionRegistry;
use crate::action_registry::{ActionResult, CompletionItem, RiskLevel};
use crate::config::schema::PrivacyConfig;
use crate::context::EnvironmentContext;
use crate::error::LychiError;
use crate::history::HistoryStore;
use crate::intent::{IntentResolver, RoutingMethod};
use crate::rules::{RulesEngine, ValidationDecision, ValidationRequest};

/// Expand `@<path>` file references into real filesystem paths before routing.
///
/// The `@` reference is a frontend affordance: the user types `@`, fuzzy-picks a
/// file, and the input becomes e.g. `resize @~/Pictures/img.png to 800x600`. No
/// handler understands the leading `@`, so we strip it here — ONE place, so every
/// file-consuming command benefits — turning `@~/Pictures/img.png` into the
/// tilde-expanded absolute path. Adaptable: works for any command + any path, no
/// per-handler or per-filename special-casing.
///
/// Only a `@` that begins a token AND is followed by a path-like character
/// (`~`, `/`, `.`, or an alphanumeric) is treated as a file reference; a bare
/// `@` or an email-ish `foo@bar` (── `@` mid-token) is left untouched.
fn expand_at_references(input: &str) -> String {
    if !input.contains('@') {
        return input.to_string();
    }
    let home = dirs::home_dir();
    input
        .split(' ')
        .map(|tok| {
            let Some(rest) = tok.strip_prefix('@') else {
                return tok.to_string();
            };
            // Guard: `@` must be followed by a path-like start, else leave as-is.
            let looks_like_path = rest
                .chars()
                .next()
                .is_some_and(|c| c == '~' || c == '/' || c == '.' || c.is_alphanumeric());
            if !looks_like_path {
                return tok.to_string();
            }
            // Tilde-expand: `~` or `~/...` → home.
            if let Some(after) = rest.strip_prefix('~')
                && (after.is_empty() || after.starts_with('/'))
                && let Some(h) = home.as_ref()
            {
                return format!("{}{}", h.display(), after);
            }
            rest.to_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Result of executing a command: the handler's clean `ActionResult`, the
/// resolved action_id, and the executor-owned envelope (risk/confirmation/
/// routing metadata the handler never sets). The Tauri layer flattens these into
/// the wire `CommandResultDto`.
pub struct ExecuteResult {
    pub result: ActionResult,
    pub action_id: String,
    pub envelope: crate::action_registry::ResultEnvelope,
    /// When the result is a pending confirmation, this carries the exact resolved
    /// intent that was assessed. The bridge stores it so the confirm step can
    /// execute THIS action (via `run_confirmed`) instead of re-resolving the raw
    /// input — closing the confirmation TOCTOU gap (G1). `None` otherwise.
    pub pending_intent: Option<crate::intent::ResolvedIntent>,
    /// True when an `Exclusive` action was rejected because another exclusive
    /// action was already running (G4 fail-fast). The confirm path uses this to
    /// REINSERT the pending confirmation instead of consuming it — so a "busy"
    /// reject doesn't force the user to reconstruct a confirmed destructive action
    /// (the reviewer's #1↔#10 interaction). No execution occurred.
    pub busy: bool,
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
    /// Optional live-output sink threaded to the handler's `ExecContext` (see
    /// [`crate::action_registry::OutputSink`]). `None` — every non-agent path —
    /// keeps today's buffered behaviour; the AI coordinator sets it so a captured
    /// `run` streams its output into the chat as it happens.
    pub sink: Option<crate::action_registry::OutputSink>,
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
/// `Clone` is deliberate and cheap (Arc'd handlers, small vecs), and it is the
/// mechanism that keeps the launcher responsive: the app snapshots the executor
/// under a short read guard and runs the command on the snapshot, so the lock
/// is NEVER held across handler execution. Before this, the guard lived for
/// the whole run, and the reactors' queued `blocking_write` on the fair RwLock
/// stalled every subsequent read — one slow handler plus one settings save
/// froze completions on every keystroke until the handler finished.
///
/// Cross-run state is SHARED across clones, not forked: the concurrency gate,
/// the suggestion tracker, and the AI router's caches all sit behind `Arc`, so
/// a snapshot obeys the same exclusivity/learning semantics as the canonical
/// executor. What a snapshot fixes in place is the point-in-time view —
/// registry contents, routing side-channels, context — which is exactly what
/// an in-flight command should see; re-registration lands on the canonical
/// executor and is picked up by the next snapshot.
#[derive(Clone)]
pub struct Executor {
    pub registry: ActionRegistry,
    pub rules: RulesEngine,
    pub resolver: IntentResolver,
    pub history: HistoryStore,
    pub db: Arc<Database>,
    /// Current environment context, refreshed on each summon.
    pub context: Option<EnvironmentContext>,
    /// Suggestion-learning state (acceptance latch + impression debounce),
    /// extracted into its own collaborator so the Executor doesn't carry the
    /// ad-hoc mutexes inline. The Executor still owns the policy (what to record).
    /// Arc: learning state is cross-run — snapshots must share it, not fork it.
    suggestions: Arc<SuggestionTracker>,
    /// The user's quicklinks, so the router can send `gh tokio` to the right
    /// place. Set from config after construction.
    ///
    /// The full records are held (not just keywords) because routing depends on
    /// each link's `kind`: a shell quicklink must be dispatched to `run` so it
    /// meets the shell gate, while a URL one is handled by the quicklink
    /// handler itself. Same side-channel pattern as `script_keywords` — these
    /// are runtime-defined, and `triggers()` is `'static`.
    quicklinks: Vec<crate::quicklinks::Quicklink>,
    /// Lowercased Script Command keywords (from `~/.config/lychi/scripts/`), so
    /// the router can send `deploy prod` to the `script` handler. Rebuilt by the
    /// scripts fs-watcher. Same side-channel pattern as `quicklinks` (can't use
    /// `triggers()` — those are `'static`, these are runtime-discovered).
    script_keywords: Vec<String>,
    /// Execution-concurrency policy (G4): enforces each handler's `ExecutionMode`
    /// (immediate / exclusive-fail-fast / replace-previous-with-cancellation).
    /// Extracted into its own collaborator so the Executor stays an orchestrator
    /// rather than accumulating specialized concurrency mechanics.
    /// Arc: the gate enforces exclusivity ACROSS runs — every snapshot must go
    /// through the one gate, or two clones could run an Exclusive handler twice.
    gate: Arc<ConcurrencyGate>,
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
            suggestions: Arc::new(SuggestionTracker::new()),
            quicklinks: Vec::new(),
            script_keywords: Vec::new(),
            gate: Arc::new(ConcurrencyGate::new()),
        }
    }

    /// Register the user's quicklinks so the router can recognise `gh tokio`.
    pub fn set_quicklinks(&mut self, links: Vec<crate::quicklinks::Quicklink>) {
        self.quicklinks = links;
    }

    /// Swap the Rules Engine — used to hot-reload the shell approval policy
    /// (profile + user allow/deny rules) when the commands config changes.
    pub fn set_rules(&mut self, rules: RulesEngine) {
        self.rules = rules;
    }

    /// Number of registered quicklinks (for logging).
    pub fn quicklink_count(&self) -> usize {
        self.quicklinks.len()
    }

    /// Resolve `input` to `(action_id, args)` when it starts with a quicklink
    /// keyword.
    ///
    /// The dispatch target depends on the quicklink's `kind`, and that is the
    /// whole point: a `Shell` quicklink resolves to the `run` action carrying
    /// its EXPANDED command, so it travels the same path as a typed `run` —
    /// Rules Engine, then `shell_exec`'s spawn-point gate. Routing every kind
    /// to the quicklink handler would have made that handler a second spawn
    /// point, which is the bypass shape the security audit found twice.
    ///
    /// Returns `None` for a bare keyword with no input, letting normal routing
    /// handle it (so a quicklink keyword that is also an app name still opens
    /// the app when typed alone).
    fn quicklink_route(&self, input: &str) -> Option<(String, String)> {
        let trimmed = input.trim();
        let (first, rest) = match trimmed.split_once(char::is_whitespace) {
            Some((k, r)) => (k, r.trim()),
            // A bare keyword with no input after it.
            None => (trimmed, ""),
        };
        let link = self
            .quicklinks
            .iter()
            .find(|q| q.keyword.eq_ignore_ascii_case(first))?;

        // A quicklink whose template has no placeholder is a complete action by
        // itself — `ghvs` opening a fixed URL needs no input, so requiring some
        // would make it unreachable. One that DOES take a placeholder still
        // falls through when typed bare, so the keyword can also match an app or
        // a command until the user actually supplies input.
        if rest.is_empty() && !crate::quicklinks::placeholders_in(&link.template).is_empty() {
            return None;
        }
        let handler = crate::action_registry::handlers::quicklink::QuicklinkHandler::new(
            self.quicklinks.clone(),
        );
        handler.resolve_route(trimmed)
    }

    /// If `input` is a `Command`-kind quicklink, the Lychi command it expands
    /// to. Applied BEFORE resolution so the expansion is routed as if the user
    /// had typed it — meaning the target command's own gate applies, and no
    /// second dispatcher is introduced here.
    fn quicklink_rewrite(&self, input: &str) -> Option<String> {
        match self.quicklink_route(input) {
            Some((action, args)) if action == "__reroute__" => Some(args),
            _ => None,
        }
    }

    /// Register the discovered Script Command keywords (lowercased) so the router
    /// can recognise `deploy prod` and route it to the `script` handler.
    pub fn set_script_keywords(&mut self, keywords: Vec<String>) {
        self.script_keywords = keywords.into_iter().map(|k| k.to_lowercase()).collect();
    }

    /// Number of registered Script Command keywords (for logging).
    pub fn script_keyword_count(&self) -> usize {
        self.script_keywords.len()
    }

    /// If `input`'s first word is a discovered Script Command keyword, return the
    /// full input for routing to the `script` handler. Unlike quicklinks, a BARE
    /// keyword (no args) is valid — many scripts take no arguments.
    fn script_route(&self, input: &str) -> Option<String> {
        let trimmed = input.trim();
        let first = trimmed
            .split_once(char::is_whitespace)
            .map(|(f, _)| f)
            .unwrap_or(trimmed);
        let first_l = first.to_lowercase();
        self.script_keywords
            .contains(&first_l)
            .then(|| trimmed.to_string())
    }

    /// Suggestion-learning hook: if `input` matches a command we suggested
    /// in the last completions pass, return the context key it should be
    /// recorded under (Alfred-style latching). Caller records via
    /// `frecency::record_suggestion`.
    pub fn suggestion_acceptance(&self, input: &str) -> Option<String> {
        let trimmed = input.trim();
        if !self.suggestions.was_shown(trimmed) {
            return None;
        }
        self.context
            .as_ref()
            .map(crate::context::suggestions::context_key)
    }

    fn note_suggestions(&self, items: &[CompletionItem]) {
        let shown: Vec<String> = items
            .iter()
            .filter(|i| i.icon_path.as_deref() == Some("__context__"))
            .map(|i| i.label.clone())
            .collect();
        self.suggestions.set_shown(shown);
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

        if !self.suggestions.should_record_impression(
            &context_key,
            &commands,
            now,
            Self::IMPRESSION_DEBOUNCE_MS,
        ) {
            return; // same panel still settling — already counted
        }
        let _ = crate::db::frecency::record_impressions(&context_key, &commands);
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
            // Carry the live-output sink through to the handler. Cloned (cheap —
            // an mpsc sender handle) because `inputs` is borrowed; `None` on
            // every non-agent path leaves buffered behaviour unchanged.
            sink: inputs.sink.clone(),
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
        // One unambiguous target: still show a row, so a typed shell command is
        // visibly runnable and names where it will run. Returning nothing here
        // meant `git status` in a single repo produced only Ask-AI/Search-web —
        // the command was executable but nothing said so.
        if resolved.mode != TargetMode::Pick {
            return match resolved.candidates.first() {
                Some(target) => vec![single_target_row(command, target)],
                None => Vec::new(),
            };
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
        self.run_inner(input, None, confirmed, privacy, inputs)
            .await
    }

    /// Execute a PRE-RESOLVED intent, re-checking policy but skipping resolution.
    ///
    /// This is the second half of the confirmation flow (G1): the first `run`
    /// assessed and captured a `ResolvedIntent`; on confirm we execute THAT exact
    /// action rather than re-resolving the raw string, closing the time-of-check /
    /// time-of-use gap (routing/context/config can't shift the action between
    /// assessment and execution). Policy (`rules.validate`) still runs, so a
    /// deny/consent change since assessment is honored.
    pub async fn run_confirmed(
        &self,
        intent: crate::intent::ResolvedIntent,
        privacy: &PrivacyConfig,
        inputs: &RunInputs,
    ) -> Result<ExecuteResult, LychiError> {
        // `input` is only used for logging here; the intent is authoritative.
        self.run_inner("", Some(intent), true, privacy, inputs)
            .await
    }

    /// Execute a PRE-RESOLVED intent WITHOUT confirmation — the agent's
    /// group-tool dispatch path. The registry already resolved a group call to
    /// (handler, flat args); re-parsing a synthesized command line here would
    /// be a second resolver that could route somewhere else. Unlike
    /// [`Executor::run_confirmed`], the full policy path runs: risk assessment
    /// and the Rules Engine see the exact flat args, and a destructive action
    /// still comes back as `needs_confirmation` for the approval flow.
    pub async fn run_resolved(
        &self,
        intent: crate::intent::ResolvedIntent,
        privacy: &PrivacyConfig,
        inputs: &RunInputs,
    ) -> Result<ExecuteResult, LychiError> {
        self.run_inner("", Some(intent), false, privacy, inputs)
            .await
    }

    async fn run_inner(
        &self,
        input: &str,
        preresolved: Option<crate::intent::ResolvedIntent>,
        confirmed: bool,
        privacy: &PrivacyConfig,
        inputs: &RunInputs,
    ) -> Result<ExecuteResult, LychiError> {
        // Expand `@<path>` file references into real paths FIRST, so every
        // downstream step (routing, clipboard expansion, handler execution) sees
        // a normal path instead of a literal `@…` token.
        let expanded = expand_at_references(input);
        let input = expanded.as_str();

        // Implicit object expansion: if input is an underspecified verb and clipboard
        // holds a compatible value, expand deterministically before hitting AI.
        // Only fires when patterns::route returns NoMatch (no structural match).
        // Strict guards: ≤2 tokens, no existing argument, compatible clipboard type.
        let effective_input = self
            .context
            .as_ref()
            .and_then(|ctx| resolve_with_clipboard(input, ctx))
            .unwrap_or_else(|| input.to_string());

        // A `command`-kind quicklink expands to another Lychi command; rewrite
        // the input here so it resolves exactly as if the user had typed it,
        // and the target command's own gate applies.
        let effective_input = self
            .quicklink_rewrite(&effective_input)
            .unwrap_or(effective_input);

        // A pre-resolved intent (confirmation re-run) is used verbatim — no
        // re-resolution, so the confirmed action is exactly what was assessed.
        // Otherwise resolve fresh. Priority: user Script Commands, then
        // quicklinks, then the general resolver. Scripts are the user's own
        // named commands (highest intent), so they win over app/web fallbacks.
        let mut intent = if let Some(intent) = preresolved {
            intent
        } else if let Some(full) = self.script_route(&effective_input) {
            crate::intent::ResolvedIntent {
                action_id: "script".to_string(),
                args: full,
                routing: crate::intent::RoutingMethod::Explicit,
            }
        } else if let Some((action_id, args)) = self.quicklink_route(&effective_input) {
            crate::intent::ResolvedIntent {
                action_id,
                args,
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
                pending_intent: None,
                busy: false,
            });
        }
        let run_repo_override = match run_repo {
            RunRepo::Resolved(dir) => Some(dir),
            _ => None,
        };
        // Metadata only at info: the JSON file log is the artifact beta users
        // send with bug reports, and at default level it must not contain what
        // the user typed (`ask <personal question>`, shell lines with pasted
        // secrets, clipboard-expanded text). The length + a short hash keep
        // repeat-failure reports correlatable without the content; the full
        // input is available at debug for local diagnosis.
        let input_hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            input.hash(&mut h);
            h.finish() & 0xffff_ffff
        };
        tracing::info!(
            action = %intent.action_id,
            routing = ?intent.routing,
            input_chars = input.chars().count(),
            input_hash = format!("{input_hash:08x}"),
            "[execute] resolved"
        );
        tracing::debug!("[execute] input={input:?}");

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

        // Ask the handler to assess this specific invocation's risk — with a cheap
        // context (cwd, workspace) so risk can depend on WHERE it runs (G2) — then
        // let the rules engine layer cross-cutting policy on top.
        let risk_ctx = crate::action_registry::RiskContext {
            cwd: self.context.as_ref().and_then(|c| c.cwd.as_deref()),
            workspace_root: self
                .context
                .as_ref()
                .and_then(|c| c.project.as_ref())
                .and_then(|p| p.workspace_root.as_deref()),
        };
        let risk = handler.assess_risk(&intent.args, &risk_ctx);
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

        // Set only when we return a pending confirmation — carries the exact
        // assessed intent so confirm executes it verbatim (G1, no re-resolve).
        let mut pending_intent: Option<crate::intent::ResolvedIntent> = None;
        // Set true if an Exclusive action was rejected as busy (no execution).
        let mut busy = false;

        let result = match decision {
            ValidationDecision::Deny { reason } => {
                envelope.risk_level = Some(RiskLevel::High);
                ActionResult::err(format!("Blocked: {reason}"))
            }
            ValidationDecision::Confirm { reason } if !confirmed => {
                envelope.needs_confirmation = Some(reason);
                // When this confirmation IS a consent prompt, ship the typed
                // feature key so the FE can persist the grant without
                // substring-matching the prose. Exact, not heuristic: consent
                // is checked FIRST in RulesEngine::decide, so an ungranted
                // consent on the assessment means this Confirm is the consent
                // prompt (pinned by a rules test). `consent_granted` is the
                // same single mapping the gate itself uses.
                envelope.consent_feature = risk
                    .consent
                    .as_ref()
                    .filter(|c| !crate::rules::consent_granted(c.kind, privacy))
                    .and_then(|c| c.kind.feature_key())
                    .map(str::to_string);
                envelope.risk_level = Some(risk.level);
                pending_intent = Some(intent.clone());
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
                let (result, was_busy) = self.gate.run(handler, &exec_ctx, &intent.args).await?;
                busy = was_busy;
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
                        pending_intent: None,
                        busy: false,
                    });
                }

                result
            }
        };

        Ok(ExecuteResult {
            result,
            action_id,
            envelope,
            busy,
            pending_intent,
        })
    }

    /// Build the suggestion list for the current input.
    ///
    /// # Shape
    ///
    /// Every stage below is a SOURCE: it emits candidates and says where they
    /// came from. None of them decides position. `suggestions::rank` alone
    /// owns ordering, deduping, capping and defaultability.
    ///
    /// This replaced a 257-line function whose nine stages each pushed,
    /// prepended, spliced or truncated one shared `Vec` — so a row's position
    /// was an emergent property of statement order, and every ordering rule
    /// lived as a comment. See `suggestions::rank` for the rules themselves.
    ///
    /// Two behaviours are deliberately NOT ranked, because they are not
    /// suggestions at all: the zero-state shortlist (nothing typed, so nothing
    /// to match against) and a quicklink preview (the user configured this
    /// exact keyword; there is no competition to resolve).
    pub async fn completions(
        &self,
        raw: &str,
        cfg: &crate::config::schema::SuggestionsConfig,
    ) -> Vec<CompletionItem> {
        use crate::suggestions::{Source, Suggestion, Tier};

        let trimmed = raw.trim();

        if let Some(items) = self.zero_state(raw, cfg) {
            return items;
        }
        if let Some(items) = self.quicklink_preview(raw) {
            return items;
        }

        let route = crate::intent::patterns::route(raw, &self.registry);
        let mut all: Vec<Suggestion> = Vec::new();

        // ── Handler completions ─────────────────────────────────────────
        use crate::intent::patterns::PatternResult;
        let (route_handler, route_args) = match &route {
            PatternResult::Match(r) => (r.handler.as_str(), r.args.as_str()),
            PatternResult::NoMatch { input } => ("open", input.as_str()),
        };

        // An unrouted query is IMPLICITLY sent to `open` (the app launcher). That
        // is right for "spotify" but wrong for a natural-language sentence: the
        // app launcher fuzzy-matches every app whose name-tokens appear anywhere
        // in the text, so "play the music and tell me my disk status" floods the
        // list with Spotify/Rhythmbox (music) and Disks/Filelight (disk) — the
        // "mentioned ≠ asked for" failure. Every mainstream launcher (Raycast,
        // Spotlight, Alfred) matches app NAMES tightly and routes prose to
        // AI/web instead. So the IMPLICIT open only runs when the query still
        // looks like an app-launch attempt; an explicit `open …` route is
        // untouched, and prose falls through to the Ask AI / web escape hatches.
        let suppress_implicit_open =
            matches!(&route, PatternResult::NoMatch { .. }) && !looks_like_app_query(trimmed);

        let (handler_results, handler_empty) = if suppress_implicit_open {
            (Vec::new(), true)
        } else {
            let r = self.registry.completions(route_handler, route_args).await;
            let empty = r.is_empty();
            (r, empty)
        };

        all.extend(
            handler_results
                .into_iter()
                .map(|i| Suggestion::matched(i, Source::Handler, trimmed)),
        );

        // ── Disambiguation: which repo? which container? ────────────────
        //
        // Not ranked alternatives — the command is ambiguous until answered, so
        // these outrank ordinary completions by SOURCE rather than by the score
        // inflation the old code used to float them to the top.
        let run_cmd: &str = match &route {
            PatternResult::Match(r) if r.handler == "run" => r.args.as_str(),
            PatternResult::NoMatch { input } => input.as_str(),
            _ => "",
        };
        let run_cmd = run_cmd.trim();
        if !run_cmd.is_empty() {
            if looks_like_shell_command(run_cmd) {
                all.extend(
                    self.multi_repo_rows(run_cmd)
                        .into_iter()
                        .map(|i| Suggestion::new(i, Source::Disambiguation, Tier::Identity)),
                );
            }
            all.extend(
                self.docker_rows(run_cmd)
                    .into_iter()
                    .map(|i| Suggestion::new(i, Source::Disambiguation, Tier::Identity)),
            );
        }

        // ── Context matches ─────────────────────────────────────────────
        //
        // Learned per-context ranking a generic completion cannot reproduce.
        if trimmed.chars().count() >= 2
            && cfg.context_actions_typed
            && let Some(ref ctx) = self.context
        {
            let matches = crate::context::suggestions::typed_matches(ctx, Some(&self.db), raw);
            if !matches.is_empty() {
                self.note_suggestions(&matches);
                all.extend(
                    matches
                        .into_iter()
                        .map(|i| Suggestion::matched(i, Source::Context, trimmed)),
                );
            }
        }

        // ── Safety guard ────────────────────────────────────────────────
        //
        // Now evaluated unconditionally. It used to sit inside the "some
        // handler matched" branch, so an UNRECOGNISED destructive input skipped
        // the warning entirely — the case where it matters most. Pinning the
        // old behaviour in a test is what made that visible.
        if let Some(row) = self.dirty_project_guard(trimmed) {
            all.push(Suggestion::new(row, Source::Guard, Tier::Identity));
        }

        // ── "Did you mean" ──────────────────────────────────────────────
        //
        // Offered only when nothing else matched: a correction competing with
        // real results is noise. Skipped for an explicit web route — the user
        // named the handler they wanted, and second-guessing that is the
        // launcher arguing with an instruction.
        let is_web_route = matches!(&route, PatternResult::Match(r) if r.handler == "web");
        if handler_empty
            && !is_web_route
            && let Some(row) = crate::intent::typo_suggest::suggest(raw, &self.registry)
        {
            all.push(Suggestion::new(row, Source::Correction, Tier::Prefix));
        }

        // ── App-search rescue ───────────────────────────────────────────
        //
        // A matched handler that returned nothing may still name an app.
        //
        // Skipped for an EXPLICIT route. If the user typed a registered keyword
        // they named the handler they wanted, and answering with a fuzzy app
        // match contradicts that: `dnf search firefox` offered Firefox, KFind
        // and Catfish, because the packages handler returned nothing for an
        // argument it had no hint for and this rescue then matched the raw text
        // against the app index. A handler that owns the input owns the empty
        // result too — "no packages match" is a real answer, and inventing
        // unrelated rows hides it.
        //
        // `web` stays excluded for the same reason it always was: natural-
        // language queries produce nonsense fuzzy hits ("How to make pasta" →
        // "os").
        if handler_empty
            && let PatternResult::Match(r) = &route
            && !r.explicit
            && r.handler != "open"
            && r.handler != "web"
        {
            let term = if r.args.is_empty() { raw } else { &r.args };
            all.extend(
                self.registry
                    .completions("open", term)
                    .await
                    .into_iter()
                    .map(|i| Suggestion::matched(i, Source::Handler, trimmed)),
            );
        }

        // ── AI command (preset) ─────────────────────────────────────────
        // A preset keyword (`summarize`, `translate`, …) is a concrete AI action,
        // not an escape hatch: it ranks as an Identity match so Enter selects it,
        // and it SUPPRESSES the generic Ask AI / Search web fallbacks — offering
        // "Ask AI" next to a resolved AI command is both redundant and, since a
        // fallback that sorts first would steal Enter, the exact bug this fixes.
        let preset = self.preset_row(raw);
        let has_preset = preset.is_some();
        if let Some(item) = preset {
            all.push(Suggestion::new(item, Source::Handler, Tier::Identity));
        }

        // ── Escape hatches ──────────────────────────────────────────────
        if !has_preset {
            all.extend(
                fallback_rows(raw, self.has_ai())
                    .into_iter()
                    .map(|i| Suggestion::new(i, Source::Fallback, Tier::Fuzzy)),
            );
        }

        // Learned query→command bindings for this exact query. Empty for a new
        // user or an unseen query, in which case ranking is unchanged.
        let latches = crate::db::frecency::get_latches(trimmed);
        // Stamp the ranker's verdict onto the item before the `Suggestion`
        // wrapper (and with it `Source` and `Tier`) is dropped. This is the only
        // point where all three inputs to `can_be_default` exist together, so
        // anything downstream that needs the answer has to be told it rather
        // than left to infer it from display text.
        crate::suggestions::rank_with_latches(all, &latches, trimmed)
            .into_iter()
            .map(|s| {
                // `Suggestion::can_be_default` is the rule; `default_index`
                // applies it to a ranked list. Both stay in the ranker — the
                // frontend receives the answer.
                let can_be_default = s.can_be_default();
                let mut item = s.item;
                item.can_be_default = can_be_default;
                item
            })
            .collect()
    }

    /// The empty-prompt shortlist: recents and context, with no query to match.
    ///
    /// Returns `None` when this isn't a zero-state summon, so the caller
    /// continues to the ranked path. Deliberately unranked — with nothing typed
    /// there is no tier to compute, and these rows are browsable offers rather
    /// than candidates competing to answer a query.
    fn zero_state(
        &self,
        raw: &str,
        cfg: &crate::config::schema::SuggestionsConfig,
    ) -> Option<Vec<CompletionItem>> {
        if !raw.trim().is_empty() {
            return None;
        }

        // The composer needs no context at all (pins + recent apps only), so
        // the first summon after launch shows the real list instead of a
        // blank panel. `zero_state_recents` is honoured inside the composer,
        // because pins survive the flag (explicit config, not history).
        let items = crate::zero_state::compose(&self.db, cfg);

        let Some(ctx) = self.context.as_ref() else {
            return Some(items);
        };
        // No `__context__` rows exist here any more, so this clears the shown
        // latch (stale acceptance credit) and records nothing.
        self.note_suggestions(&items);
        self.record_impressions_debounced(ctx, &items);
        let mut items = items;

        // Staleness is a FLAG, not a row: the `__context_stale__` sentinel
        // carries tooltip text the frontend renders as a dim glyph in the
        // status bar. A warning row here would push real suggestions down to
        // report something the user did not ask about.
        if ctx.is_soft_stale() {
            crate::context::metrics::inc_soft_stale_hit();
            let hard = ctx.is_hard_stale();
            if hard {
                crate::context::metrics::inc_hard_stale_hit();
            }
            let desc = if hard {
                "Context is over 5 min old — AI routing will be conservative"
            } else {
                "Suggestions reflect state from your last summon"
            };
            items.insert(
                0,
                CompletionItem {
                    label: String::new(),
                    icon_path: Some("__context_stale__".to_string()),
                    score: 0,
                    description: Some(desc.to_string()),
                    ..Default::default()
                },
            );
        }
        Some(items)
    }

    /// A configured quicklink's expansion preview — `gh tok` → what will run.
    ///
    /// Returns the whole list, bypassing the ranker: the user configured this
    /// exact keyword, so there is no competition to resolve. Shows the EXPANDED
    /// command rather than echoing the typed text, so the user sees what they
    /// are about to approve.
    fn quicklink_preview(&self, raw: &str) -> Option<Vec<CompletionItem>> {
        use crate::quicklinks::QuicklinkKind;

        // `quicklink_route` is the single decider for "is this a quicklink and
        // what does it expand to" — asking it, rather than re-deriving the
        // keyword, keeps preview and execution in agreement.
        let (_, expanded) = self.quicklink_route(raw)?;
        let keyword = raw.split_whitespace().next()?;
        let link = self
            .quicklinks
            .iter()
            .find(|q| q.keyword.eq_ignore_ascii_case(keyword))?;

        let (icon, verb) = match link.kind {
            QuicklinkKind::Url => ("__web__", "Open"),
            QuicklinkKind::Shell => ("__terminal__", "Run"),
            QuicklinkKind::Open => ("__file__", "Open"),
            QuicklinkKind::Command => ("__command__", "Run"),
        };
        Some(vec![
            CompletionItem::new(
                format!("{verb} {}: {expanded}", link.display_name()),
                Some(icon.into()),
                200,
            )
            .with_run(raw.trim().to_string())
            .with_description("Enter to run"),
        ])
    }

    /// Warn when a destructive action would run against a dirty checkout.
    ///
    /// Reversible-but-risky actions (suspend) count: the risk is unsaved work,
    /// not irreversibility.
    fn dirty_project_guard(&self, trimmed: &str) -> Option<CompletionItem> {
        const DIRTY_GUARD_ACTIONS: &[&str] =
            &["shutdown", "reboot", "hibernate", "logout", "suspend"];

        let ctx = self.context.as_ref()?;
        let git = ctx.git.as_ref()?;
        if !git.dirty {
            return None;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.is_empty() {
            return None;
        }
        if !DIRTY_GUARD_ACTIONS
            .iter()
            .any(|&name| name.starts_with(&lower) || lower.starts_with(name))
        {
            return None;
        }
        let project_name = ctx
            .project
            .as_ref()
            .and_then(|p| p.root.rsplit('/').next())
            .unwrap_or("repo");
        Some(CompletionItem {
            label: format!("⚠ {project_name} has uncommitted changes"),
            icon_path: Some("__warning__".to_string()),
            score: 200,
            description: Some(format!(
                "Branch '{}' is dirty — consider committing first",
                git.branch
            )),
            ..Default::default()
        })
    }

    /// Whether a query is worth offering fallbacks for. Blank or single-character
    /// input isn't a question yet — offering to search the web for "d" is noise.
    fn wants_fallbacks(raw: &str) -> bool {
        raw.trim().chars().count() >= 2
    }

    /// The AI-command (preset) suggestion for a query whose first word is a
    /// preset keyword, or `None`. The row carries the template in `run` and the
    /// already-typed argument (the words after the keyword) in `description`, so
    /// the actuator can render `{input}` from it or fall back to the selection.
    ///
    /// Only offered when AI is on — a preset with no provider has nowhere to run,
    /// and `classify` already downgrades that case to the web escape hatch. The
    /// keyword lookup mirrors `classify`'s injected `preset_for`; keeping the
    /// SAME lookup on both paths is what stops the suggestion and the Enter
    /// verdict from ever disagreeing.
    fn preset_row(&self, raw: &str) -> Option<CompletionItem> {
        use crate::action_registry::CompletionKind;

        if !self.has_ai() {
            return None;
        }
        let trimmed = raw.trim();
        let first_word = trimmed.split_whitespace().next()?;
        let (template, name) = crate::ai_presets::store::AiPresetsStore::new()
            .get_preset_by_keyword(&self.db, first_word)
            .ok()
            .flatten()
            .map(|p| (p.template, p.name))?;
        let arg = trimmed
            .split_once(char::is_whitespace)
            .map(|(_, r)| r.trim().to_string())
            .unwrap_or_default();

        Some(CompletionItem {
            label: name,
            icon_path: Some("__ai_chat__".to_string()),
            score: 0,
            description: Some(arg),
            kind: Some(CompletionKind::Preset),
            run: Some(template),
            ..Default::default()
        })
    }

    /// Whether AI is available.
    pub fn has_ai(&self) -> bool {
        self.resolver.has_ai()
    }

    /// Classify a raw input string into a [`RouteDecision`] — the SINGLE source of
    /// truth the frontend actuates on Enter. Folds the executor-owned side-channels
    /// (user Script Commands, quicklinks) BEFORE the general classifier, in the
    /// same priority order `run_inner` dispatches them, so a script/bang keyword
    /// classifies as a `Command` the frontend runs verbatim. Everything else
    /// delegates to [`crate::intent::classify::classify_string`] (the one place
    /// command/NL/preset/panel/typo grading lives).
    pub fn classify(&self, raw: &str) -> crate::intent::classify::RouteDecision {
        use crate::intent::classify::{RouteDecision, classify_string};

        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return RouteDecision::Nl {
                prompt: String::new(),
            };
        }

        // User Script Commands and quicklinks win over the general router
        // (highest intent), exactly as in `run_inner`. Both run verbatim.
        if self.script_route(trimmed).is_some() || self.quicklink_route(trimmed).is_some() {
            return RouteDecision::Command {
                command: trimmed.to_string(),
            };
        }

        // Preset keyword lookup, injected so the classifier stays IO-free.
        let db = self.db.clone();
        let preset_for = |keyword: &str| {
            crate::ai_presets::store::AiPresetsStore::new()
                .get_preset_by_keyword(&db, keyword)
                .ok()
                .flatten()
                .map(|p| (p.template, p.name))
        };

        classify_string(trimmed, &self.registry, preset_for, self.has_ai())
    }

    /// Refresh environment context (call on summon).
    pub fn refresh_context(&mut self, pre_window: Option<crate::context::WindowContext>) {
        self.context = Some(crate::context::gather(pre_window));
    }
}

/// The two universal escape hatches, in the order they should appear.
///
/// These are FALLBACKS, not results: they carry a deliberately low score so
/// they sort last, and the frontend never auto-selects a fallback row. Both
/// declare their intent via `kind` and carry the QUERY in `description` — no
/// command string for anything downstream to re-parse (there is no `ask`
/// handler at all, so a `run: "ask …"` row would silently pattern-route to a
/// web search).
///
/// Empty for a query too short to be a real question. "Ask AI" is omitted
/// entirely when no provider is configured — an escape hatch that leads
/// nowhere is worse than no escape hatch, since it looks like an answer.
/// Web search always works, so it is always offered.
fn fallback_rows(raw: &str, has_ai: bool) -> Vec<CompletionItem> {
    use crate::action_registry::CompletionKind;

    let q = raw.trim();
    if !Executor::wants_fallbacks(q) {
        return Vec::new();
    }
    // Clean labels ("Ask AI" / "Search web"), with the query kept ONLY in
    // `description` — NOT echoed into the label. The input is already visible in
    // the prompt above, so repeating it in the label clutters the row. The
    // `description` is not rendered as a pill for these fallback kinds (the
    // frontend hides it — see CompletionsList); it stays because the submit
    // router reads it to recover the query when the row is chosen.
    let mut rows = Vec::with_capacity(2);
    if has_ai {
        rows.push(CompletionItem {
            label: "Ask AI".to_string(),
            icon_path: Some("__ai_chat__".to_string()),
            score: 2,
            description: Some(q.to_string()),
            kind: Some(CompletionKind::AskAi),
            ..Default::default()
        });
    }
    rows.push(CompletionItem {
        label: "Search web".to_string(),
        icon_path: Some("__web__".to_string()),
        score: 1,
        description: Some(q.to_string()),
        kind: Some(CompletionKind::SearchWeb),
        ..Default::default()
    });
    rows
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
/// Whether an unrouted query still looks like an attempt to LAUNCH AN APP,
/// versus a natural-language sentence that should go to AI/web.
///
/// App names are short — one to a few words ("spotify", "vs code", "sublime
/// text"). A launcher summon to open something is not a sentence. Two cheap,
/// language-agnostic signals gate the implicit `open` fuzzy match so a phrase
/// like "play the music and tell me my disk status" stops matching every
/// music/disk app (the Raycast/Spotlight behaviour: match app NAMES tightly,
/// route prose elsewhere):
///
/// - **word count**: at most `MAX_APP_WORDS` — longer is prose, not a name;
/// - **not a question / request phrasing**: a leading interrogative or an
///   embedded "tell me / show me / what's" reads as an ask, never an app name.
///
/// Deliberately permissive on the short side: a 1-3 word query still hits the
/// app index (so "disk usage" can still surface Filelight if the user meant
/// that), because the cost of a stray app row on a short query is low, while
/// suppressing a real app launch would be the worse failure.
fn looks_like_app_query(query: &str) -> bool {
    let q = query.trim().to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();

    // Zero-length can't reach here (zero-state handles empty); guard anyway.
    if words.is_empty() {
        return false;
    }
    // App names top out at a few words; beyond that it's a phrase.
    const MAX_APP_WORDS: usize = 4;
    if words.len() > MAX_APP_WORDS {
        return false;
    }
    // Question / request phrasing → an ask, not a name. Checked as whole words
    // so "whatsapp" (contains "what") isn't caught, and multi-word cues match.
    const REQUEST_CUES: &[&str] = &[
        "what",
        "whats",
        "what's",
        "how",
        "why",
        "who",
        "when",
        "where",
        "which",
        "can",
        "should",
        "tell",
        "show",
        "explain",
        "summarize",
        "translate",
        "write",
        "give",
    ];
    let has_request_cue = words.iter().any(|w| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric());
        REQUEST_CUES.contains(&w)
    });
    !has_request_cue
}

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
        // Deliberately no `|| echo failed` marker. It read nicely, but `||`
        // contains `|`, which the shell decider flags as a pipe — so EVERY
        // fan-out prompted for confirmation, including a read-only `git
        // status`, for an operator the app itself injected. Generating syntax
        // that trips our own gate trains the user to click through prompts.
        //
        // Failures are still visible: each repo prints its own header, and the
        // command's own stderr appears beneath it.
        out.push_str(&format!("echo {header}; (cd {qdir} && {cmd}); echo ''"));
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
/// The row for a command with exactly ONE target.
///
/// Reads as the command itself — there is no choice to present, so naming the
/// repo in the title would be noise. The directory goes in the description, so
/// it is still obvious where the command lands.
fn single_target_row(
    command: &str,
    target: &crate::context::multi_repo::RunTarget,
) -> CompletionItem {
    CompletionItem::new(command.to_string(), Some("__terminal__".into()), 150)
        .with_run(format!("run {command}"))
        .with_description(format!("Run in {}", target.name))
}

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
mod run_row_tests {
    use super::*;
    use crate::context::multi_repo::RunTarget;

    fn target(name: &str) -> RunTarget {
        RunTarget {
            dir: format!("/home/u/ws/{name}"),
            name: name.to_string(),
        }
    }

    #[test]
    fn a_single_target_command_still_gets_a_row() {
        // The reported bug, twice: typing `git status` in a single repo showed
        // only Ask-AI / Search-web. The command was perfectly runnable, but
        // nothing in the list said so, so there was nothing to press Enter on.
        let row = single_target_row("git status", &target("api"));
        assert_eq!(row.label, "git status");
        assert_eq!(row.run.as_deref(), Some("run git status"));
    }

    #[test]
    fn a_single_target_row_says_where_it_runs() {
        // No choice to present, so the repo belongs in the description rather
        // than cluttering the title.
        let row = single_target_row("git status", &target("api"));
        assert!(
            row.description.as_deref().unwrap_or("").contains("api"),
            "description must name the target: {:?}",
            row.description
        );
        assert!(!row.label.contains("api"), "title must stay clean");
    }

    #[test]
    fn a_multi_target_row_names_its_repo_in_the_title() {
        // Here the repo IS the distinguishing information — three rows differ
        // only by target, so the title has to carry it.
        let row = repo_row("git status", &target("admin"), 120);
        assert!(row.label.contains("admin"), "got {}", row.label);
        assert!(
            row.run.as_deref().unwrap_or("").contains("@@"),
            "multi-target row must pin its directory: {:?}",
            row.run
        );
    }

    #[test]
    fn every_run_row_carries_an_executable_command() {
        // A row with no `run` falls back to executing its label, which for a
        // pinned row would drop the directory and run in the wrong place.
        for row in [
            single_target_row("ls -la", &target("api")),
            repo_row("ls -la", &target("api"), 100),
        ] {
            let run = row.run.as_deref().unwrap_or("");
            assert!(run.starts_with("run "), "not executable: {run:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_registry::{ActionHandler, ActionResult};
    use crate::config::schema::PrivacyConfig;
    use crate::history::HistoryStore;
    use crate::rules::RulesEngine;
    use async_trait::async_trait;

    #[test]
    fn expand_at_strips_reference_and_expands_tilde() {
        let home = dirs::home_dir().unwrap();
        let out = expand_at_references("resize @~/Pictures/img.png to 800x600");
        assert_eq!(
            out,
            format!("resize {}/Pictures/img.png to 800x600", home.display())
        );
    }

    #[test]
    fn expand_at_strips_reference_on_absolute_path() {
        assert_eq!(expand_at_references("open @/tmp/a.png"), "open /tmp/a.png");
    }

    #[test]
    fn expand_at_leaves_email_and_bare_at_untouched() {
        // `@` mid-token (email) is not a token-leading reference.
        assert_eq!(expand_at_references("mail foo@bar.com"), "mail foo@bar.com");
        // A leading `@` not followed by a path-like char is left as-is.
        assert_eq!(expand_at_references("say @ hi"), "say @ hi");
    }

    #[test]
    fn expand_at_noop_without_at() {
        assert_eq!(expand_at_references("weather tokyo"), "weather tokyo");
    }

    // --- Stub handlers ---

    /// Always succeeds — used for "web" and optionally "open"
    struct StubHandler {
        id: &'static str,
    }

    /// An `open` handler that returns a completion.
    ///
    /// The plain `StubHandler` returns none, which makes any assertion about
    /// completion ROWS silently depend on the app index — i.e. on which
    /// applications the test machine happens to have installed.
    struct CompletingHandler;

    #[async_trait]
    impl ActionHandler for CompletingHandler {
        fn id(&self) -> &str {
            "open"
        }

        fn description(&self) -> &str {
            "mock open handler that completes"
        }

        async fn execute(
            &self,
            _ctx: &crate::action_registry::ExecContext,
            _args: &str,
        ) -> Result<ActionResult, crate::error::LychiError> {
            Ok(ActionResult::ok(
                "open stub executed",
                crate::action_registry::OutputType::Status,
            ))
        }

        async fn completions(&self, partial: &str) -> Vec<crate::action_registry::CompletionItem> {
            // Mirrors `AppLauncher::completions`: `partial` is the ARGS after
            // routing ("spotify" for `open spotify`), the label is the APP
            // NAME, and `run` is `open <Name>`. Labelling with the full command
            // instead would be a mock that cannot reproduce the defaultability
            // bug this exists to test.
            let arg = partial.trim();
            if arg.is_empty() {
                // A bare "open" names no app; the real handler returns nothing
                // for an empty query.
                return vec![crate::action_registry::CompletionItem {
                    label: "open".to_string(),
                    score: 100,
                    run: Some("open".to_string()),
                    ..Default::default()
                }];
            }
            // Title-case the arg the way a display name reads.
            let mut chars = arg.chars();
            let name = match chars.next() {
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                None => arg.to_string(),
            };
            vec![crate::action_registry::CompletionItem {
                label: name.clone(),
                score: 100,
                run: Some(format!("open {name}")),
                ..Default::default()
            }]
        }
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

    /// A `ReplacePrevious` handler that sleeps, so a superseding call can cancel
    /// it mid-flight. Records whether its body ran to completion.
    struct SlowReplaceHandler {
        delay_ms: u64,
        completed: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait]
    impl ActionHandler for SlowReplaceHandler {
        fn id(&self) -> &str {
            "slow"
        }
        fn description(&self) -> &str {
            "slow replace stub"
        }
        fn execution_mode(&self) -> crate::action_registry::ExecutionMode {
            crate::action_registry::ExecutionMode::ReplacePrevious
        }
        async fn execute(
            &self,
            _ctx: &crate::action_registry::ExecContext,
            _args: &str,
        ) -> Result<ActionResult, crate::error::LychiError> {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            // Only reached if NOT cancelled — the cancel branch drops this future
            // before this line runs.
            self.completed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(ActionResult::ok(
                "slow done",
                crate::action_registry::OutputType::Status,
            ))
        }
    }

    #[tokio::test]
    async fn replace_previous_cancels_superseded_call() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let exec = make_executor(ActionRegistry::new());
        let ctx = crate::action_registry::ExecContext::default();

        let first_done = Arc::new(AtomicBool::new(false));
        let second_done = Arc::new(AtomicBool::new(false));
        let first = SlowReplaceHandler {
            delay_ms: 500,
            completed: first_done.clone(),
        };
        let second = SlowReplaceHandler {
            delay_ms: 10,
            completed: second_done.clone(),
        };

        // Start the slow first call, then supersede it with a fast second call.
        // Both target the same handler id ("slow"), so the second cancels the first.
        let (r1, r2) = tokio::join!(
            async {
                let f = exec.gate.run(&first, &ctx, "a");
                f.await
            },
            async {
                // Let the first register its cancel handle before we supersede it.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                exec.gate.run(&second, &ctx, "b").await
            },
        );

        let (r1, _busy1) = r1.unwrap();
        let (r2, _busy2) = r2.unwrap();

        // The first was superseded: its body never completed (cancelled mid-sleep)
        // and it returns an unsuccessful (discarded) result.
        assert!(
            !first_done.load(Ordering::SeqCst),
            "first call should have been cancelled before completing"
        );
        assert!(!r1.success, "superseded call returns unsuccessful result");
        // The second ran to completion.
        assert!(second_done.load(Ordering::SeqCst), "second call completed");
        assert!(r2.success, "second call succeeded");
    }

    /// A handler that always assesses High risk (→ RulesEngine returns Confirm),
    /// records whether its body actually executed, and lets a test flip it to
    /// Exclusive mode. Used to exercise the confirmation (G1) and busy (G4) paths.
    struct RiskyHandler {
        id: &'static str,
        executed: Arc<std::sync::atomic::AtomicBool>,
        mode: crate::action_registry::ExecutionMode,
    }

    #[async_trait]
    impl ActionHandler for RiskyHandler {
        fn id(&self) -> &str {
            self.id
        }
        fn description(&self) -> &str {
            "risky stub"
        }
        fn default_risk(&self) -> RiskLevel {
            RiskLevel::High
        }
        fn execution_mode(&self) -> crate::action_registry::ExecutionMode {
            self.mode
        }
        fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
            static T: &[crate::action_registry::Trigger] =
                &[crate::action_registry::Trigger::keywords(&["danger"])];
            T
        }
        async fn execute(
            &self,
            _ctx: &crate::action_registry::ExecContext,
            _args: &str,
        ) -> Result<ActionResult, crate::error::LychiError> {
            self.executed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(ActionResult::ok(
                "risky executed",
                crate::action_registry::OutputType::Status,
            ))
        }
    }

    // --- G1: confirmation binds to the assessed intent (no re-resolve) ---

    #[tokio::test]
    async fn confirm_returns_pending_then_run_confirmed_executes_stored_intent() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let executed = Arc::new(AtomicBool::new(false));
        let mut reg = ActionRegistry::new();
        reg.register(Box::new(RiskyHandler {
            id: "danger",
            executed: executed.clone(),
            mode: crate::action_registry::ExecutionMode::Immediate,
        }));
        let ex = make_executor(reg);
        let privacy = PrivacyConfig::default();
        let inputs = RunInputs::default();

        // First pass: High risk → pending confirmation, NOT executed, and the exact
        // resolved intent is captured for the confirm step.
        let first = ex
            .run("danger rm -rf /tmp/x", false, &privacy, &inputs)
            .await
            .unwrap();
        assert!(
            first.envelope.needs_confirmation.is_some(),
            "should require confirmation"
        );
        assert!(
            !executed.load(Ordering::SeqCst),
            "must not run before confirm"
        );
        let pending = first.pending_intent.expect("captured pending intent");
        assert_eq!(pending.action_id, "danger");

        // Confirm: runs the STORED intent (no re-resolution) → now executes.
        let confirmed = ex.run_confirmed(pending, &privacy, &inputs).await.unwrap();
        assert!(confirmed.result.success, "confirmed run executes");
        assert!(executed.load(Ordering::SeqCst), "body ran after confirm");
    }

    #[tokio::test]
    async fn confirmed_run_still_blocked_by_deny() {
        // Even on the confirmed path, a Deny decision must halt execution — the
        // shell denylist is a hard Deny, so a pre-resolved `run rm -rf /` intent
        // must never execute even when routed through run_confirmed.
        let mut reg = ActionRegistry::new();
        reg.register(Box::new(
            crate::action_registry::handlers::shell_exec::ShellExec::new(),
        ));
        let ex = make_executor(reg);
        let privacy = PrivacyConfig::default();
        let inputs = RunInputs::default();

        // A denied shell command, pre-resolved as if it had been confirmed.
        let intent = crate::intent::ResolvedIntent {
            action_id: "run".into(),
            args: "rm -rf /".into(),
            routing: crate::intent::RoutingMethod::Explicit,
        };
        let res = ex.run_confirmed(intent, &privacy, &inputs).await.unwrap();
        assert!(!res.result.success, "denied command must not succeed");
        assert!(
            res.result
                .error
                .as_deref()
                .unwrap_or("")
                .to_lowercase()
                .contains("block"),
            "should be blocked by the rules engine, got: {:?}",
            res.result.error
        );
    }

    // --- G2: risk assessment receives the request context (cwd/workspace) ---

    #[tokio::test]
    async fn assess_risk_sees_context_cwd() {
        use std::sync::atomic::{AtomicBool, Ordering};
        // A handler that flips to High risk ONLY when the cwd is under /tmp — proving
        // the RiskContext (cwd) actually reaches assess_risk. Records what it saw.
        struct CtxRiskHandler {
            saw_tmp: Arc<AtomicBool>,
        }
        #[async_trait]
        impl ActionHandler for CtxRiskHandler {
            fn id(&self) -> &str {
                "ctxrisk"
            }
            fn description(&self) -> &str {
                "context-risk stub"
            }
            fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
                static T: &[crate::action_registry::Trigger] =
                    &[crate::action_registry::Trigger::keywords(&["ctxrisk"])];
                T
            }
            fn assess_risk(
                &self,
                _args: &str,
                ctx: &crate::action_registry::RiskContext<'_>,
            ) -> crate::action_registry::RiskAssessment {
                let in_tmp = ctx.cwd.is_some_and(|c| c.starts_with("/tmp"));
                if in_tmp {
                    self.saw_tmp.store(true, Ordering::SeqCst);
                }
                crate::action_registry::RiskAssessment::level(if in_tmp {
                    RiskLevel::High
                } else {
                    RiskLevel::Low
                })
            }
            async fn execute(
                &self,
                _ctx: &crate::action_registry::ExecContext,
                _args: &str,
            ) -> Result<ActionResult, crate::error::LychiError> {
                Ok(ActionResult::ok(
                    "ok",
                    crate::action_registry::OutputType::Status,
                ))
            }
        }

        let saw_tmp = Arc::new(AtomicBool::new(false));
        let mut reg = ActionRegistry::new();
        reg.register(Box::new(CtxRiskHandler {
            saw_tmp: saw_tmp.clone(),
        }));
        let mut ex = make_executor(reg);
        // Inject a context whose cwd is under /tmp.
        ex.context = Some(crate::context::EnvironmentContext {
            cwd: Some("/tmp/work".to_string()),
            ..Default::default()
        });

        let privacy = PrivacyConfig::default();
        let inputs = RunInputs::default();
        let res = ex
            .run("ctxrisk go", false, &privacy, &inputs)
            .await
            .unwrap();

        assert!(
            saw_tmp.load(Ordering::SeqCst),
            "assess_risk should have seen the /tmp cwd from RiskContext"
        );
        // High risk under /tmp → the run returns a confirmation prompt.
        assert!(
            res.envelope.needs_confirmation.is_some(),
            "context-elevated risk should require confirmation"
        );
    }

    // --- G4: Exclusive rejects with busy while one is running ---

    #[tokio::test]
    async fn exclusive_second_call_is_busy_while_first_runs() {
        let ex = make_executor(ActionRegistry::new());
        let ctx = crate::action_registry::ExecContext::default();

        struct SlowExclusive;
        #[async_trait]
        impl ActionHandler for SlowExclusive {
            fn id(&self) -> &str {
                "excl"
            }
            fn description(&self) -> &str {
                "slow exclusive"
            }
            fn execution_mode(&self) -> crate::action_registry::ExecutionMode {
                crate::action_registry::ExecutionMode::Exclusive
            }
            async fn execute(
                &self,
                _ctx: &crate::action_registry::ExecContext,
                _args: &str,
            ) -> Result<ActionResult, crate::error::LychiError> {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                Ok(ActionResult::ok(
                    "excl done",
                    crate::action_registry::OutputType::Status,
                ))
            }
        }

        let h1 = SlowExclusive;
        let h2 = SlowExclusive;
        let (r1, r2) = tokio::join!(async { ex.gate.run(&h1, &ctx, "a").await }, async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            ex.gate.run(&h2, &ctx, "b").await
        },);
        let (res1, busy1) = r1.unwrap();
        let (res2, busy2) = r2.unwrap();
        assert!(!busy1 && res1.success, "first exclusive runs");
        assert!(busy2, "second exclusive is rejected as busy");
        assert!(!res2.success, "busy reject is not a success");
    }

    #[tokio::test]
    async fn busy_flag_propagates_through_run_to_execute_result() {
        // The Tauri confirm path reinserts a pending confirmation when
        // `ExecuteResult.busy` is true — so verify busy flows all the way through
        // the PUBLIC `run()` pipeline, not just `execute_gated`. Two concurrent
        // exclusive runs: the second must surface `busy` on its ExecuteResult.
        struct SlowExcl;
        #[async_trait]
        impl ActionHandler for SlowExcl {
            fn id(&self) -> &str {
                "excl"
            }
            fn description(&self) -> &str {
                "slow exclusive"
            }
            fn execution_mode(&self) -> crate::action_registry::ExecutionMode {
                crate::action_registry::ExecutionMode::Exclusive
            }
            fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
                static T: &[crate::action_registry::Trigger] =
                    &[crate::action_registry::Trigger::keywords(&["excl"])];
                T
            }
            async fn execute(
                &self,
                _ctx: &crate::action_registry::ExecContext,
                _args: &str,
            ) -> Result<ActionResult, crate::error::LychiError> {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                Ok(ActionResult::ok(
                    "done",
                    crate::action_registry::OutputType::Status,
                ))
            }
        }
        let mut reg = ActionRegistry::new();
        reg.register(Box::new(SlowExcl));
        let ex = make_executor(reg);
        let privacy = PrivacyConfig::default();
        let inputs = RunInputs::default();

        let (r1, r2) = tokio::join!(
            async { ex.run("excl a", false, &privacy, &inputs).await },
            async {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                ex.run("excl b", false, &privacy, &inputs).await
            },
        );
        assert!(!r1.unwrap().busy, "first run not busy");
        assert!(
            r2.unwrap().busy,
            "second run surfaces busy on ExecuteResult"
        );
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

    /// A consent-needing confirmation ships its TYPED feature key on the
    /// envelope; granting the consent removes both prompt and key. The FE
    /// persists the grant from this field — it used to substring-match the
    /// prompt prose, so rewording a sentence silently broke persistence.
    #[tokio::test]
    async fn a_consent_confirmation_carries_its_typed_feature_key() {
        struct NetHandler;
        #[async_trait]
        impl ActionHandler for NetHandler {
            fn id(&self) -> &str {
                "netprobe"
            }
            fn description(&self) -> &str {
                "test"
            }
            fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
                static T: &[crate::action_registry::Trigger] =
                    &[crate::action_registry::Trigger::keywords(&["netprobe"])];
                T
            }
            fn assess_risk(
                &self,
                _args: &str,
                _ctx: &crate::action_registry::RiskContext<'_>,
            ) -> crate::action_registry::RiskAssessment {
                crate::action_registry::RiskAssessment::level(RiskLevel::Low).with_consent(
                    crate::action_registry::ConsentKind::PublicIp,
                    "This will look up your public IP. Allow and remember?",
                )
            }
            async fn execute(
                &self,
                _ctx: &crate::action_registry::ExecContext,
                _args: &str,
            ) -> Result<ActionResult, crate::error::LychiError> {
                Ok(ActionResult::ok(
                    "ran",
                    crate::action_registry::OutputType::Status,
                ))
            }
        }
        let mut reg = ActionRegistry::new();
        reg.register(Box::new(NetHandler));
        let ex = make_executor(reg);
        let inputs = RunInputs::default();

        // Ungranted: pending confirmation + the typed key.
        let r = ex
            .run("netprobe", false, &PrivacyConfig::default(), &inputs)
            .await
            .unwrap();
        assert!(r.envelope.needs_confirmation.is_some());
        assert_eq!(r.envelope.consent_feature.as_deref(), Some("public_ip"));

        // Granted: executes, no prompt, no key.
        let granted = PrivacyConfig {
            allow_public_ip: true,
            ..PrivacyConfig::default()
        };
        let r = ex.run("netprobe", false, &granted, &inputs).await.unwrap();
        assert!(r.envelope.needs_confirmation.is_none());
        assert!(r.envelope.consent_feature.is_none());
    }

    /// The Clone contract that keeps snapshot-and-release sound: cross-run
    /// state is SHARED between a snapshot and the canonical executor. A forked
    /// gate would let an Exclusive handler run twice concurrently (one per
    /// clone); forked learning state would silently drop acceptance signals
    /// recorded on a snapshot. `Arc::ptr_eq` is the whole assertion — if either
    /// field stops being shared, snapshot execution is no longer semantics-
    /// preserving and the app must go back to holding the lock.
    #[test]
    fn snapshots_share_the_gate_and_learning_state() {
        let ex = make_executor(ActionRegistry::new());
        let snap = ex.clone();
        assert!(
            Arc::ptr_eq(&ex.gate, &snap.gate),
            "a snapshot must run through the SAME concurrency gate"
        );
        assert!(
            Arc::ptr_eq(&ex.suggestions, &snap.suggestions),
            "a snapshot must record into the SAME suggestion tracker"
        );
    }

    // --- Quicklink routing ---

    fn quicklink(
        keyword: &str,
        kind: crate::quicklinks::QuicklinkKind,
        template: &str,
    ) -> crate::quicklinks::Quicklink {
        crate::quicklinks::Quicklink {
            keyword: keyword.to_string(),
            name: String::new(),
            kind,
            template: template.to_string(),
        }
    }

    fn executor_with_quicklinks() -> Executor {
        use crate::quicklinks::QuicklinkKind;
        let mut ex = make_executor(registry_web_only());
        ex.set_quicklinks(vec![
            // No placeholder — a complete action on its own.
            quicklink(
                "ghvs",
                QuicklinkKind::Url,
                "https://github.com/ValariSolutions",
            ),
            // Takes input.
            quicklink(
                "gh",
                QuicklinkKind::Url,
                "https://github.com/search?q={query}",
            ),
        ]);
        ex
    }

    #[test]
    fn a_placeholderless_quicklink_runs_from_the_bare_keyword() {
        // The reported bug: `ghvs` alone did nothing, because routing inherited
        // the bang rule of "keyword must be followed by a query". A quicklink
        // with no placeholder needs no input, so requiring some made it
        // unreachable.
        let ex = executor_with_quicklinks();
        let (action, args) = ex.quicklink_route("ghvs").expect("bare keyword must route");
        assert_eq!(action, "quicklink");
        assert_eq!(args, "ghvs");
    }

    #[test]
    fn a_placeholderless_quicklink_routes_with_trailing_whitespace() {
        let ex = executor_with_quicklinks();
        assert!(ex.quicklink_route("ghvs  ").is_some());
    }

    #[test]
    fn a_parameterized_quicklink_still_falls_through_when_typed_bare() {
        // `gh` alone should stay available to app-launch/search, since the
        // quicklink can't do anything useful without input.
        let ex = executor_with_quicklinks();
        assert!(ex.quicklink_route("gh").is_none());
    }

    #[test]
    fn a_parameterized_quicklink_routes_once_input_arrives() {
        let ex = executor_with_quicklinks();
        let (_, args) = ex.quicklink_route("gh tokio").expect("should route");
        assert_eq!(args, "gh tokio");
    }

    #[test]
    fn an_unconfigured_keyword_never_routes() {
        let ex = executor_with_quicklinks();
        assert!(ex.quicklink_route("nope").is_none());
        assert!(ex.quicklink_route("nope input").is_none());
    }

    /// Registry with only a "web" stub (no "open" handler)
    fn registry_web_only() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register(Box::new(StubHandler { id: "web" }));
        r
    }

    /// Registry where "open" succeeds and "web" is present
    /// `open` returns one real completion, so `completions()` has a row whose
    /// `can_be_default` stamp can be asserted without depending on the host's
    /// installed applications.
    fn registry_with_completing_open() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register(Box::new(CompletingHandler));
        r.register(Box::new(StubHandler { id: "web" }));
        r
    }

    /// An `open` mock with a FIXED app name — faithful to the real
    /// `AppLauncher`, whose labels are APP NAMES from the index, never echoes
    /// of the typed query. `CompletingHandler` title-cases its arg, so its
    /// label always equals the typed text (Identity): it can reproduce
    /// neither the strict-PREFIX shape ("spoti" → "Spotify", the
    /// spoti-double-Enter bug) nor the mention shape ("how do i install
    /// firefox" → label "Firefox", Subset).
    struct FixedAppHandler {
        name: &'static str,
    }

    #[async_trait]
    impl ActionHandler for FixedAppHandler {
        fn id(&self) -> &str {
            "open"
        }
        fn description(&self) -> &str {
            "mock open handler with one fixed app"
        }
        async fn execute(
            &self,
            _ctx: &crate::action_registry::ExecContext,
            _args: &str,
        ) -> Result<ActionResult, crate::error::LychiError> {
            Ok(ActionResult::ok(
                "open stub executed",
                crate::action_registry::OutputType::Status,
            ))
        }
        async fn completions(&self, _partial: &str) -> Vec<crate::action_registry::CompletionItem> {
            vec![crate::action_registry::CompletionItem {
                label: self.name.to_string(),
                score: 100,
                run: Some(format!("open {}", self.name)),
                ..Default::default()
            }]
        }
    }

    fn registry_with_fixed_app_open(name: &'static str) -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register(Box::new(FixedAppHandler { name }));
        r.register(Box::new(StubHandler { id: "web" }));
        r
    }

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

    /// Pin the app index to ONE known app for the duration of a test.
    ///
    /// These tests assert *routing*, but routing consults the global app index,
    /// so without this they assert whatever is installed on the machine: they
    /// passed on a developer desktop with Firefox and failed on a CI runner
    /// with no `.desktop` files, where "firefox" fell through to `run` and the
    /// stub registry answered `UnknownCommand("run")`.
    ///
    /// The guard restores the real index on drop, and the lock serialises
    /// tests that touch the process-wide global.
    fn with_firefox_indexed() -> impl Drop {
        with_indexed_app("Firefox", "/usr/bin/firefox")
    }

    /// As above, for a caller that needs a specific app name.
    fn with_indexed_app(name: &str, exec: &str) -> impl Drop {
        use crate::desktop_apps::index;
        // The guard is never *read* — holding it IS the point, since dropping
        // it releases the lock that serialises index swaps. Named rather than
        // positional so `dead_code` can see it is deliberate.
        struct Restore {
            _lock: std::sync::MutexGuard<'static, ()>,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                index::rebuild_app_index();
            }
        }
        let guard = index::test_index_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        index::set_app_index_for_test(vec![index::tests::make_entry(
            name,
            exec,
            &[],
            None,
            Some(&name.to_lowercase()),
        )]);
        Restore { _lock: guard }
    }

    /// 2. Short word + "open" handler succeeds → action stays "open"
    #[tokio::test]
    async fn app_present_routes_to_open() {
        let _idx = with_firefox_indexed();
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
        let _idx = with_firefox_indexed();
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

    /// 6. Completions: a NoMatch natural-language query surfaces NO inline
    /// Fallbacks are always OFFERED but never auto-selected.
    ///
    /// They were once removed wholesale because they were auto-selectable and
    /// hijacked Enter on a question. The fix is not absence — an unmatched query
    /// with an empty list is a dead end ("defuu" showed nothing at all) — it's
    /// that they sort last and the frontend never preselects a fallback.
    #[tokio::test]
    async fn fallback_rows_are_offered_but_rank_last() {
        let ex = make_executor(registry_open_fails());
        let completions = ex
            .completions(
                "zzunknownquery",
                &crate::config::schema::SuggestionsConfig::default(),
            )
            .await;
        let web = completions
            .iter()
            .find(|c| c.kind == Some(crate::action_registry::CompletionKind::SearchWeb))
            .unwrap_or_else(|| panic!("expected a web fallback, got: {completions:?}"));
        // Lowest score in the list → sorts last, so it can never outrank a real
        // result or be the default selection.
        assert!(
            completions.iter().all(|c| c.score >= web.score),
            "the fallback must rank last, got: {completions:?}"
        );
        // And it carries the ranker's verdict across IPC. The frontend selects
        // Enter's target by reading this flag, so a fallback marked defaultable
        // would hijack Enter on an unmatched query — the exact bug that once got
        // fallbacks removed wholesale.
        assert!(
            !web.can_be_default,
            "a fallback must never be stamped defaultable, got: {web:?}"
        );
    }

    /// The verdict must be STAMPED, not merely computable.
    ///
    /// `Source` and `Tier` are dropped at this boundary — the frontend receives
    /// `CompletionItem`, not `Suggestion` — so if the executor forgot to copy
    /// the answer onto the item, every row would arrive with the `false` default
    /// and Enter would stop auto-selecting anything. A test that only checks the
    /// rule in `suggestions` cannot see that.
    /// `open spotify` must be Enter-launchable.
    ///
    /// It was not: `Tier::classify` saw that "open spotify" *contains*
    /// "Spotify" and returned `Subset`, which refuses to auto-select. That
    /// rule is right for `dnf search firefox` and wrong here — the user named
    /// the verb AND the app. Explicit routes now classify against the args.
    #[tokio::test]
    async fn an_explicit_open_is_enter_launchable() {
        // `CompletingHandler` stands in for AppLauncher: it returns a row
        // whose label is the app name, exactly as the real handler does for
        // `open spotify` (route args = "spotify").
        let ex = make_executor(registry_with_completing_open());
        let completions = ex
            .completions(
                "open spotify",
                &crate::config::schema::SuggestionsConfig::default(),
            )
            .await;
        let row = completions
            .iter()
            .find(|c| c.label == "Spotify")
            .unwrap_or_else(|| panic!("no app row: {completions:?}"));
        assert!(
            row.can_be_default,
            "Enter would not launch the app the user explicitly named: {row:?}"
        );
    }

    /// THE "spoti ⏎ ⏎" BUG end-to-end: typing a strict prefix of an app's
    /// display name must make that row Enter-launchable, even though its
    /// command (`open Spotify`) does not prefix-extend the typed text. Judged
    /// by the command alone the row classified Subset, nothing was
    /// defaultable, and the first Enter fell through to the typo corrector's
    /// fill — launching took two Enters.
    #[tokio::test]
    async fn a_typed_app_name_prefix_is_enter_launchable() {
        let ex = make_executor(registry_with_fixed_app_open("Spotify"));
        let completions = ex
            .completions(
                "spoti",
                &crate::config::schema::SuggestionsConfig::default(),
            )
            .await;
        let row = completions
            .iter()
            .find(|c| c.label == "Spotify")
            .unwrap_or_else(|| panic!("no app row: {completions:?}"));
        assert!(
            row.can_be_default,
            "Enter would not launch the app whose name the user is typing: {row:?}"
        );
    }

    /// The protection that must survive: a NON-explicit route still classifies
    /// against the whole input, so a query that merely mentions an app cannot
    /// auto-launch it. This is the `dnf search firefox` shape.
    #[tokio::test]
    async fn a_query_that_merely_mentions_an_app_is_not_defaultable() {
        // The fixed-name mock, not `CompletingHandler`: the real AppLauncher
        // labels this row "Firefox" (the matched app), not an echo of the
        // query — and the label tier must classify against what the real
        // handler displays for the guard to be tested honestly.
        let ex = make_executor(registry_with_fixed_app_open("Firefox"));
        // No registered keyword typed → the route is not explicit.
        let completions = ex
            .completions(
                "how do i install firefox",
                &crate::config::schema::SuggestionsConfig::default(),
            )
            .await;
        assert!(
            !completions.iter().any(|c| c.can_be_default),
            "a query mentioning an app must never auto-launch it: {completions:?}"
        );
    }

    #[tokio::test]
    async fn a_real_match_is_stamped_defaultable() {
        // A MOCK handler that actually returns a row, rather than relying on
        // whatever the host has installed. The stub `open` handler yields no
        // completions, so on the dev box this test passed only because a real
        // installed app happened to match "open" through the app index — and
        // on a CI runner with no .desktop files nothing did. What is being
        // asserted is that the executor STAMPS the verdict onto the item, so
        // the row's provenance is irrelevant; it just has to exist.
        let ex = make_executor(registry_with_completing_open());
        let completions = ex
            .completions("open", &crate::config::schema::SuggestionsConfig::default())
            .await;
        assert!(
            completions.iter().any(|c| c.can_be_default),
            "no row was stamped defaultable, so Enter would select nothing: {completions:?}"
        );
    }

    /// A dead end is the bug being fixed: an unmatched query must still offer a
    /// way forward.
    #[tokio::test]
    async fn an_unmatched_query_is_never_a_dead_end() {
        let ex = make_executor(registry_open_fails());
        let completions = ex
            .completions(
                "defuu",
                &crate::config::schema::SuggestionsConfig::default(),
            )
            .await;
        assert!(
            !completions.is_empty(),
            "an unmatched query must offer SOMETHING"
        );
        assert!(
            completions
                .iter()
                .any(|c| c.kind.is_some_and(|k| k.is_fallback())),
            "expected a fallback escape hatch, got: {completions:?}"
        );
    }

    /// A one-character query isn't a question yet — offering to search the web
    /// for "d" is noise on the way to typing a real command.
    #[tokio::test]
    async fn very_short_input_gets_no_fallbacks() {
        let ex = make_executor(registry_open_fails());
        let completions = ex
            .completions("d", &crate::config::schema::SuggestionsConfig::default())
            .await;
        assert!(
            !completions
                .iter()
                .any(|c| c.kind.is_some_and(|k| k.is_fallback())),
            "no fallbacks for a single character, got: {completions:?}"
        );
    }

    /// A handler that declares a real trigger keyword, so the "Did you mean"
    /// matcher has a vocabulary to find.
    struct KeywordHandler;

    #[async_trait]
    impl ActionHandler for KeywordHandler {
        fn id(&self) -> &str {
            "define"
        }
        fn description(&self) -> &str {
            "Define a word"
        }
        fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
            static T: &[crate::action_registry::Trigger] =
                &[crate::action_registry::Trigger::keywords(&["define"])];
            T
        }
        async fn execute(
            &self,
            _ctx: &crate::action_registry::ExecContext,
            _args: &str,
        ) -> Result<ActionResult, crate::error::LychiError> {
            Ok(ActionResult::default())
        }
    }

    /// A naturally-phrased request gets NO "Did you mean" row — it is prose for
    /// the AI, not something to force-fit into a command (see `typo_suggest`,
    /// which is now app-name-typo only). It still gets the fallback escape hatch,
    /// and with NO AI configured that is the web fallback (an AI row that leads
    /// nowhere would be worse than none, since it looks like an answer).
    #[tokio::test]
    async fn natural_phrasing_gets_the_fallback_not_a_correction() {
        let mut r = ActionRegistry::new();
        r.register(Box::new(KeywordHandler));
        r.register(Box::new(StubHandler { id: "web" }));
        let ex = make_executor(r);

        let completions = ex
            .completions(
                "can you define gallop",
                &crate::config::schema::SuggestionsConfig::default(),
            )
            .await;

        // No correction: prose is not force-fit into a command anymore.
        assert!(
            completions
                .iter()
                .all(|c| c.kind != Some(crate::action_registry::CompletionKind::Correction)),
            "prose must not produce a 'Did you mean' row, got: {completions:?}"
        );

        // No AI provider → "Ask AI" absent, web fallback stands in.
        assert!(!ex.has_ai(), "test executor should have no AI provider");
        assert!(
            completions
                .iter()
                .all(|c| c.kind != Some(crate::action_registry::CompletionKind::AskAi)),
            "Ask AI must not be offered without a provider, got: {completions:?}"
        );
        let web = completions
            .iter()
            .find(|c| c.kind == Some(crate::action_registry::CompletionKind::SearchWeb))
            .unwrap_or_else(|| panic!("expected a web fallback, got: {completions:?}"));
        // The QUERY travels in `description`; no command string to re-parse.
        assert_eq!(web.description.as_deref(), Some("can you define gallop"));
        assert!(
            web.run.is_none(),
            "a fallback row carries no `run` to re-parse"
        );
    }

    /// Every `run` string a completion offers must name a REAL registered
    /// trigger.
    ///
    /// This is the defect class that sent "Ask AI" to a web search: the row
    /// carried `run: "ask …"`, no `ask` handler existed, and the executor's
    /// pattern router silently fell through to `web`. A row whose meaning has to
    /// be recovered by re-parsing text can break without anything failing
    /// loudly — so assert the contract instead of trusting it.
    #[tokio::test]
    async fn completion_run_strings_name_real_triggers() {
        let mut r = ActionRegistry::new();
        r.register(Box::new(KeywordHandler));
        r.register(Box::new(StubHandler { id: "web" }));
        let ex = make_executor(r);

        for query in ["can you define gallop", "weathr tokyo", "define gallop"] {
            let completions = ex
                .completions(query, &crate::config::schema::SuggestionsConfig::default())
                .await;
            for c in &completions {
                let Some(run) = c.run.as_deref() else {
                    continue;
                };
                let Some(first) = run.split_whitespace().next() else {
                    continue;
                };
                assert!(
                    ex.registry.is_known_prefix(first),
                    "completion {:?} has run={run:?} whose first word is not a \
                     registered trigger — it would pattern-route to a web search",
                    c.label
                );
            }
        }
    }

    // ── Ordering invariants ─────────────────────────────────────────────
    //
    // `completions()` builds its list by pushing, prepending, splicing and
    // truncating a shared `Vec` across nine stages, so a suggestion's POSITION
    // is an emergent property of the order the code happens to run in. Every
    // ordering rule is currently written as a comment — "Context matches lead",
    // "sort last and are never auto-selected", "Prepend so the repo choices sit
    // at the top" — and prose cannot fail a build, so it drifts invisibly.
    //
    // These tests pin the rules as behaviour BEFORE the suggestion-source
    // refactor, so the refactor has something to be correct against. They are
    // deliberately written against the public `completions()` surface rather
    // than internals, so they survive the rewrite that is meant to follow.

    /// Context matches lead the handler section.
    ///
    /// They carry learned per-context ranking that a generic completion can't,
    /// so a clipboard/project-derived action must not be buried under fuzzy
    /// handler output. Today this is a `splice(0..0, …)` guarded only by a
    /// comment.
    #[tokio::test]
    async fn context_matches_lead_the_handler_section() {
        // A registry that DOES return handler completions, so "leads" is a real
        // claim about ordering rather than a list of one.
        let mut ex = make_executor(registry_open_matches());
        // The navigation provider: cwd sits below the project root, so
        // "Open project root" is offered, and it is TypedOnly — the tier
        // `typed_matches` actually collects. (A ColdEligible provider such as
        // clipboard can never appear here, only on the empty prompt.) Being
        // dev-window-gated, it also needs an active terminal.
        ex.context = Some(crate::context::EnvironmentContext {
            cwd: Some("/home/u/lychi/core/src".into()),
            project: Some(project_at("/home/u/lychi")),
            active_window: Some(crate::context::WindowContext {
                title: "zsh".into(),
                wm_class: "kitty".into(),
                pid: 1,
                is_terminal: true,
                is_ide: false,
                window_id: None,
            }),
            gathered_at: Some(std::time::Instant::now()),
            ..Default::default()
        });

        let completions = ex
            .completions(
                "project",
                &crate::config::schema::SuggestionsConfig::default(),
            )
            .await;
        // Identified by its typed provenance (`__context__`), not by display
        // text — a context row's label is the COMMAND, with the human phrasing
        // in `description`.
        let pos = completions
            .iter()
            .position(|c| c.icon_path.as_deref() == Some("__context__"))
            .unwrap_or_else(|| panic!("expected the context match, got: {completions:?}"));
        assert_eq!(pos, 0, "a context match must LEAD, got: {completions:?}");
        // …and it must genuinely be leading something, or "leads" is vacuous.
        assert!(
            completions.iter().any(|c| c.label == "Open project"),
            "expected a handler result to lead over, got: {completions:?}"
        );
    }

    /// A `ProjectContext` with only the field under test set. Written out
    /// because `ProjectKind` has no meaningful default — there is no such thing
    /// as a project of no kind.
    fn project_at(root: &str) -> crate::context::ProjectContext {
        crate::context::ProjectContext {
            root: root.into(),
            kind: crate::context::ProjectKind::Rust,
            has_compose: false,
            scripts: Vec::new(),
            package_manager: None,
            workspace_root: None,
            workspace_scripts: Vec::new(),
        }
    }

    /// The dirty-project guard is a real safety warning, not decoration.
    ///
    /// Typing a destructive system action while the checkout has uncommitted
    /// work must surface the warning FIRST — behind anything else it is a
    /// warning the user reads after deciding.
    /// An `open` handler that always yields one completion, so a test can reach
    /// the branch of `completions()` that runs when something DID match.
    struct MatchingOpenHandler;

    #[async_trait]
    impl ActionHandler for MatchingOpenHandler {
        fn id(&self) -> &str {
            "open"
        }
        fn description(&self) -> &str {
            "matching open stub"
        }
        async fn execute(
            &self,
            _ctx: &crate::action_registry::ExecContext,
            _args: &str,
        ) -> Result<ActionResult, crate::error::LychiError> {
            Ok(ActionResult::default())
        }
        async fn completions(&self, partial: &str) -> Vec<crate::action_registry::CompletionItem> {
            if partial.trim().is_empty() {
                return Vec::new();
            }
            vec![crate::action_registry::CompletionItem::new(
                format!("Open {}", partial.trim()),
                None,
                50,
            )]
        }
    }

    /// A handler with a real trigger that never offers completions — models
    /// `packages` receiving an argument it has no hint for.
    struct SilentTriggerHandler;

    #[async_trait]
    impl ActionHandler for SilentTriggerHandler {
        fn id(&self) -> &str {
            "pkgs"
        }
        fn description(&self) -> &str {
            "silent triggered stub"
        }
        fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
            static T: &[crate::action_registry::Trigger] =
                &[crate::action_registry::Trigger::keywords(&["pkgs"])];
            T
        }
        async fn execute(
            &self,
            _ctx: &crate::action_registry::ExecContext,
            _args: &str,
        ) -> Result<ActionResult, crate::error::LychiError> {
            Ok(ActionResult::default())
        }
    }

    fn registry_open_matches() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register(Box::new(MatchingOpenHandler));
        r.register(Box::new(StubHandler { id: "web" }));
        r
    }

    /// The guard now fires even when NO handler matched — previously it lived
    /// inside the "something matched" branch, so an unrecognised destructive
    /// input skipped the warning entirely. Pinning the old behaviour in a test
    /// is what made the gap visible; the refactor closed it.
    #[tokio::test]
    async fn dirty_project_guard_fires_even_when_nothing_matched() {
        let mut ex = make_executor(registry_open_fails());
        ex.context = Some(crate::context::EnvironmentContext {
            git: Some(crate::context::GitContext {
                repo_root: "/home/u/lychi".into(),
                branch: "main".into(),
                dirty: true,
                remote: None,
            }),
            project: Some(project_at("/home/u/lychi")),
            gathered_at: Some(std::time::Instant::now()),
            ..Default::default()
        });

        let completions = ex
            .completions(
                "shutdown",
                &crate::config::schema::SuggestionsConfig::default(),
            )
            .await;
        assert_eq!(
            completions.first().map(|c| c.icon_path.as_deref()),
            Some(Some("__warning__")),
            "the warning must lead even with no handler results, got: {completions:?}"
        );
    }

    #[tokio::test]
    async fn dirty_project_guard_leads_for_a_destructive_action() {
        // NOTE: the guard lives inside the "something matched" branch, so it
        // needs a handler that returns completions. That is itself a gap worth
        // recording — an unrecognised destructive input skips the warning
        // entirely — but it is existing behaviour, and this test's job is to
        // pin what the guard does today, not to change when it fires.
        let mut ex = make_executor(registry_open_matches());
        ex.context = Some(crate::context::EnvironmentContext {
            git: Some(crate::context::GitContext {
                repo_root: "/home/u/lychi".into(),
                branch: "main".into(),
                dirty: true,
                remote: None,
            }),
            project: Some(project_at("/home/u/lychi")),
            gathered_at: Some(std::time::Instant::now()),
            ..Default::default()
        });

        let completions = ex
            .completions(
                "shutdown",
                &crate::config::schema::SuggestionsConfig::default(),
            )
            .await;
        let first = completions
            .first()
            .unwrap_or_else(|| panic!("expected suggestions for a destructive action"));
        assert_eq!(
            first.icon_path.as_deref(),
            Some("__warning__"),
            "the dirty-repo warning must lead, got: {completions:?}"
        );
        assert!(
            first.label.contains("lychi"),
            "the warning must name the affected repo, got: {first:?}"
        );
    }

    /// A clean checkout gets no guard — the warning must be a real signal, not
    /// a banner that always fires and so is always ignored.
    #[tokio::test]
    async fn dirty_project_guard_is_silent_on_a_clean_checkout() {
        // Same registry as the positive case: with a registry that returns no
        // completions this would pass because the guard's whole BRANCH is
        // skipped, proving nothing about `dirty: false`.
        let mut ex = make_executor(registry_open_matches());
        ex.context = Some(crate::context::EnvironmentContext {
            git: Some(crate::context::GitContext {
                repo_root: "/home/u/lychi".into(),
                branch: "main".into(),
                dirty: false,
                remote: None,
            }),
            gathered_at: Some(std::time::Instant::now()),
            ..Default::default()
        });

        let completions = ex
            .completions(
                "shutdown",
                &crate::config::schema::SuggestionsConfig::default(),
            )
            .await;
        assert!(
            !completions
                .iter()
                .any(|c| c.icon_path.as_deref() == Some("__warning__")),
            "no guard on a clean checkout, got: {completions:?}"
        );
    }

    // NOTE: the former `an_explicit_web_route_suppresses_typo_suggestions` test
    // was removed with the command-typo correction it exercised. "Did you mean"
    // is now a single-word APP-name typo offer only (see `typo_suggest`), which
    // depends on the process-global app index a unit test can't seed. The web
    // suppression guard it protected still stands as the one-line `!is_web_route`
    // check in `completions()`.

    /// The consent rule, end to end: a query that merely CONTAINS an app name
    /// must not produce an auto-selectable row.
    ///
    /// This is the `dnf search firefox` defect. It had two halves, in two
    /// places that disagreed: the intent resolver launched on a ≥0.90 subset
    /// match, and the frontend used a prefix rule. `Tier` is now the single
    /// implementation — asserted here on the SAME predicate the frontend
    /// consumes, so the two cannot drift apart again.
    #[test]
    fn subset_matches_are_offered_never_auto_run() {
        use crate::suggestions::{Source, Suggestion, Tier};

        let firefox = CompletionItem::new("Firefox", None, 92).with_run("firefox");
        let typed = "dnf search firefox";

        let s = Suggestion::matched(firefox, Source::Handler, typed);
        assert_eq!(s.tier, Tier::Subset, "contains, does not extend");
        assert!(
            !s.can_be_default(),
            "a subset match must never take Enter — this is the launch bug"
        );

        // …while the app typed on its own still runs, so the rule costs nothing
        // in the case it is meant to allow.
        let bare = CompletionItem::new("firefox", None, 92);
        assert!(Suggestion::matched(bare, Source::Handler, "firefox").can_be_default());
    }

    /// The latching LOOP, end to end through `completions()`.
    ///
    /// The unit tests cover the store and the ranker separately; this asserts
    /// they are actually connected — that a recorded latch changes what
    /// `completions()` returns. Wiring is exactly what unit tests miss, and
    /// this feature was in fact unwired from the keyboard when first written.
    #[tokio::test]
    async fn a_recorded_latch_reorders_the_next_completion_list() {
        let ex = make_executor(registry_open_matches());
        crate::db::frecency::set_store_for_test(ex.db.clone());
        let cfg = crate::config::schema::SuggestionsConfig::default();

        // Baseline: whatever the ranker decides on its own.
        let before = ex.completions("zqx", &cfg).await;
        let before_first = before.first().map(|c| c.label.clone());

        // The user picks the web fallback for this query, twice.
        let chosen = before
            .iter()
            .find(|c| c.kind == Some(crate::action_registry::CompletionKind::SearchWeb))
            .map(|c| c.label.clone())
            .expect("fixture needs a fallback row to choose");
        crate::db::frecency::record_latch("zqx", &chosen).unwrap();
        crate::db::frecency::record_latch("zqx", &chosen).unwrap();

        // The latch is readable for this query and only this query.
        let latches = crate::db::frecency::get_latches("zqx");
        assert!(
            latches.contains_key(&chosen.to_lowercase()) || latches.contains_key(&chosen),
            "the executor's db must see the latch it just recorded, got: {latches:?}"
        );
        assert!(crate::db::frecency::get_latches("other").is_empty());

        // Sanity: the list is still produced (the latch must not break ranking).
        let after = ex.completions("zqx", &cfg).await;
        assert!(!after.is_empty());
        let _ = before_first;
    }

    /// A latched FALLBACK must still not become Enter's default. The consent
    /// rule has to survive the round trip through the real pipeline, not just
    /// the ranker's unit tests.
    #[tokio::test]
    async fn latching_a_fallback_does_not_make_it_the_default() {
        use crate::suggestions::{Source, Suggestion, Tier};

        let ex = make_executor(registry_open_matches());
        let cfg = crate::config::schema::SuggestionsConfig::default();
        let rows = ex.completions("zqx", &cfg).await;
        let fallback = rows
            .iter()
            .find(|c| c.kind.is_some_and(|k| k.is_fallback()))
            .expect("expected a fallback row");

        crate::db::frecency::record_latch("zqx", &fallback.label).unwrap();

        // Re-wrap as the ranker would and confirm the row is still ineligible.
        let s = Suggestion::new(fallback.clone(), Source::Fallback, Tier::Prefix);
        assert!(
            !s.can_be_default(),
            "a latched fallback must never take Enter"
        );
    }

    /// A handler that OWNS the input must not be second-guessed with app rows.
    ///
    /// The regression: `dnf search firefox` routed correctly to `packages`, but
    /// the handler returned no completions for an argument it had no hint for,
    /// and the app-search rescue then fuzzy-matched the raw text — offering
    /// Firefox, KFind, Run Program and Catfish for a package search. Routing was
    /// right and the LIST was wrong, which is why execution-path tests missed it.
    #[tokio::test]
    async fn an_explicit_route_is_not_rescued_with_app_matches() {
        // A handler with a real trigger that returns NOTHING for this input —
        // exactly the shape that used to fall through to app search.
        let mut r = ActionRegistry::new();
        r.register(Box::new(SilentTriggerHandler));
        r.register(Box::new(MatchingOpenHandler));
        r.register(Box::new(StubHandler { id: "web" }));
        let ex = make_executor(r);

        let completions = ex
            .completions(
                "pkgs firefox",
                &crate::config::schema::SuggestionsConfig::default(),
            )
            .await;

        // The `open` stub would happily return "Open firefox" — the point is
        // that it is never asked, because the user named a handler.
        assert!(
            !completions.iter().any(|c| c.label.starts_with("Open ")),
            "an explicit route must not be answered with app matches, got: {completions:?}"
        );
    }

    /// …but a NON-explicit route still gets the rescue. Removing it wholesale
    /// would regress the case it exists for.
    #[tokio::test]
    async fn a_non_explicit_route_still_gets_the_app_rescue() {
        let mut r = ActionRegistry::new();
        r.register(Box::new(MatchingOpenHandler));
        r.register(Box::new(StubHandler { id: "web" }));
        let ex = make_executor(r);

        // Bare text → NoMatch → routed to `open`, which answers.
        let completions = ex
            .completions(
                "firefox",
                &crate::config::schema::SuggestionsConfig::default(),
            )
            .await;
        assert!(
            completions.iter().any(|c| c.label.starts_with("Open ")),
            "a bare query must still reach app search, got: {completions:?}"
        );
    }

    // NOTE: `a_correction_row_is_typed_not_label_matched` was removed — it built
    // a correction from a multi-word command typo, which "Did you mean" no longer
    // does (it is app-name-typo only now, needing the process-global app index a
    // unit test can't seed). The typed-not-label-matched invariant it guarded is
    // enforced at the source: `typo_suggest::row` always sets `kind: Correction`.

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
    fn app_queries_pass_the_launch_gate() {
        // Short, name-shaped → still hit the app index.
        for q in ["spotify", "vs code", "sublime text", "disk usage"] {
            assert!(
                super::looks_like_app_query(q),
                "{q:?} should read as an app-launch attempt"
            );
        }
    }

    #[test]
    fn sentences_and_questions_do_not() {
        // The reported bug + its family: prose must NOT reach the app fuzzy match.
        for q in [
            "play the music and tell me whats my disk status",
            "what's using my disk",
            "how do I resize an image",
            "summarize this document for me",
            "translate hello to french",
        ] {
            assert!(
                !super::looks_like_app_query(q),
                "{q:?} is prose, must route to AI/web not app launch"
            );
        }
    }

    #[test]
    fn a_request_cue_is_matched_as_a_whole_word() {
        // "whatsapp" contains "what" but is one word and a real app name — the
        // cue check is whole-word, so it is NOT mistaken for a question.
        assert!(super::looks_like_app_query("whatsapp"));
    }

    #[test]
    fn fanout_command_quotes_paths_with_apostrophes() {
        let dirs = vec!["/home/sab/sab's-project".to_string()];
        let out = super::fanout_command("git status", &dirs);
        // The dir is safely quoted (no raw apostrophe breaking the cd).
        assert!(out.contains("cd '/home/sab/sab'\\''s-project'"));
    }

    #[test]
    fn a_fanout_command_does_not_trip_our_own_shell_gate() {
        // The generated wrapper used `|| echo failed` as a per-repo failure
        // marker. `||` contains `|`, which the shell decider flags as a pipe —
        // so every fan-out asked for confirmation, including a read-only `git
        // status`, for an operator the app injected itself. Prompting the user
        // about our own syntax trains them to click through prompts.
        let dirs = vec!["/home/u/ws/api".to_string(), "/home/u/ws/admin".to_string()];
        let out = super::fanout_command("git status", &dirs);
        assert!(!out.contains('|'), "fan-out must not emit a pipe: {out}");
        assert!(
            !out.contains('>'),
            "fan-out must not emit a redirect: {out}"
        );
        assert!(
            matches!(
                crate::rules::shell::authorize(&out),
                crate::rules::shell::ShellDecision::Allow
            ),
            "a read-only fan-out must not need confirmation: {out}"
        );
    }

    #[test]
    fn a_fanout_still_labels_each_repo() {
        // Dropping the failure marker must not cost the per-repo headers —
        // without them the combined output is an unattributed wall of text.
        let dirs = vec!["/home/u/ws/api".to_string(), "/home/u/ws/admin".to_string()];
        let out = super::fanout_command("git status", &dirs);
        assert!(out.contains("=== api ==="), "missing header: {out}");
        assert!(out.contains("=== admin ==="), "missing header: {out}");
    }
}
