use std::sync::Arc;
use tokio::sync::RwLock;

use lychi_core::ai::agent::AgentPlan;
use lychi_core::ai::byo::{BYOClient, BYOProvider};
use lychi_core::ai::router::AiRouter;
use lychi_core::command::app_launcher::AppLauncher;
use lychi_core::command::calc::CalcHandler;
use lychi_core::command::file_open::FileOpen;
use lychi_core::command::project_open::ProjectOpen;
use lychi_core::command::registry::CommandRegistry;
use lychi_core::command::shell_exec::ShellExec;
use lychi_core::command::spotify::{MediaHandler, SpotifyHandler};
use lychi_core::command::system::SystemCommand;
use lychi_core::command::url_open::UrlOpen;
use lychi_core::command::web_search::WebSearch;
use lychi_core::command::youtube::YouTube;
use lychi_core::config::Config;
use lychi_core::history::HistoryStore;
use lychi_core::mpris::MprisManager;
use lychi_core::paths;

pub struct AppState {
    pub registry: Arc<RwLock<CommandRegistry>>,
    pub history: Arc<RwLock<HistoryStore>>,
    pub config: Arc<RwLock<Config>>,
    pub pending_plan: Arc<RwLock<Option<AgentPlan>>>,
    pub mpris: Arc<RwLock<Option<MprisManager>>>,
}

impl AppState {
    pub fn new() -> Self {
        let config = Config::load_or_default(&paths::config_file());

        let mut registry = CommandRegistry::new();
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
        registry.register(Box::new(SpotifyHandler::new()));
        registry.register(Box::new(MediaHandler::new()));
        registry.register(Box::new(ProjectOpen::with_directories(
            config.projects.directories.clone(),
        )));
        registry.register(Box::new(SystemCommand::new()));

        // Initialize AI router if configured
        if config.ai.mode == "byo" {
            match Self::init_byo_router(&config.ai.provider, &config.ai.model) {
                Ok(router) => {
                    registry.set_ai_router(router);
                    tracing::info!("AI router initialized (BYO: {})", config.ai.provider);
                }
                Err(e) => {
                    tracing::warn!("Failed to initialize AI router: {e}");
                }
            }
        }

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
            registry: Arc::new(RwLock::new(registry)),
            history: Arc::new(RwLock::new(history)),
            config: Arc::new(RwLock::new(config)),
            pending_plan: Arc::new(RwLock::new(None)),
            mpris: Arc::new(RwLock::new(None)),
        }
    }

    fn init_byo_router(provider_name: &str, model: &str) -> Result<AiRouter, String> {
        let provider: BYOProvider = provider_name.parse().map_err(|e: lychi_core::error::LychiError| e.to_string())?;

        let entry = keyring::Entry::new("lychi", &format!("byo-{provider_name}"))
            .map_err(|e| format!("Keyring error: {e}"))?;
        let api_key = entry
            .get_password()
            .map_err(|e| format!("No API key stored for {provider_name}: {e}"))?;

        let client = BYOClient::new(provider, model.to_string(), api_key);
        Ok(AiRouter::new(Box::new(client)))
    }
}
