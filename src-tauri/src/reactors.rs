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

/// Subscribe all config reactors to the bus. Called once from `AppState::wire_reactors`.
pub fn register_config_reactors(
    bus: &EventBus,
    executor: Arc<RwLock<Executor>>,
    config: Arc<RwLock<Config>>,
) {
    bus.subscribe(Arc::new(CommandsReactor {
        executor: executor.clone(),
        config: config.clone(),
    }));
    bus.subscribe(Arc::new(ProjectsReactor { executor, config }));
}
