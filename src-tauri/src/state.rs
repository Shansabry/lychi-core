use std::sync::Arc;
use tokio::sync::RwLock;

use lychi_core::action_registry::handlers::app_launcher::AppLauncher;
use lychi_core::action_registry::handlers::ask::AskHandler;
use lychi_core::action_registry::handlers::browse::BrowseHandler;
use lychi_core::action_registry::handlers::calc::CalcHandler;
use lychi_core::action_registry::handlers::file_open::FileOpen;
use lychi_core::action_registry::handlers::notes::{NotesHandler, TodoHandler};
use lychi_core::action_registry::handlers::project_open::ProjectOpen;
use lychi_core::action_registry::handlers::shell_exec::ShellExec;
#[cfg(feature = "mpris")]
use lychi_core::action_registry::handlers::spotify::{MediaHandler, SpotifyHandler};
use lychi_core::action_registry::handlers::sysinfo::SysInfoHandler;
use lychi_core::action_registry::handlers::system::SystemCommand;
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
use lychi_core::notes::store::NotesStore;
use lychi_core::paths;
use lychi_core::providers::byo::{BYOClient, BYOProvider};
use lychi_core::providers::{AgentPlan, AiProvider};
use lychi_core::rules::RulesEngine;

pub struct AppState {
    pub executor: Arc<RwLock<Executor>>,
    pub history: Arc<RwLock<HistoryStore>>,
    pub config: Arc<RwLock<Config>>,
    pub notes: Arc<RwLock<NotesStore>>,
    pub pending_plan: Arc<RwLock<Option<AgentPlan>>>,
    #[cfg(feature = "mpris")]
    pub mpris: Arc<RwLock<Option<MprisManager>>>,
}

impl AppState {
    pub fn new() -> Self {
        let config = Config::load_or_default(&paths::config_file());

        // Load notes store early so handlers can share the reference
        let notes = Arc::new(RwLock::new(
            NotesStore::load_or_create(&paths::notes_file()).unwrap_or_else(|e| {
                tracing::error!("Failed to load notes: {e} — starting fresh");
                NotesStore::load_or_create(&paths::notes_file()).unwrap_or_else(|_| {
                    NotesStore::load_or_create(
                        &std::env::temp_dir().join("lychi-notes-fallback.json"),
                    )
                    .expect("Failed to create fallback notes store")
                })
            }),
        ));

        let mut registry = ActionRegistry::new();
        registry.register(Box::new(AppLauncher::new()));
        registry.register(Box::new(WebSearch::with_search_url(
            config.commands.default_search_engine.clone(),
        )));
        registry.register(Box::new(YouTube::new()));
        registry.register(Box::new(ShellExec::with_shell(
            config.commands.shell.clone(),
        )));
        registry.register(Box::new(CalcHandler::new()));
        registry.register(Box::new(FileOpen::new()));
        registry.register(Box::new(UrlOpen::new()));
        #[cfg(feature = "mpris")]
        {
            registry.register(Box::new(SpotifyHandler::new()));
            registry.register(Box::new(MediaHandler::new()));
        }
        registry.register(Box::new(ProjectOpen::with_directories(
            config.projects.directories.clone(),
        )));
        registry.register(Box::new(SystemCommand::new()));
        registry.register(Box::new(SysInfoHandler::new()));
        registry.register(Box::new(NotesHandler::new(notes.clone())));
        registry.register(Box::new(TodoHandler::new(notes.clone())));
        registry.register(Box::new(BrowseHandler::new()));
        let weather_handler = Arc::new(WeatherHandler::new(
            config.weather.unit.clone(),
            config.weather.default_location.clone(),
        ));
        registry.register(Box::new(weather_handler.clone()));

        // Initialize AI provider if configured (shared between router and ask handler)
        let ai_provider: Option<Arc<dyn AiProvider>> = if config.ai.mode == "byo" {
            match Self::init_byo_client(&config.ai.provider, &config.ai.model) {
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

        let ai_router = ai_provider.map(AiRouter::new_shared);

        let resolver = IntentResolver::new(ai_router);
        let rules = RulesEngine::new();
        let executor = Executor::new(registry, rules, resolver);

        let history = HistoryStore::load_or_create(
            &paths::history_file(),
            config.history.max_entries,
            config.history.deduplicate,
        )
        .unwrap_or_else(|e| {
            tracing::error!("Failed to load history: {e}");
            HistoryStore::load_or_create(&paths::history_file(), 500, true).unwrap_or_else(|e2| {
                tracing::error!("Failed to create history store: {e2} — using in-memory only");
                HistoryStore::empty(paths::history_file(), 500, true)
            })
        });

        Self {
            executor: Arc::new(RwLock::new(executor)),
            history: Arc::new(RwLock::new(history)),
            notes,
            config: Arc::new(RwLock::new(config)),
            pending_plan: Arc::new(RwLock::new(None)),
            #[cfg(feature = "mpris")]
            mpris: Arc::new(RwLock::new(None)),
        }
    }

    fn init_byo_client(provider_name: &str, model: &str) -> Result<Arc<dyn AiProvider>, String> {
        let provider: BYOProvider = provider_name
            .parse()
            .map_err(|e: lychi_core::error::LychiError| e.to_string())?;

        let entry = keyring::Entry::new("lychi", &format!("byo-{provider_name}"))
            .map_err(|e| format!("Keyring error: {e}"))?;
        let api_key = entry
            .get_password()
            .map_err(|e| format!("No API key stored for {provider_name}: {e}"))?;

        let client = BYOClient::new(provider, model.to_string(), api_key);
        Ok(Arc::new(client))
    }
}
