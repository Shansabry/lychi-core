//! Event reactors — the app-layer subscribers that react to `DomainEvent`s.
//!
//! These live in the Tauri crate (not `lychi-core`) because the reactions touch
//! app-owned state — the executor's registry and the live `Config` — which are
//! `AppState` fields. `lychi-core` defines the event vocabulary; the app wires the
//! reactions. That keeps the core Tauri-free while still letting settings changes
//! fan out cleanly.
//!
//! Before this, `save_commands_config` / `save_projects_config` imperatively poked
//! five subsystems (re-register the shell handler, refresh bang keywords, update
//! IDE markers, set the pinned workspace, re-register the project handler). Now the
//! command emits one `ConfigChanged { section }` and these reactors each handle
//! their slice — the command no longer knows they exist.
//!
//! Reactors are synchronous and acquire the executor/config locks with
//! `blocking_*`. This is safe because the emit happens from the config command
//! *after* it has released its own write locks (see the migration in
//! `commands/config.rs`), so there is no self-contention, and config saves are
//! rare and user-initiated.

use std::sync::Arc;

use tauri::async_runtime::RwLock;

use lychi_core::config::Config;
use lychi_core::events::{ConfigSection, DomainEvent, EventBus, EventHandler};
use lychi_core::executor::Executor;

/// Reacts to `ConfigChanged { Commands }`: re-derives the shell handler, terminal
/// setting, and bang (search-engine) routing from the live config.
struct CommandsReactor {
    executor: Arc<RwLock<Executor>>,
    config: Arc<RwLock<Config>>,
}

impl EventHandler for CommandsReactor {
    fn handle(&self, event: &DomainEvent) {
        let DomainEvent::ConfigChanged {
            section: ConfigSection::Commands,
        } = event
        else {
            return;
        };

        let commands = self.config.blocking_read().commands.clone();

        // The shell env cache is invalidated on config change; the terminal
        // emulator now flows per-run via `RunInputs`, not a global setter.
        lychi_core::action_registry::handlers::shell_exec::invalidate_shell_env();

        let mut executor = self.executor.blocking_write();
        // Re-register the shell handler with the current shell.
        executor.registry.register(Box::new(
            lychi_core::action_registry::handlers::shell_exec::ShellExec::with_shell(
                commands.shell.clone(),
            ),
        ));
        // Re-register the bang handler + refresh routing keywords so search-engine
        // edits go live without a restart.
        let keywords: Vec<String> = commands.search_engines.keys().cloned().collect();
        executor.registry.register(Box::new(
            lychi_core::action_registry::handlers::bang::BangHandler::new(
                commands.search_engines.clone(),
            ),
        ));
        executor.set_bang_keywords(keywords);
        tracing::info!("[reactor] commands config applied (shell + bangs)");
    }
}

/// Reacts to `ConfigChanged { Projects }`: updates IDE markers, the pinned
/// workspace, and re-registers the project handler with the current directories.
struct ProjectsReactor {
    executor: Arc<RwLock<Executor>>,
    config: Arc<RwLock<Config>>,
}

impl EventHandler for ProjectsReactor {
    fn handle(&self, event: &DomainEvent) {
        let DomainEvent::ConfigChanged {
            section: ConfigSection::Projects,
        } = event
        else {
            return;
        };

        let config = self.config.blocking_read();
        let projects = config.projects.clone();

        // Apply all context-detection config atomically (markers + pin, plus the
        // terminal/IDE extras from `commands` — so any config change re-applies
        // the full set through one owned entry point).
        lychi_core::context::config::ContextConfig {
            extra_terminals: config.commands.extra_terminals.clone(),
            extra_ides: config.commands.extra_ides.clone(),
            extra_strong_markers: projects.extra_strong_markers.clone(),
            extra_soft_markers: projects.extra_soft_markers.clone(),
            pinned_workspace: projects.pinned_workspace.clone(),
        }
        .apply();
        drop(config);

        let mut executor = self.executor.blocking_write();
        executor.registry.register(Box::new(
            lychi_core::action_registry::handlers::project_open::ProjectOpen::with_directories(
                projects.directories.clone(),
            ),
        ));
        tracing::info!("[reactor] projects config applied (markers + pin + dirs)");
    }
}

