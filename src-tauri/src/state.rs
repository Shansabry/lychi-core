use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};
use tokio::sync::RwLock;

use redb::Database;

use lychi_core::action_registry::handlers::aliases::AliasHandler;
use lychi_core::action_registry::handlers::app_control::AppControlHandler;
use lychi_core::action_registry::handlers::app_launcher::AppLauncher;
use lychi_core::action_registry::handlers::ask::AskHandler;
use lychi_core::action_registry::handlers::bang::BangHandler;
use lychi_core::action_registry::handlers::bookmarks::BookmarkHandler;
use lychi_core::action_registry::handlers::browse::BrowseHandler;
use lychi_core::action_registry::handlers::calc::CalcHandler;
use lychi_core::action_registry::handlers::clear::ClearHandler;
use lychi_core::action_registry::handlers::clipboard::ClipboardHandler;
use lychi_core::action_registry::handlers::clipboard_transform::ClipboardTransformHandler;
use lychi_core::action_registry::handlers::color::ColorHandler;
use lychi_core::action_registry::handlers::context_debug::ContextDebugHandler;
use lychi_core::action_registry::handlers::define::DefineHandler;
use lychi_core::action_registry::handlers::dev_utils::DevUtilsHandler;
use lychi_core::action_registry::handlers::emoji::EmojiHandler;
use lychi_core::action_registry::handlers::file_open::FileOpen;
use lychi_core::action_registry::handlers::generate::GenerateHandler;
#[cfg(feature = "mpris")]
use lychi_core::action_registry::handlers::media::MediaHandler;
use lychi_core::action_registry::handlers::notes::{NotesHandler, TodoHandler};
use lychi_core::action_registry::handlers::packages::PackagesHandler;
use lychi_core::action_registry::handlers::pin_workspace::PinWorkspaceHandler;
use lychi_core::action_registry::handlers::project_open::ProjectOpen;
use lychi_core::action_registry::handlers::qr::QrHandler;
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
use lychi_core::action_registry::handlers::translate::TranslateHandler;
use lychi_core::action_registry::handlers::unicode::UnicodeHandler;
use lychi_core::action_registry::handlers::url_open::UrlOpen;
use lychi_core::action_registry::handlers::weather::WeatherHandler;
use lychi_core::action_registry::handlers::weather_ask::WeatherAskHandler;
use lychi_core::action_registry::handlers::web_search::WebSearch;
use lychi_core::action_registry::handlers::window_switcher::WindowSwitcherHandler;
use lychi_core::action_registry::handlers::youtube::YouTube;
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

pub struct AppState {
    pub executor: Arc<RwLock<Executor>>,
    pub db: Arc<Database>,
    pub history: HistoryStore,
    pub config: Arc<RwLock<Config>>,
    pub pending_plan: Arc<RwLock<Option<AgentPlan>>>,
    pub active_file_search: Arc<AtomicU64>,
    /// Persistent per-scope fuzzy file indexes (nucleo engines). Built lazily on
    /// first search of a scope, reused across keystrokes for instant matching.
    pub file_index: Arc<lychi_core::file_search::FileIndexStore>,
    pub timer_state: TimerState,
    /// Dismiss-on-blur armed flag. Set true when the user interacts with the
    /// launcher (key press, pointer click). Reset on hide. Focus-out only
    /// dismisses if this is true, avoiding KWin's automatic focus revoke
    /// (~9ms after show) from triggering a false dismiss.
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
    pub hotkey_registered: Arc<AtomicBool>,
    /// Whether the hotkey is bound through the XDG GlobalShortcuts portal —
    /// compositor-level and therefore reliable even on Wayland.
    pub portal_bound: Arc<AtomicBool>,
    /// Shutdown signal for the clipboard monitor OS thread. Set to false on exit
    /// to stop the monitor before D-Bus/arboard teardown begins.
    pub clipboard_running: Arc<AtomicBool>,
    /// Shutdown signal for the timer/reminder monitor OS thread.
    pub timer_running: Arc<AtomicBool>,
    /// Shutdown signal for the app-index filesystem watcher OS thread.
    pub app_index_watcher_running: Arc<AtomicBool>,
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
    pub fn new() -> Self {
        let mut config = Config::load_or_default(&paths::config_file());

        // Open redb database (creates file if missing)
        let db_path = paths::db_file();
        let db = lychi_core::db::open_database(&db_path).unwrap_or_else(|e| {
            panic!("Failed to open database: {e}");
        });
        let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
        tracing::info!(
            "Database opened: {} ({:.1} KB)",
            db_path.display(),
            db_size as f64 / 1024.0
        );

        // Seed settings from TOML on first launch (if settings table is empty)
        if let Err(e) = lychi_core::config::db::seed_from_config(&db, &config) {
            tracing::error!("Failed to seed settings: {e}");
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
        let weather_handler = Arc::new(WeatherHandler::new(
            config.weather.unit.clone(),
            config.weather.default_location.clone(),
        ));
        registry.register(Box::new(weather_handler.clone()));

        // Initialize AI provider if configured (shared between router and ask
        // handler). Single source of truth for mode dispatch is the core factory.
        let ai_provider: Option<Arc<dyn AiProvider>> = Self::build_ai_provider(&config.ai);

        registry.register(Box::new(AskHandler::new(
            ai_provider.clone(),
            config.commands.default_search_engine.clone(),
        )));
        registry.register(Box::new(WeatherAskHandler::new(
            weather_handler,
            ai_provider.clone(),
        )));
        registry.register(Box::new(ClipboardTransformHandler::new(
            ai_provider.clone(),
        )));
        registry.register(Box::new(TranslateHandler::new(ai_provider.clone())));

        // Custom search-engine shortcuts ("bangs") — user-extensible via config.
        let bang_keywords: Vec<String> = config.commands.search_engines.keys().cloned().collect();
        registry.register(Box::new(BangHandler::new(
            config.commands.search_engines.clone(),
        )));

        let ai_router = ai_provider.map(|p| {
            AiRouter::new_shared(p, std::time::Duration::from_secs(config.ai.timeout_secs))
        });

        let history = HistoryStore::new(config.history.max_entries, config.history.deduplicate);

        let resolver = IntentResolver::new(ai_router);
        let rules = RulesEngine::new();
        let mut executor = Executor::new(registry, rules, resolver, history.clone(), db.clone());
        executor.set_bang_keywords(bang_keywords);

        Self {
            executor: Arc::new(RwLock::new(executor)),
            db,
            history,
            config: Arc::new(RwLock::new(config)),
            pending_plan: Arc::new(RwLock::new(None)),
            active_file_search: Arc::new(AtomicU64::new(0)),
            file_index: Arc::new(lychi_core::file_search::FileIndexStore::default()),
            timer_state,
            dismiss_armed: Arc::new(AtomicBool::new(false)),
            summon_seq: Arc::new(AtomicU64::new(0)),
            armed_seq: Arc::new(AtomicU64::new(0)),
            hotkey_registered: Arc::new(AtomicBool::new(false)),
            portal_bound: Arc::new(AtomicBool::new(false)),
            clipboard_running: Arc::new(AtomicBool::new(true)),
            timer_running: Arc::new(AtomicBool::new(true)),
            app_index_watcher_running: Arc::new(AtomicBool::new(true)),
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
    pub fn wire_reactors(&self) {
        crate::reactors::register_config_reactors(
            &self.event_bus,
            self.executor.clone(),
            self.config.clone(),
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
