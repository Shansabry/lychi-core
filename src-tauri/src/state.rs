use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};
use tokio::sync::RwLock;

use redb::Database;

use lychi_core::action_registry::handlers::aliases::AliasHandler;
use lychi_core::action_registry::handlers::app_control::AppControlHandler;
use lychi_core::action_registry::handlers::app_launcher::AppLauncher;
use lychi_core::action_registry::handlers::ask::AskHandler;
use lychi_core::action_registry::handlers::bookmarks::BookmarkHandler;
use lychi_core::action_registry::handlers::browse::BrowseHandler;
use lychi_core::action_registry::handlers::calc::CalcHandler;
use lychi_core::action_registry::handlers::clipboard::ClipboardHandler;
use lychi_core::action_registry::handlers::context_debug::ContextDebugHandler;
use lychi_core::action_registry::handlers::emoji::EmojiHandler;
use lychi_core::action_registry::handlers::file_open::FileOpen;
#[cfg(feature = "mpris")]
use lychi_core::action_registry::handlers::media::MediaHandler;
use lychi_core::action_registry::handlers::notes::{NotesHandler, TodoHandler};
use lychi_core::action_registry::handlers::pin_workspace::PinWorkspaceHandler;
use lychi_core::action_registry::handlers::project_open::ProjectOpen;
use lychi_core::action_registry::handlers::reminders::RemindersHandler;
use lychi_core::action_registry::handlers::shell_exec::ShellExec;
use lychi_core::action_registry::handlers::snippets::SnippetsHandler;
use lychi_core::action_registry::handlers::symbol::SymbolHandler;
use lychi_core::action_registry::handlers::sysinfo::SysInfoHandler;
use lychi_core::action_registry::handlers::system::SystemCommand;
use lychi_core::action_registry::handlers::time::TimeHandler;
use lychi_core::action_registry::handlers::timer::{TimerHandler, TimerState};
use lychi_core::action_registry::handlers::unicode::UnicodeHandler;
use lychi_core::action_registry::handlers::url_open::UrlOpen;
use lychi_core::action_registry::handlers::weather::WeatherHandler;
use lychi_core::action_registry::handlers::weather_ask::WeatherAskHandler;
use lychi_core::action_registry::handlers::web_search::WebSearch;
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
use lychi_core::providers::byo::{BYOClient, BYOProvider};
use lychi_core::providers::{AgentPlan, AiProvider};
use lychi_core::rules::RulesEngine;

pub struct AppState {
    pub executor: Arc<RwLock<Executor>>,
    pub db: Arc<Database>,
    pub history: HistoryStore,
    pub config: Arc<RwLock<Config>>,
    pub pending_plan: Arc<RwLock<Option<AgentPlan>>>,
    pub active_file_search: Arc<AtomicU64>,
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
                // Apply DB settings over TOML (DB wins for syncable fields)
                lychi_core::config::db::apply_to_config(&db_settings, &mut config);
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

        let timer_state = lychi_core::action_registry::handlers::timer::new_timer_state();

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
        // Set the terminal emulator for `run` commands that open a real terminal window
        lychi_core::action_registry::handlers::shell_exec::set_terminal(Some(
            config.commands.terminal.clone(),
        ));
        registry.register(Box::new(CalcHandler::new()));
        registry.register(Box::new(ClipboardHandler::new(db.clone())));
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
        registry.register(Box::new(TimerHandler::new(timer_state.clone())));
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
        let weather_handler = Arc::new(WeatherHandler::new(
            config.weather.unit.clone(),
            config.weather.default_location.clone(),
        ));
        registry.register(Box::new(weather_handler.clone()));

        // Initialize AI provider if configured (shared between router and ask handler)
        let ai_provider: Option<Arc<dyn AiProvider>> = if config.ai.mode == "byo" {
            match Self::init_byo_client(&config.ai.provider, &config.ai.model, config.ai.max_tokens)
            {
                Ok(client) => {
                    tracing::info!("AI initialized (BYO: {})", config.ai.provider);
                    Some(client)
                }
                Err(e) => {
                    tracing::warn!("Failed to initialize AI: {e}");
                    None
                }
            }
        } else {
            None
        };

        registry.register(Box::new(AskHandler::new(
            ai_provider.clone(),
            config.commands.default_search_engine.clone(),
        )));
        registry.register(Box::new(WeatherAskHandler::new(
            weather_handler,
            ai_provider.clone(),
        )));

        let ai_router = ai_provider.map(|p| {
            AiRouter::new_shared(p, std::time::Duration::from_secs(config.ai.timeout_secs))
        });

        let history = HistoryStore::new(config.history.max_entries, config.history.deduplicate);

        let resolver = IntentResolver::new(ai_router);
        let rules = RulesEngine::new();
        let executor = Executor::new(registry, rules, resolver, history.clone(), db.clone());

        Self {
            executor: Arc::new(RwLock::new(executor)),
            db,
            history,
            config: Arc::new(RwLock::new(config)),
            pending_plan: Arc::new(RwLock::new(None)),
            active_file_search: Arc::new(AtomicU64::new(0)),
            timer_state,
            dismiss_armed: Arc::new(AtomicBool::new(false)),
            summon_seq: Arc::new(AtomicU64::new(0)),
            clipboard_running: Arc::new(AtomicBool::new(true)),
            timer_running: Arc::new(AtomicBool::new(true)),
            app_index_watcher_running: Arc::new(AtomicBool::new(true)),
            heavy_sem: Arc::new(tokio::sync::Semaphore::new(3)),
            #[cfg(feature = "mpris")]
            mpris,
        }
    }

    fn init_byo_client(
        provider_name: &str,
        model: &str,
        max_tokens: u32,
    ) -> Result<Arc<dyn AiProvider>, String> {
        let provider: BYOProvider = provider_name
            .parse()
            .map_err(|e: lychi_core::error::LychiError| e.to_string())?;

        let entry = keyring::Entry::new("lychi", &format!("byo-{provider_name}"))
            .map_err(|e| format!("Keyring error: {e}"))?;
        let api_key = entry
            .get_password()
            .map_err(|e| format!("No API key stored for {provider_name}: {e}"))?;

        let client = BYOClient::new(provider, model.to_string(), api_key, max_tokens);
        Ok(Arc::new(client))
    }
}