/// Reacts to `ConfigChanged { Ai }`: rebuilds the AI provider from the live
/// config via the core factory, hot-swaps the intent router, and re-registers
/// the four AI-dependent handlers (ask, weather-ask, clipboard-transform,
/// translate) so switching provider/model/mode goes live without a restart.
///
/// This is the runtime half of the unified AI switcher: `save_ai_config` writes
/// and emits, this applies. The build/keyring logic lives in `AppState`
/// via `build_ai_provider` so the keyring stays out of `lychi-core`.
struct AiReactor {
    executor: Arc<RwLock<Executor>>,
    config: Arc<RwLock<Config>>,
    /// The AppState handle the streaming-chat command reads. Written here in the
    /// same pass that rebuilds the executor's AI handlers, so a mode switch
    /// updates both paths atomically-enough (the command reads it per request).
    ai_provider: Arc<RwLock<Option<Arc<dyn lychi_core::providers::AiProvider>>>>,
    /// App handle used to push AI availability to the frontend. The FE mirrors
    /// this into its `aiEnabled` flag so submit-routing (NL/`ask` → agent vs.
    /// web) stays in sync with the live provider, event-driven, no polling.
    app: tauri::AppHandle,
}

impl EventHandler for AiReactor {
    fn handle(&self, event: &DomainEvent) {
        let DomainEvent::ConfigChanged {
            section: ConfigSection::Ai,
        } = event
        else {
            return;
        };

        use lychi_core::intent::ai_router::AiRouter;

        let ai = self.config.blocking_read().ai.clone();

        // Single source of truth for provider construction.
        let provider = crate::state::AppState::build_ai_provider(&ai);

        // Update the streaming-chat command's handle. This is what the agent
        // (fork card / full chat / presets) uses — the primary and only AI path.
        // The old AI handlers (ask/weather_ask/translate/convert) were all
        // deleted; presets replace the text transforms, so there's nothing to
        // re-register here on a provider hot-swap.
        *self.ai_provider.blocking_write() = provider.clone();

        let mut executor = self.executor.blocking_write();

        // Swap (or clear) the intent router.
        let available = provider.is_some();
        match provider {
            Some(p) => {
                let router = AiRouter::new_shared(p, crate::state::AppState::ask_timeout(&ai));
                executor.resolver.set_ai_router(router);
                tracing::info!(
                    "[reactor] ai config applied (provider: {}/{})",
                    ai.mode,
                    ai.provider
                );
            }
            None => {
                executor.resolver.clear_ai_router();
                tracing::info!("[reactor] ai config applied (AI off)");
            }
        }
        drop(executor);

        // Push the new availability to the frontend so its `aiEnabled` flag —
        // the single gate for routing NL/`ask` to the agent vs. web — updates
        // the instant AI is enabled/disabled in Settings, with no polling.
        use tauri::Emitter;
        let _ = self.app.emit("lychi://ai-status-changed", available);
    }
}

/// Subscribe all config reactors to the bus. Called once from `AppState::wire_reactors`.
pub fn register_config_reactors(
    bus: &EventBus,
    executor: Arc<RwLock<Executor>>,
    config: Arc<RwLock<Config>>,
    ai_provider: Arc<RwLock<Option<Arc<dyn lychi_core::providers::AiProvider>>>>,
    app: tauri::AppHandle,
) {
    bus.subscribe(Arc::new(CommandsReactor {
        executor: executor.clone(),
        config: config.clone(),
    }));
    bus.subscribe(Arc::new(ProjectsReactor {
        executor: executor.clone(),
        config: config.clone(),
    }));
    bus.subscribe(Arc::new(AiReactor {
        executor,
        config,
        ai_provider,
        app,
    }));
}
