use crate::state::AppState;
use crate::window;
use lychi_core::config::db as config_db;
use lychi_core::config::schema::{KeybindingsConfig, PrivacyConfig};
use lychi_core::config::{AiConfig, CommandsConfig, GeneralConfig, ProjectsConfig};
use lychi_core::error::LychiError;
use lychi_core::paths;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

/// Single-IPC batch response for all settings data the frontend needs at startup.
#[derive(Serialize)]
pub struct AllSettings {
    pub ai: AiConfig,
    pub general: GeneralConfig,
    pub commands: CommandsConfig,
    pub projects: ProjectsConfig,
    pub privacy: PrivacyConfig,
    pub keybindings: KeybindingsConfig,
    pub app_version: String,
    pub layer_shell_supported: bool,
    pub active_window_strategy: String,
}

#[tauri::command]
pub fn get_all_settings(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AllSettings, LychiError> {
    let config = state.config.blocking_read();

    let layer_shell_supported = gtk_layer_shell::is_supported();

    let active_window_strategy = if let Some(win) = app.get_webview_window("main") {
        if let Ok(gtk_win) = win.gtk_window() {
            use gtk_layer_shell::LayerShell;
            if gtk_win.is_layer_window() {
                "layer-shell"
            } else {
                "x11"
            }
        } else {
            "x11"
        }
    } else {
        "x11"
    }
    .to_string();

    let app_version = app.package_info().version.to_string();

    Ok(AllSettings {
        ai: config.ai.clone(),
        general: config.general.clone(),
        commands: config.commands.clone(),
        projects: config.projects.clone(),
        privacy: config.privacy.clone(),
        keybindings: config.keybindings.clone(),
        app_version,
        layer_shell_supported,
        active_window_strategy,
    })
}

#[tauri::command]
pub async fn get_hide_on_blur(state: State<'_, AppState>) -> Result<bool, LychiError> {
    let config = state.config.read().await;
    Ok(config.general.hide_on_blur)
}

#[tauri::command]
pub async fn save_window_position(
    state: State<'_, AppState>,
    x: i32,
    y: i32,
) -> Result<(), LychiError> {
    let mut config = state.config.write().await;
    config.general.window_x = Some(x);
    config.general.window_y = Some(y);
    config.save(&paths::config_file())
}

#[tauri::command]
pub async fn get_general_config(state: State<'_, AppState>) -> Result<GeneralConfig, LychiError> {
    let config = state.config.read().await;
    Ok(config.general.clone())
}

#[tauri::command]
pub async fn save_general_config(
    state: State<'_, AppState>,
    general: GeneralConfig,
) -> Result<(), LychiError> {
    let mut config = state.config.write().await;
    config.general = general;
    config.save(&paths::config_file())?;
    config_db::save_config_to_db(&state.db, &config)?;
    Ok(())
}

#[tauri::command]
pub async fn get_commands_config(state: State<'_, AppState>) -> Result<CommandsConfig, LychiError> {
    let config = state.config.read().await;
    Ok(config.commands.clone())
}

#[tauri::command]
pub async fn save_commands_config(
    state: State<'_, AppState>,
    commands: CommandsConfig,
) -> Result<(), LychiError> {
    let new_shell = commands.shell.clone();
    let mut config = state.config.write().await;
    let old_shell = config.commands.shell.clone();
    config.commands = commands;
    config.save(&paths::config_file())?;
    config_db::save_config_to_db(&state.db, &config)?;

    // If shell changed, invalidate cached env and re-register handler
    if old_shell != new_shell {
        lychi_core::action_registry::handlers::shell_exec::invalidate_shell_env();
        let mut executor = state.executor.write().await;
        executor.registry.register(Box::new(
            lychi_core::action_registry::handlers::shell_exec::ShellExec::with_shell(
                new_shell.clone(),
            ),
        ));
        tracing::info!("Shell changed to: {new_shell}");
    }

    // Update terminal setting
    let new_terminal = config.commands.terminal.clone();
    lychi_core::action_registry::handlers::shell_exec::set_terminal(Some(new_terminal));

    Ok(())
}

#[tauri::command]
pub async fn get_projects_config(state: State<'_, AppState>) -> Result<ProjectsConfig, LychiError> {
    let config = state.config.read().await;
    Ok(config.projects.clone())
}

#[tauri::command]
pub async fn save_projects_config(
    state: State<'_, AppState>,
    projects: ProjectsConfig,
) -> Result<(), LychiError> {
    let dirs = projects.directories.clone();
    let mut config = state.config.write().await;
    config.projects = projects;
    config.save(&paths::config_file())?;
    config_db::save_config_to_db(&state.db, &config)?;

    // Re-register project handler with updated directories
    let mut executor = state.executor.write().await;
    executor.registry.register(Box::new(
        lychi_core::action_registry::handlers::project_open::ProjectOpen::with_directories(dirs),
    ));

    Ok(())
}

// --- Privacy ---

#[tauri::command]
pub async fn get_privacy_config(state: State<'_, AppState>) -> Result<PrivacyConfig, LychiError> {
    let config = state.config.read().await;
    Ok(config.privacy.clone())
}

#[tauri::command]
pub async fn save_privacy_config(
    state: State<'_, AppState>,
    privacy: PrivacyConfig,
) -> Result<(), LychiError> {
    let mut config = state.config.write().await;
    config.privacy = privacy;
    config.save(&paths::config_file())?;
    config_db::save_config_to_db(&state.db, &config)?;
    Ok(())
}

/// C6: Grant a specific privacy consent and persist it.
/// Called by the frontend when the user confirms a privacy-gated action.
/// `feature` is one of: "ip_geolocation", "public_ip"
#[tauri::command]
pub async fn grant_privacy_consent(
    state: State<'_, AppState>,
    feature: String,
) -> Result<(), LychiError> {
    let mut config = state.config.write().await;
    match feature.as_str() {
        "ip_geolocation" => config.privacy.allow_ip_geolocation = true,
        "public_ip" => config.privacy.allow_public_ip = true,
        other => {
            return Err(LychiError::Config(format!(
                "Unknown privacy feature: {other}"
            )));
        }
    }
    config.save(&paths::config_file())
}

// --- Keybindings ---

#[tauri::command]
pub async fn get_keybindings_config(
    state: State<'_, AppState>,
) -> Result<KeybindingsConfig, LychiError> {
    let config = state.config.read().await;
    Ok(config.keybindings.clone())
}

#[tauri::command]
pub async fn save_keybindings_config(
    state: State<'_, AppState>,
    keybindings: KeybindingsConfig,
) -> Result<(), LychiError> {
    let mut config = state.config.write().await;
    config.keybindings = keybindings;
    config.save(&paths::config_file())?;
    Ok(())
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    app.restart();
}

/// Returns whether layer-shell (Wayland) is supported on this session.
#[tauri::command]
pub fn get_layer_shell_supported() -> bool {
    gtk_layer_shell::is_supported()
}

/// Returns the window strategy that is currently active (what init_window chose).
/// "layer-shell" if the main window is a layer-shell surface, "x11" otherwise.
#[tauri::command]
pub fn get_active_window_strategy(app: AppHandle) -> String {
    if let Some(win) = app.get_webview_window("main")
        && let Ok(gtk_win) = win.gtk_window()
    {
        use gtk_layer_shell::LayerShell;
        if gtk_win.is_layer_window() {
            return "layer-shell".to_string();
        }
    }
    "x11".to_string()
}

/// Change the global hotkey at runtime: unregister old, register new, persist to config.
#[tauri::command]
pub async fn set_hotkey(
    app: AppHandle,
    state: State<'_, AppState>,
    hotkey: String,
) -> Result<(), LychiError> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    let shortcut_manager = app.global_shortcut();

    // Unregister the old hotkey
    let old_hotkey = {
        let config = state.config.read().await;
        config.general.hotkey.clone()
    };
    let _ = shortcut_manager.unregister(old_hotkey.as_str());

    // Register the new hotkey
    shortcut_manager
        .on_shortcut(hotkey.as_str(), move |app, _shortcut, event| {
            if event.state == ShortcutState::Pressed
                && let Some(w) = app.get_webview_window("main")
            {
                window::toggle_window(&w);
            }
        })
        .map_err(|e| LychiError::Config(format!("Invalid hotkey: {e}")))?;

    // Persist to config
    let mut config = state.config.write().await;
    config.general.hotkey = hotkey.clone();
    config.save(&paths::config_file())?;

    tracing::info!("Hotkey changed to: {hotkey}");
    Ok(())
}

