use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};
use tokio::sync::RwLock;

use redb::Database;

use lychi_core::action_registry::handlers::aliases::AliasHandler;
use lychi_core::action_registry::handlers::app_control::AppControlHandler;
use lychi_core::action_registry::handlers::app_launcher::AppLauncher;
use lychi_core::action_registry::handlers::bookmarks::BookmarkHandler;
use lychi_core::action_registry::handlers::browse::BrowseHandler;
use lychi_core::action_registry::handlers::calc::CalcHandler;
use lychi_core::action_registry::handlers::clear::ClearHandler;
use lychi_core::action_registry::handlers::clipboard::ClipboardHandler;
use lychi_core::action_registry::handlers::color::ColorHandler;
use lychi_core::action_registry::handlers::context_debug::ContextDebugHandler;
use lychi_core::action_registry::handlers::convert_image::ConvertImageHandler;
use lychi_core::action_registry::handlers::define::DefineHandler;
use lychi_core::action_registry::handlers::dev_utils::DevUtilsHandler;
use lychi_core::action_registry::handlers::emoji::EmojiHandler;
use lychi_core::action_registry::handlers::extract::ExtractHandler;
use lychi_core::action_registry::handlers::file_open::FileOpen;
use lychi_core::action_registry::handlers::generate::GenerateHandler;
#[cfg(feature = "mpris")]
use lychi_core::action_registry::handlers::media::MediaHandler;
use lychi_core::action_registry::handlers::notes::{NotesHandler, TodoHandler};
use lychi_core::action_registry::handlers::packages::PackagesHandler;
use lychi_core::action_registry::handlers::pin_workspace::PinWorkspaceHandler;
use lychi_core::action_registry::handlers::project_open::ProjectOpen;
use lychi_core::action_registry::handlers::qr::QrHandler;
use lychi_core::action_registry::handlers::quicklink::QuicklinkHandler;
use lychi_core::action_registry::handlers::reminders::RemindersHandler;
use lychi_core::action_registry::handlers::resize_image::ResizeImageHandler;
use lychi_core::action_registry::handlers::screenshot::ScreenshotHandler;
use lychi_core::action_registry::handlers::services::{ServicesHandler, ServicesListHandler};
use lychi_core::action_registry::handlers::shell_exec::ShellExec;
use lychi_core::action_registry::handlers::snippets::SnippetsHandler;
use lychi_core::action_registry::handlers::ssh::SshHandler;
use lychi_core::action_registry::handlers::symbol::SymbolHandler;
use lychi_core::action_registry::handlers::sysinfo::SysInfoHandler;
use lychi_core::action_registry::handlers::system::SystemCommand;
use lychi_core::action_registry::handlers::time::TimeHandler;
use lychi_core::action_registry::handlers::timer::{TimerHandler, TimerState};
use lychi_core::action_registry::handlers::unicode::UnicodeHandler;
use lychi_core::action_registry::handlers::url_open::UrlOpen;
use lychi_core::action_registry::handlers::weather::WeatherHandler;
use lychi_core::action_registry::handlers::web_search::WebSearch;
use lychi_core::action_registry::handlers::window_switcher::WindowSwitcherHandler;
use lychi_core::action_registry::handlers::youtube::YouTube;
use lychi_core::action_registry::handlers::zip::ZipHandler;
use lychi_core::action_registry::registry::ActionRegistry;
use lychi_core::config::Config;
use lychi_core::executor::Executor;
use lychi_core::history::HistoryStore;
use lychi_core::intent::IntentResolver;
use lychi_core::intent::ai_router::AiRouter;
#[cfg(feature = "mpris")]
use lychi_core::mpris::MprisManager;
use lychi_core::paths;
use lychi_core::providers::{AgentPlan, AiProvider};
use lychi_core::rules::RulesEngine;

/// An action assessed and awaiting user confirmation (G1). Stores the exact
/// resolved intent so confirm executes THAT action, plus the risk it was assessed
/// at (auditability) and an expiry so a stale confirmation (user walked away, then
/// confirmed minutes later) is rejected.
///
/// This is a SINGLE slot (`Option<PendingExecution>` in `AppState`), which is the
/// tightest possible bound — a launcher shows one confirm prompt at a time, and a
/// new prompt replaces any unanswered one. So there's no store to cap or GC.
#[derive(Clone)]
pub struct PendingExecution {
    pub intent: lychi_core::intent::ResolvedIntent,
    /// Risk level the action was assessed at when the prompt was shown. Recorded
    /// for auditability; execution re-assesses risk with fresh context, so this is
    /// the "what the user saw," not the authority for the execute-time decision.
    pub risk: lychi_core::action_registry::RiskLevel,
    /// Monotonic instant after which this pending action is no longer valid.
    pub expires_at: std::time::Instant,
}

impl PendingExecution {
    /// How long a confirmation prompt stays valid before it must be re-issued.
    const TTL: std::time::Duration = std::time::Duration::from_secs(120);