/// Grab keyboard and wait for a modifier+key combo.
/// Returns the combo string (e.g. "Super+Space") or an error.
#[tauri::command]
pub async fn record_hotkey(app: AppHandle) -> Result<String, LychiError> {
    // First, unregister the current hotkey so it doesn't interfere with recording
    let current_hotkey = {
        let state = app.state::<AppState>();
        let config = state.config.read().await;
        config.general.hotkey.clone()
    };
    {
        use tauri_plugin_global_shortcut::GlobalShortcutExt;
        let _ = app.global_shortcut().unregister(current_hotkey.as_str());
    }

    // Delegate to platform-specific key capture
    let result = crate::platform::record_hotkey(&app).await;

    // Re-register the current hotkey if recording was cancelled
    if result.is_err() {
        let app_clone = app.clone();
        let hotkey = current_hotkey.clone();
        use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
        if let Err(e) =
            app.global_shortcut()
                .on_shortcut(hotkey.as_str(), move |_app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed
                        && let Some(w) = app_clone.get_webview_window("main")
                    {
                        window::toggle_window(&w);
                    }
                })
        {
            tracing::error!(
                "Failed to re-register hotkey '{hotkey}' after cancelled recording: {e}"
            );
            return Err(LychiError::Config(format!(
                "Hotkey recording cancelled and failed to restore previous hotkey: {e}"
            )));
        }
    }

    result.map_err(LychiError::Config)
}