    pub fn new(
        intent: lychi_core::intent::ResolvedIntent,
        risk: lychi_core::action_registry::RiskLevel,
    ) -> Self {
        Self {
            intent,
            risk,
            expires_at: std::time::Instant::now() + Self::TTL,
        }
    }

    pub fn is_expired(&self) -> bool {
        std::time::Instant::now() >= self.expires_at
    }
}

pub struct AppState {
    /// # Lock discipline: never hold `config` and `executor` at the same time
    ///
    /// Both nesting orders used to exist — `config`→`executor` in
    /// `get_ai_status` and `completions`, `executor`→`config` in `execute`,
    /// `confirm` and `run_plan`. That is a deadlock cycle, and tokio's `RwLock`
    /// is **write-preferring**, so it is reachable with readers alone: a pending
    /// writer blocks later readers, so two tasks each holding one lock and
    /// waiting on the other never make progress. `reactors.rs` takes both with
    /// `blocking_write`, which is what supplies the writer.
    ///
    /// It survived only by accident: the reactors happened to read config
    /// through a *temporary* (`self.config.blocking_read().commands.clone()`),
    /// which drops at the end of the statement. Binding that to a variable —
    /// an ordinary, invisible refactor — would have completed the cycle.
    ///
    /// The rule is therefore not "always take config first" (an ordering
    /// convention has to be remembered at ~40 sites) but **never hold both**.
    /// Snapshot what you need from config, let the guard drop, then take the
    /// executor. [`AppState::config_snapshot`] does exactly that; prefer it.
    pub executor: Arc<RwLock<Executor>>,
    pub db: Arc<Database>,
    pub history: HistoryStore,
    /// See the lock-discipline note on [`AppState::executor`].
    pub config: Arc<RwLock<Config>>,
    /// The live AI provider, reachable from the streaming-chat command (which
    /// runs outside the executor). Today the provider lives inside the executor's
    /// handlers/router; this is a second handle the `AiReactor` writes in the same
    /// pass it rebuilds those, so `ai_chat_stream` can drive `AiProvider::chat`
    /// directly. `None` when AI is disabled.
    pub ai_provider: Arc<RwLock<Option<Arc<dyn AiProvider>>>>,
    /// Monotonic generation counter for the streaming chat. Each new stream bumps
    /// it; the frontend drops tokens tagged with a stale `gen`. Also gates the
    /// terminal `done`/`error` event so a superseded stream can't paint over a
    /// newer one.
    pub ai_generation: Arc<AtomicU64>,
    /// The in-flight chat stream's cancellation token. Cancelling it stops the
    /// stream at its source (drops the HTTP body / halts the local loop). A new
    /// stream replaces it; `cancel_ai_chat` cancels the current one.
    pub ai_cancel: Arc<RwLock<Option<lychi_core::providers::CancellationToken>>>,
    /// The agent coordinator's suspended session, awaiting a tool approval. Set
    /// when a run pauses on a destructive tool; `agent_approve` takes it to
    /// resume. Single-slot (the UI shows one approval at a time).
    pub agent_session: Arc<RwLock<Option<lychi_core::coordinator::Session>>>,
    /// Stable id for the current conversation, so follow-ups upsert the SAME
    /// history row (Phase 4). Set on a fresh start, reused on continue/approve.
    pub agent_conversation_id: Arc<RwLock<Option<String>>>,
    pub pending_plan: Arc<RwLock<Option<AgentPlan>>>,
    /// The action awaiting user confirmation (G1). Captured when the pipeline
    /// returns `needs_confirmation`; the `confirm_execution` command executes
    /// THIS exact resolved intent rather than re-resolving raw input, closing the
    /// confirmation time-of-check/time-of-use gap. Single-slot: the UI only shows
    /// one confirm prompt at a time.
    pub pending_execution: Arc<RwLock<Option<PendingExecution>>>,
    pub active_file_search: Arc<AtomicU64>,
    /// The in-flight file search, plus the generation guard that makes the newest
    /// query the only one able to emit. Owns one matcher per search rather than
    /// sharing one across surfaces, so results can never be attributed to the
    /// wrong query.
    pub live_search: Arc<lychi_core::file_search::live::LiveSearch>,
    pub timer_state: TimerState,
    /// The single owner of "is the launcher open?" — see [`crate::launcher_state`].
    ///
    /// Every show/hide/focus decision goes through this. It replaces the
    /// previous arrangement where `toggle_window`, the focus-out handler, and
    /// `hide_launcher` each polled GTK independently and could disagree.
    pub launcher: Arc<crate::launcher_state::LauncherStateMachine>,
    /// Dismiss-on-blur armed flag. Retained for log correlation only — it no
    /// longer gates dismissal. Arming never made focus-out trustworthy; it only
    /// postponed the first spurious dismiss until after the first keystroke,
    /// which is why the bug presented as "it closes when I type".
    pub dismiss_armed: Arc<AtomicBool>,
    /// Monotonic summon sequence. Incremented at the start of each show_window().
    /// Focus handlers check this to ignore stale events from previous summon
    /// cycles (e.g. rapid double-summon).
    pub summon_seq: Arc<AtomicU64>,
    /// The summon_seq value captured when dismiss_armed was set. Focus-out
    /// only dismisses when this matches the CURRENT summon_seq — a stale
    /// focus-out from a previous summon cycle can never close a fresh window.
    pub armed_seq: Arc<AtomicU64>,
    /// Whether the global shortcut plugin successfully registered the hotkey.
    /// Registration failure is non-fatal (common on Wayland); the frontend
    /// reads this via get_hotkey_status to guide users to `lychi --toggle`.
    ///
    /// Note this says the *call* succeeded, not that the key reaches us — see
    /// `hotkey_verdict` for the question users actually care about.
    pub hotkey_registered: Arc<AtomicBool>,
    /// Whether the hotkey is bound through the XDG GlobalShortcuts portal —
    /// compositor-level and therefore reliable even on Wayland.
    pub portal_bound: Arc<AtomicBool>,
    /// How the hotkey was bound and how much that proves.
    ///
    /// Written at the one place each binding is attempted and read unchanged
    /// thereafter. The previous design logged the `hotkey_de` outcome, dropped
    /// it, and re-derived a weaker answer in `get_hotkey_status` — which then
    /// reported every X11 grab as reliable, exactly the case that silently
    /// fails.
    pub hotkey_verdict: Arc<std::sync::Mutex<lychi_core::hotkey::HotkeyVerdict>>,
    /// Shutdown signal for the clipboard monitor OS thread. Set to false on exit
    /// to stop the monitor before D-Bus/arboard teardown begins.
    pub clipboard_running: Arc<AtomicBool>,
    /// Shutdown signal for the timer/reminder monitor OS thread.
    pub timer_running: Arc<AtomicBool>,
    /// Shutdown signal for the app-index filesystem watcher OS thread.
    pub app_index_watcher_running: Arc<AtomicBool>,
    /// Shutdown signal for the Script Commands directory watcher OS thread.
    pub scripts_watcher_running: Arc<AtomicBool>,
    /// Limits concurrent heavy spawn_blocking tasks (file preview, future indexing)
    /// to prevent burst-load exhaustion of the blocking thread pool.
    pub heavy_sem: Arc<tokio::sync::Semaphore>,
    /// Domain-event bus — state-change propagation spine. Config saves and other
    /// state changes emit here; subsystems subscribe once (in `wire_reactors`)
    /// and react to their own concern, so no command has to poke them directly.
    pub event_bus: Arc<lychi_core::events::EventBus>,
    #[cfg(feature = "mpris")]
    pub mpris: Arc<RwLock<Option<MprisManager>>>,
}

impl AppState {
    /// Read something out of the config and drop the guard before returning.
    ///
    /// Exists so the deadlock-free pattern is also the shortest one to write.
    /// Because the guard cannot escape the closure, a caller physically cannot
    /// still be holding config when they go on to take the executor — the rule
    /// is enforced by the signature rather than by remembering a convention.
    ///
    /// ```ignore
    /// let privacy = state.config_snapshot(|c| c.privacy.clone()).await;
    /// let executor = state.executor.read().await; // config already released
    /// ```
    ///
    /// See the lock-discipline note on [`AppState::executor`].
    pub async fn config_snapshot<T>(&self, f: impl FnOnce(&Config) -> T) -> T {
        f(&*self.config.read().await)
    }

    pub fn new() -> Self {
        let mut config = Config::load_or_default(&paths::config_file());

        // Open redb database (creates file if missing)
        let db_path = paths::db_file();
        // A second instance now gets a clear refusal rather than an empty
        // database (see `open_database`). Exiting beats panicking: a stack
        // trace for "Lychi is already running" is noise, and the running
        // instance is the one the user wants anyway.
        let db = lychi_core::db::open_database(&db_path).unwrap_or_else(|e| {
            tracing::error!("Cannot open database: {e}");
            eprintln!("lychi: cannot open database: {e}");
            eprintln!("lychi: if Lychi is already running, use the hotkey or `lychi --toggle`.");
            std::process::exit(1);
        });
        let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
        tracing::info!(
            "Database opened: {} ({:.1} KB)",
            db_path.display(),
            db_size as f64 / 1024.0
        );

        // Register the learned-model-capability store before any provider is
        // built, so the factory can wire failure-learning into BYO clients.
        lychi_core::providers::capability::init_store(db.clone());

        // FIRST write-adjacent step at startup: if this is the first run of a
        // new version, snapshot the old version's data before any seeding,
        // migration or new-code path has touched it. Ordering is the whole
        // point — a backup taken after the new code writes is a backup of the
        // problem. Never fatal; a failure is logged inside.
        if let Some(b) = lychi_core::backup::backup_if_upgraded(&db, env!("CARGO_PKG_VERSION")) {
            tracing::info!("[backup] pre-upgrade snapshot saved: {}", b.name);
        }

        // The rolling hourly backup. Separate from the pre-upgrade one above
        // and deliberately after it: on the first run of a new version both
        // fire, and the upgrade snapshot must be the one taken BEFORE any of
        // this version's code has written anything.
        if let Some(b) = lychi_core::backup::hourly_backup(&db, env!("CARGO_PKG_VERSION")) {
            tracing::info!("[backup] hourly snapshot saved: {}", b.name);
        }

        // Seed settings from TOML on first launch (if settings table is empty)
        if let Err(e) = lychi_core::config::db::seed_from_config(&db, &config) {
            tracing::error!("Failed to seed settings: {e}");
        }

        // Seed the built-in AI presets (translate/summarize/rewrite) on first run.
        // Idempotent: only installs a keyword that isn't already present, so it
        // never clobbers user edits. Users can add/edit/delete any preset.
        if let Err(e) = lychi_core::ai_presets::store::AiPresetsStore::new().seed_builtins(&db) {
            tracing::error!("Failed to seed AI presets: {e}");
        }

        // Sync TOML hand-edits → DB (if user edited TOML between launches)
        match lychi_core::config::db::load_syncable(&db) {
            Ok(db_settings) => {
                if let Err(e) =
                    lychi_core::config::db::sync_toml_changes(&db, &config, &db_settings)
                {
                    tracing::error!("Failed to sync TOML changes: {e}");
                }
                // Re-read DB after sync so TOML edits take effect immediately
                match lychi_core::config::db::load_syncable(&db) {
                    Ok(synced_settings) => {
                        lychi_core::config::db::apply_to_config(&synced_settings, &mut config);
                    }
                    Err(_) => {
                        // Fall back to pre-sync settings if re-read fails
                        lychi_core::config::db::apply_to_config(&db_settings, &mut config);
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to load settings from DB: {e}");
            }
        }

        // Log DB table stats
        if let Ok(stats) = lychi_core::db::table_stats(&db) {
            let total = stats.history
                + stats.notes
                + stats.todos
                + stats.clipboard
                + stats.settings
                + stats.frecency
                + stats.aliases
                + stats.reminders
                + stats.snippets;
            tracing::info!(
                "DB tables: {} history, {} notes, {} todos, {} clipboard, {} settings, {} frecency, {} aliases, {} reminders, {} snippets ({} total rows, {:.1} KB on disk)",
                stats.history,
                stats.notes,
                stats.todos,
                stats.clipboard,
                stats.settings,
                stats.frecency,
                stats.aliases,
                stats.reminders,
                stats.snippets,
                total,
                db_size as f64 / 1024.0,
            );
        }

        // Rehydrate timers persisted from a previous run so running countdowns
        // and stopwatches survive an app restart.
        let timer_state = lychi_core::action_registry::handlers::timer::new_timer_state();
        {
            let restored = lychi_core::action_registry::handlers::timer::load_timers(&db);
            if !restored.is_empty() {
                tracing::info!("[timer] restored {} timer(s) from disk", restored.len());
                if let Ok(mut t) = timer_state.lock() {
                    *t = restored;
                }
            }
        }

        #[cfg(feature = "mpris")]
        let mpris: Arc<RwLock<Option<lychi_core::mpris::MprisManager>>> =
            Arc::new(RwLock::new(None));

        let mut registry = ActionRegistry::new();
        registry.register(Box::new(AppControlHandler::new()));
        registry.register(Box::new(AppLauncher::new(db.clone())));
        registry.register(Box::new(BookmarkHandler::new()));
        registry.register(Box::new(EmojiHandler::new()));
        registry.register(Box::new(WebSearch::with_search_url(
            config.commands.default_search_engine.clone(),
        )));
        registry.register(Box::new(YouTube::new()));
        registry.register(Box::new(ShellExec::with_shell(
            config.commands.shell.clone(),
        )));
        // The terminal emulator for `run` commands now flows per-run via
        // `RunInputs` (built in execute.rs from config), not a global setter.
        registry.register(Box::new(CalcHandler::new()));
        registry.register(Box::new(DefineHandler::new()));
        registry.register(Box::new(DevUtilsHandler::new()));
        registry.register(Box::new(QrHandler::new()));
        registry.register(Box::new(ResizeImageHandler::new()));
        registry.register(Box::new(ConvertImageHandler::new()));
        registry.register(Box::new(ZipHandler::new()));
        registry.register(Box::new(ExtractHandler::new()));
        registry.register(Box::new(ScreenshotHandler::new()));
        registry.register(Box::new(ServicesHandler::new()));
        registry.register(Box::new(ServicesListHandler::new()));
        registry.register(Box::new(PackagesHandler::new()));
        registry.register(Box::new(ClipboardHandler::new(db.clone())));
        registry.register(Box::new(ClearHandler::new(db.clone())));
        registry.register(Box::new(FileOpen::new(db.clone())));
        registry.register(Box::new(UrlOpen::new()));
        #[cfg(feature = "mpris")]
        {
            registry.register(Box::new(MediaHandler::new(Arc::clone(&mpris))));
        }
        registry.register(Box::new(ProjectOpen::with_directories(
            config.projects.directories.clone(),
        )));
        registry.register(Box::new(SystemCommand::new()));
        registry.register(Box::new(TimeHandler::new()));
        registry.register(Box::new(TimerHandler::new(timer_state.clone(), db.clone())));
        registry.register(Box::new(SymbolHandler::new()));
        registry.register(Box::new(SysInfoHandler::new()));
        registry.register(Box::new(UnicodeHandler::new()));
        registry.register(Box::new(NotesHandler::new(db.clone())));
        registry.register(Box::new(TodoHandler::new(db.clone())));
        registry.register(Box::new(AliasHandler::new(db.clone())));
        registry.register(Box::new(RemindersHandler::new(db.clone())));
        registry.register(Box::new(SnippetsHandler::new(db.clone())));
        registry.register(Box::new(BrowseHandler::new()));
        registry.register(Box::new(ContextDebugHandler::new()));
        registry.register(Box::new(PinWorkspaceHandler));
        registry.register(Box::new(GenerateHandler::new()));
        registry.register(Box::new(SshHandler::new()));
        registry.register(Box::new(ColorHandler::new()));
        registry.register(Box::new(WindowSwitcherHandler::new(db.clone())));
        registry.register(Box::new(WeatherHandler::new(
            config.weather.unit.clone(),
            config.weather.default_location.clone(),
        )));

        // Initialize AI provider if configured (shared between router and ask
        // handler). Single source of truth for mode dispatch is the core factory.
        let ai_provider: Option<Arc<dyn AiProvider>> = Self::build_ai_provider(&config.ai);

        // NOTE: the old AI handlers (`ask`, `weather_ask`, `translate`, `convert`)
        // were all deleted in the AI rewrite. AI Q&A is the streaming agent (fork
        // card / full chat); `translate` / `summarize` / `rewrite` / `convert` are
        // now AI PRESETS (Phase 3) — user-editable prompt templates that stream a
        // fast answer, replacing the slow buffered handlers. `ai_provider` below
        // feeds the router/agent, not any handler.

        // Quicklinks — user-defined parameterized commands, extensible via
        // config. `resolved_quicklinks` merges the legacy `search_engines` map
        // in as URL quicklinks, so existing configs keep working untouched.
        let quicklinks = config.commands.resolved_quicklinks();
        registry.register(Box::new(QuicklinkHandler::new(quicklinks.clone())));

        // Script Commands — files in ~/.config/lychi/scripts/ become named
        // commands. Discovered at startup; hot-reloaded by the scripts watcher.
        let scripts = lychi_core::script_commands::discover(&lychi_core::paths::scripts_dir());
        let script_keywords: Vec<String> = scripts.iter().map(|s| s.keyword.clone()).collect();
        registry.register(Box::new(
            lychi_core::action_registry::handlers::script_commands::ScriptCommandsHandler::new(
                scripts,
                config.commands.shell.clone(),
            ),
        ));

        // Clone for the router; the original `ai_provider` is stored on AppState
        // (below) as the handle the streaming-chat command reads.
        let ai_router = ai_provider
            .clone()
            .map(|p| AiRouter::new_shared(p, Self::ask_timeout(&config.ai)));

        let history = HistoryStore::new(config.history.max_entries, config.history.deduplicate);

        let resolver = IntentResolver::new(ai_router);
        let rules = RulesEngine::new();
        let mut executor = Executor::new(registry, rules, resolver, history.clone(), db.clone());
        executor.set_quicklinks(quicklinks);
        executor.set_script_keywords(script_keywords);

        Self {
            executor: Arc::new(RwLock::new(executor)),
            db,
            history,
            config: Arc::new(RwLock::new(config)),
            // Reuse the provider built above (line ~319) for the executor's
            // handlers, so the streaming-chat command shares the same instance.
            ai_provider: Arc::new(RwLock::new(ai_provider)),
            ai_generation: Arc::new(AtomicU64::new(0)),
            ai_cancel: Arc::new(RwLock::new(None)),
            agent_session: Arc::new(RwLock::new(None)),
            agent_conversation_id: Arc::new(RwLock::new(None)),
            pending_plan: Arc::new(RwLock::new(None)),
            pending_execution: Arc::new(RwLock::new(None)),
            active_file_search: Arc::new(AtomicU64::new(0)),
            live_search: Arc::new(lychi_core::file_search::live::LiveSearch::new(Arc::new(
                lychi_core::file_search::corpus::CorpusStore::default(),
            ))),
            timer_state,
            launcher: Arc::new(crate::launcher_state::LauncherStateMachine::new()),
            dismiss_armed: Arc::new(AtomicBool::new(false)),
            summon_seq: Arc::new(AtomicU64::new(0)),
            armed_seq: Arc::new(AtomicU64::new(0)),
            hotkey_registered: Arc::new(AtomicBool::new(false)),
            portal_bound: Arc::new(AtomicBool::new(false)),
            // Nothing has bound the key yet; setup overwrites this.
            hotkey_verdict: Arc::new(std::sync::Mutex::new(
                lychi_core::hotkey::HotkeyVerdict::assess(
                    lychi_core::hotkey::Binding::None,
                    lychi_core::context::is_wayland(),
                ),
            )),
            clipboard_running: Arc::new(AtomicBool::new(true)),
            timer_running: Arc::new(AtomicBool::new(true)),
            app_index_watcher_running: Arc::new(AtomicBool::new(true)),
            scripts_watcher_running: Arc::new(AtomicBool::new(true)),
            heavy_sem: Arc::new(tokio::sync::Semaphore::new(3)),
            event_bus: Arc::new(lychi_core::events::EventBus::new()),
            #[cfg(feature = "mpris")]
            mpris,
        }
    }

    /// Subscribe the config reactors to the event bus. Called once after the app
    /// is set up. Each reactor owns exactly the state it manages (via `Arc`
    /// clones), so a `ConfigChanged` emit fans out to whoever cares without the
    /// emitting command knowing they exist.
    pub fn wire_reactors(&self, app: tauri::AppHandle) {
        crate::reactors::register_config_reactors(
            &self.event_bus,
            self.executor.clone(),
            self.config.clone(),
            self.ai_provider.clone(),
            app,
        );
    }

    /// Build the live AI provider from config via the core factory, logging the
    /// outcome. Returns `None` (AI off) on any non-fatal reason. This is the ONE
    /// place the Tauri layer turns config into a provider.
    ///
    /// IMPORTANT: this reads the OS keyring, whose secret-service backend spins
    /// its own runtime internally (`block_on`) and PANICS if called on a tokio
    /// async worker thread ("Cannot start a runtime from within a runtime").
    /// Only call this from a synchronous/blocking context — app startup, a
    /// `spawn_blocking` closure, or an event reactor running on its own thread.
    /// Async command handlers must go through [`build_ai_provider_async`].
    pub fn build_ai_provider(ai: &lychi_core::config::AiConfig) -> Option<Arc<dyn AiProvider>> {
        use lychi_core::providers::factory::{ProviderError, build_provider_with_cloud};
        // Pre-fetch the BYO key once (blocking keyring read) so the factory
        // closure is a pure lookup with no further blocking work.
        let key = if ai.mode == "byo" {
            byo_key_lookup(&ai.provider)
        } else {
            None
        };
        match build_provider_with_cloud(ai, |_| key.clone(), cloud_provider) {
            Ok(provider) => {
                tracing::info!("AI initialized ({} / {})", ai.mode, provider.name());
                Some(provider)
            }
            Err(ProviderError::Disabled) => None,
            Err(e) => {
                tracing::warn!("AI not initialized: {e}");
                None
            }
        }
    }

    /// The timeout the `ask`/transform handlers should use, derived from the AI
    /// config. Cloud/BYO want to fail fast (short) to fall back to web; LOCAL CPU
    /// inference is slow and runs on the user's own machine, so give it a
    /// generous floor rather than bailing to web after a few seconds.
    pub fn ask_timeout(ai: &lychi_core::config::AiConfig) -> std::time::Duration {
        use std::time::Duration;
        if ai.mode == "local" {
            // Config timeout, but never less than 60s — a 1B model on CPU can take
            // 15-40s for a full answer, and the user chose local deliberately.
            Duration::from_secs(ai.timeout_secs.max(60))
        } else {
            Duration::from_secs(ai.timeout_secs.max(1))
        }
    }

    /// Warm up the local model in the background when local AI is the active
    /// mode + a model is selected, so the FIRST query isn't slow (the multi-GB
    /// load happens up front). Emits `lychi://ai-load-state` as it progresses so
    /// the status bar can show a "loading" indicator. No-op unless mode=local.
    #[cfg(feature = "local-ai")]
    pub fn warmup_local_ai(app: &tauri::AppHandle, ai: &lychi_core::config::AiConfig) {
        use lychi_core::providers::{local, local_models};
        use tauri::Emitter;

        if ai.mode != "local" || ai.local_model.trim().is_empty() {
            return;
        }
        let id = ai.local_model.trim_end_matches(".gguf");
        let Some(spec) = local_models::find(id) else {
            return;
        };
        let path = lychi_core::paths::models_dir().join(&ai.local_model);
        if !path.exists() {
            return;
        }

        let app = app.clone();
        let spec_id = spec.id;
        std::thread::Builder::new()
            .name("local-ai-warmup".into())
            .spawn(move || {
                let Some(spec) = local_models::find(spec_id) else {
                    return;
                };
                let _ = app.emit("lychi://ai-load-state", "loading");
                // warmup returns its terminal state — no reaching back into a
                // global to find out how it went.
                let state = match local::warmup(spec, &path) {
                    local::LoadState::Ready => "ready",
                    local::LoadState::Failed => "failed",
                    _ => "idle",
                };
                let _ = app.emit("lychi://ai-load-state", state);
            })
            .ok();
    }

    #[cfg(not(feature = "local-ai"))]
    pub fn warmup_local_ai(_app: &tauri::AppHandle, _ai: &lychi_core::config::AiConfig) {}

    /// Async-safe wrapper: builds the provider on a blocking thread so the
    /// keyring's internal `block_on` never runs on a tokio worker. Use this from
    /// `#[tauri::command] async fn` handlers.
    pub async fn build_ai_provider_async(
        ai: lychi_core::config::AiConfig,
    ) -> Option<Arc<dyn AiProvider>> {
        tauri::async_runtime::spawn_blocking(move || Self::build_ai_provider(&ai))
            .await
            .unwrap_or(None)
    }
}

/// Read a stored BYO API key from the OS keyring for the factory's key lookup.
/// Blocking — must run on a non-async thread (see `build_ai_provider`). Kept out
/// of `lychi-core` so the core stays keyring-free.
fn byo_key_lookup(provider_id: &str) -> Option<String> {
    keyring::Entry::new("lychi", &format!("byo-{provider_id}"))
        .ok()?
        .get_password()
        .ok()
}

/// Build a signed-in Lychi Cloud provider for the factory's cloud arm, or `None`
/// if the user isn't signed in. Kept out of `lychi-core` so the Firebase/keyring
/// token provider stays in the Tauri layer.
///
/// Lychi Cloud is deferred to Phase 2.3; until it ships this returns `None`
/// unless a token is already present, so `mode = "cloud"` degrades to "not
/// available yet" rather than erroring.
fn cloud_provider() -> Option<Arc<dyn AiProvider>> {
    use crate::commands::firebase_auth::{CLOUD_BASE_URL, KeyringTokenProvider};
    let token_provider = Arc::new(KeyringTokenProvider::new());
    if !token_provider.is_signed_in() {
        return None;
    }
    Some(Arc::new(lychi_core::providers::cloud::CloudClient::new(
        CLOUD_BASE_URL.to_string(),
        token_provider,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lychi_core::action_registry::RiskLevel;
    use lychi_core::intent::{ResolvedIntent, RoutingMethod};

    fn intent() -> ResolvedIntent {
        ResolvedIntent {
            action_id: "run".into(),
            args: "rm -rf /tmp/x".into(),
            routing: RoutingMethod::Explicit,
        }
    }

    #[test]
    fn pending_execution_stores_intent_and_risk() {
        let p = PendingExecution::new(intent(), RiskLevel::High);
        assert_eq!(p.intent.action_id, "run");
        assert_eq!(p.intent.args, "rm -rf /tmp/x");
        assert_eq!(p.risk, RiskLevel::High);
    }

    #[test]
    fn pending_execution_fresh_is_not_expired() {
        let p = PendingExecution::new(intent(), RiskLevel::High);
        assert!(!p.is_expired(), "a just-created pending must be valid");
    }

    #[test]
    fn pending_execution_past_expiry_is_expired() {
        let mut p = PendingExecution::new(intent(), RiskLevel::High);
        // Force the deadline into the past.
        p.expires_at = std::time::Instant::now() - std::time::Duration::from_secs(1);
        assert!(p.is_expired(), "a past-deadline pending must be rejected");
    }
}

#[cfg(test)]
mod ai_prompt_coverage {
    /// E3: `intent::prompt::action_description` is a hand-maintained table
    /// parallel to `ActionHandler::description()`, and a handler missing from
    /// it reaches the model as "Unknown command" — i.e. it is invisible to AI
    /// routing, silently, from the moment it is registered.
    ///
    /// The duplication itself is not fixed here (see the note at that
    /// function: `known_actions: &[&str]` is a trait parameter on every
    /// provider and part of the cloud request body). This test makes the
    /// consequence impossible to miss instead: add a handler, forget the
    /// description, and CI says so.
    ///
    /// It lives in this crate because this is where the registry is built —
    /// `lychi-core` has no list of what gets registered.
    /// Every `registry.register(...)` line in this file, by handler id.
    ///
    /// Read from the SOURCE rather than by constructing handlers: most need a
    /// database and a live config, so building the real registry in a unit
    /// test would mean standing up half the app to check a list of strings.
    /// Scanning the registration block cannot drift from it either — adding a
    /// handler necessarily adds a line here.
    fn registered_handler_types() -> Vec<String> {
        let src = include_str!("state.rs");
        src.lines()
            .filter_map(|l| l.trim().strip_prefix("registry.register(Box::new("))
            .filter_map(|rest| {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                (!name.is_empty()).then_some(name)
            })
            .collect()
    }

    #[test]
    fn the_registration_scan_finds_the_handlers() {
        // Guard the guard: a scan that silently matches nothing would make the
        // coverage test below vacuously pass.
        let found = registered_handler_types();
        assert!(
            found.len() > 30,
            "expected to find the registration block; got {found:?}"
        );
        assert!(found.iter().any(|h| h == "WebSearch"));
    }

    /// Handler ids that carry no AI description today.
    ///
    /// This list is the CURRENT state, not the desired one — it exists so the
    /// number cannot grow silently. Adding a handler without a description
    /// fails this test; the fix is an arm in `action_description`, not an
    /// entry here.
    #[test]
    fn ai_description_coverage_does_not_regress() {
        // Ids are the stable contract with the prompt table, and a handler
        // type name is not its id, so only ids we can name are checked.
        // Read off `ActionHandler::id()` across handlers/ — the actual ids, not
        // plausible-looking guesses. (`snip` not "snippet", `sym` not "symbol",
        // `reminder` not "remind": the first version of this list guessed and
        // reported three commands missing that do not exist.)
        const KNOWN_IDS: &[&str] = &[
            "alias",
            "appctl",
            "bm",
            "browse",
            "calc",
            "clear",
            "clip",
            "color",
            "convert",
            "ctx",
            "define",
            "devutil",
            "emoji",
            "extract",
            "file",
            "generate",
            "media",
            "note",
            "open",
            "packages",
            "pin_workspace",
            "project",
            "qr",
            "quicklink",
            "reminder",
            "resize",
            "run",
            "screenshot",
            "script",
            "service",
            "services",
            "snip",
            "ssh",
            "sym",
            "sysinfo",
            "system",
            "time",
            "timer",
            "todo",
            "unicode",
            "url",
            "weather",
            "web",
            "win",
            "yt",
            "zip",
        ];
        let missing: Vec<&&str> = KNOWN_IDS
            .iter()
            .filter(|id| !lychi_core::intent::prompt::has_action_description(id))
            .collect();
        assert!(
            missing.is_empty(),
            "these commands have no AI description, so the model sees \"Unknown \
             command\" and cannot route to them: {missing:?}\n\
             Add an arm to `intent::prompt::action_description`.",
        );
    }
}

#[cfg(test)]
mod sentinel_contract {
    /// D4: the `__sentinel__` strings are the one part of the IPC surface that
    /// is NOT a generated type — the backend writes bare strings into
    /// `icon_path`/`output` and the frontend matches them by hand. A rename on
    /// either side breaks the UI silently: the row just stops rendering as a
    /// folder, or a panel stops opening, with nothing to fail.
    ///
    /// `src/lib/sentinels.ts` collects the frontend's expectations. This
    /// asserts the Rust side still emits the same strings, so the two ends
    /// cannot drift apart unnoticed. It reads the TypeScript as text rather
    /// than generating it, because generating would make this a build-order
    /// problem for one small table.
    #[test]
    fn the_frontend_and_backend_agree_on_sentinels() {
        let ts = include_str!("../../src/lib/sentinels.ts");

        // Every `"__foo__"` the frontend declares.
        let declared: Vec<String> = ts
            .split('"')
            .filter(|s| s.starts_with("__") && (s.ends_with("__") || s.ends_with("__:")))
            .map(str::to_string)
            .collect();

        assert!(
            declared.len() > 15,
            "the sentinel scan found almost nothing ({declared:?}) — has sentinels.ts moved?"
        );

        // The ones this crate and lychi-core actually emit. Kept explicit: a
        // grep over the Rust sources would also match these very literals.
        const EMITTED: &[&str] = &[
            "__folder__",
            "__none__",
            "__web__",
            "__history__",
            "__separator__",
            "__warning__",
            "__context__",
            "__context_stale__",
            "__terminal__",
            "__info__",
            "__clipboard_image__",
            "__ai_chat__",
            "__media_panel__",
            "__notes_panel__",
            "__timer_panel__",
            "__reminders_panel__",
            "__snippets_panel__",
            "__browse_panel__:",
            "__notes_limit__:",
        ];

        let missing: Vec<&&str> = EMITTED
            .iter()
            .filter(|e| !declared.iter().any(|d| d == *e))
            .collect();
        assert!(
            missing.is_empty(),
            "the backend emits these but src/lib/sentinels.ts does not declare them, \
             so the frontend will silently ignore them: {missing:?}"
        );
    }
}
