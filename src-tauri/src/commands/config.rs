use crate::state::AppState;
use crate::window;
use lychi_core::config::db as config_db;
use lychi_core::config::schema::{KeybindingsConfig, PrivacyConfig};
use lychi_core::config::{AiConfig, CommandsConfig, GeneralConfig, ProjectsConfig};
use lychi_core::error::LychiError;
use lychi_core::events::{ConfigSection, DomainEvent};
use lychi_core::paths;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

/// Single-IPC batch response for all settings data the frontend needs at startup.
#[derive(Serialize, specta::Type)]
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
    /// False when running on X11 without a compositor — the transparent
    /// overlay renders opaque and the user should enable compositing.
    pub screen_composited: bool,
}

#[tauri::command]
#[specta::specta]
pub fn get_all_settings(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AllSettings, LychiError> {
    let config = state.config.blocking_read();

    let layer_shell_supported = gtk_layer_shell::is_supported();

    let active_window_strategy = crate::platform::active_strategy().as_str().to_string();

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
        screen_composited: crate::platform::screen_composited(),
    })
}

#[tauri::command]
#[specta::specta]
pub async fn get_hide_on_blur(state: State<'_, AppState>) -> Result<bool, LychiError> {
    let config = state.config.read().await;
    Ok(config.general.hide_on_blur)
}

#[tauri::command]
#[specta::specta]
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
#[specta::specta]
pub async fn get_general_config(state: State<'_, AppState>) -> Result<GeneralConfig, LychiError> {
    let config = state.config.read().await;
    Ok(config.general.clone())
}

#[tauri::command]
#[specta::specta]
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
#[specta::specta]
pub async fn get_commands_config(state: State<'_, AppState>) -> Result<CommandsConfig, LychiError> {
    let config = state.config.read().await;
    Ok(config.commands.clone())
}

/// Font families installed on this system, for the Settings font pickers.
///
/// The WebView can't answer this itself — `document.fonts` only knows faces the
/// page has loaded — so it comes from fontconfig. Runs on a blocking thread
/// because it shells out to `fc-list`, which walks the font cache and takes
/// tens of milliseconds on a system with a thousand families.
#[tauri::command]
#[specta::specta]
pub async fn get_installed_fonts() -> Result<Vec<lychi_core::fonts::FontFamily>, LychiError> {
    tokio::task::spawn_blocking(lychi_core::fonts::installed_families)
        .await
        .map_err(|e| LychiError::Config(format!("font enumeration failed: {e}")))
}

/// Every keyword already taken by a built-in command.
///
/// Serves the Settings UI so it can warn about a colliding quicklink keyword
/// while typing. The list comes from the live registry — the same source the
/// save-path check uses — so the two can never disagree. A hand-maintained copy
/// in the frontend would drift silently every time a handler is added.
#[tauri::command]
#[specta::specta]
pub async fn get_reserved_keywords(state: State<'_, AppState>) -> Result<Vec<String>, LychiError> {
    let executor = state.executor.read().await;
    Ok(executor
        .registry
        .known_prefixes()
        .into_iter()
        .map(String::from)
        .collect())
}

#[tauri::command]
#[specta::specta]
pub async fn save_commands_config(
    state: State<'_, AppState>,
    commands: CommandsConfig,
) -> Result<(), LychiError> {
    // Reject quicklinks that collide with a reserved command before persisting,
    // so a bad keyword never shadows a real command. The reserved-command check
    // is delegated to the live action registry — the single source of truth for
    // what a keyword would shadow.
    {
        let executor = state.executor.read().await;
        commands
            .validate_quicklinks(&|w| executor.registry.is_known_prefix(w))
            .map_err(LychiError::Config)?;
    }

    // Persist, then release the write lock BEFORE emitting — the reactors acquire
    // the config/executor locks with blocking_*, so the command must not still be
    // holding them when the event fans out.
    {
        let mut config = state.config.write().await;
        config.commands = commands;
        config.save(&paths::config_file())?;
        config_db::save_config_to_db(&state.db, &config)?;
    }

    // The CommandsReactor re-derives shell + terminal + bang routing from the live
    // config. The command no longer knows those subsystems exist.
    state.event_bus.emit(DomainEvent::ConfigChanged {
        section: ConfigSection::Commands,
    });

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn get_projects_config(state: State<'_, AppState>) -> Result<ProjectsConfig, LychiError> {
    let config = state.config.read().await;
    Ok(config.projects.clone())
}

#[tauri::command]
#[specta::specta]
pub async fn save_projects_config(
    state: State<'_, AppState>,
    projects: ProjectsConfig,
) -> Result<(), LychiError> {
    // Persist, then release the write lock before emitting (see the note in
    // save_commands_config — reactors take the config/executor locks blocking).
    {
        let mut config = state.config.write().await;
        config.projects = projects;
        config.save(&paths::config_file())?;
        config_db::save_config_to_db(&state.db, &config)?;
    }

    // The ProjectsReactor updates IDE markers, the pinned workspace, and the
    // project handler's directories from the live config.
    state.event_bus.emit(DomainEvent::ConfigChanged {
        section: ConfigSection::Projects,
    });

    Ok(())
}

// --- Privacy ---

#[tauri::command]
#[specta::specta]
pub async fn get_privacy_config(state: State<'_, AppState>) -> Result<PrivacyConfig, LychiError> {
    let config = state.config.read().await;
    Ok(config.privacy.clone())
}

#[tauri::command]
#[specta::specta]
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
#[specta::specta]
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
#[specta::specta]
pub async fn get_keybindings_config(
    state: State<'_, AppState>,
) -> Result<KeybindingsConfig, LychiError> {
    let config = state.config.read().await;
    Ok(config.keybindings.clone())
}

#[tauri::command]
#[specta::specta]
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
#[specta::specta]
pub fn restart_app(app: AppHandle) {
    app.restart();
}

/// Returns terminal emulators found in PATH.
#[tauri::command]
#[specta::specta]
pub fn get_installed_terminals() -> Vec<String> {
    lychi_core::config::schema::detect_installed_terminals()
}

/// Returns whether layer-shell (Wayland) is supported on this session.
#[tauri::command]
#[specta::specta]
pub fn get_layer_shell_supported() -> bool {
    gtk_layer_shell::is_supported()
}

/// Returns the window strategy that is currently active (what init_window chose).
/// "layer-shell" | "toplevel" | "x11"
#[tauri::command]
#[specta::specta]
pub fn get_active_window_strategy() -> String {
    crate::platform::active_strategy().as_str().to_string()
}

/// Whether the app is registered to start at login (XDG autostart entry).
/// The OS entry is the source of truth — no config field involved.
#[tauri::command]
#[specta::specta]
pub fn get_autostart_enabled(app: AppHandle) -> Result<bool, LychiError> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch()
        .is_enabled()
        .map_err(|e| LychiError::Config(format!("autostart: {e}")))
}

#[tauri::command]
#[specta::specta]
pub fn set_autostart_enabled(app: AppHandle, enabled: bool) -> Result<(), LychiError> {
    use tauri_plugin_autostart::ManagerExt;
    let autolaunch = app.autolaunch();
    let result = if enabled {
        autolaunch.enable()
    } else {
        autolaunch.disable()
    };
    result.map_err(|e| LychiError::Config(format!("autostart: {e}")))?;
    tracing::info!("Autostart {}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}

/// Hotkey registration status, used by the frontend to guide Wayland users
/// toward a DE-bound `lychi --toggle` shortcut when the in-app hotkey
/// cannot work system-wide.
#[derive(Serialize, specta::Type)]
pub struct HotkeyStatus {
    pub registered: bool,
    /// "wayland" | "x11"
    pub session_type: String,
    /// XDG_CURRENT_DESKTOP (e.g. "KDE", "GNOME"), empty if unset
    pub desktop: String,
    /// True when the in-app shortcut fires system-wide. On Wayland the
    /// X11-based plugin only fires while an XWayland window is focused,
    /// so registration success there does not mean reliable.
    pub reliable: bool,
}

#[tauri::command]
#[specta::specta]
pub fn get_hotkey_status(state: State<'_, AppState>) -> HotkeyStatus {
    let registered = state
        .hotkey_registered
        .load(std::sync::atomic::Ordering::SeqCst);
    let portal_bound = state.portal_bound.load(std::sync::atomic::Ordering::SeqCst);
    let wayland = lychi_core::context::is_wayland();
    HotkeyStatus {
        registered,
        session_type: if wayland { "wayland" } else { "x11" }.to_string(),
        desktop: std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        // X11 plugin registration is system-wide on X11; on Wayland only a
        // portal binding counts as reliable.
        reliable: registered && (!wayland || portal_bound),
    }
}

/// Hide the launcher window.
/// Called from frontend instead of window.hide() to keep hide logic centralised.
#[tauri::command]
#[specta::specta]
pub fn hide_launcher(app: AppHandle) {
    // Disarm dismiss so focus-out during hide doesn't re-trigger
    let state = app.state::<AppState>();
    state
        .dismiss_armed
        .store(false, std::sync::atomic::Ordering::SeqCst);
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
}

/// Change the global hotkey at runtime: unregister old, register new, persist to config.
#[tauri::command]
#[specta::specta]
pub async fn set_hotkey(
    app: AppHandle,
    state: State<'_, AppState>,
    hotkey: String,
) -> Result<(), LychiError> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    // Wayland: the compositor binding (`lychi --toggle`) is the single toggle
    // source — persist the combo but never register the X11-based plugin
    // (it would double-fire with the DE binding over XWayland windows).
    if !lychi_core::context::is_wayland() {
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
        state
            .hotkey_registered
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    // Move the desktop-settings binding too, not just the X11 grab.
    //
    // On XFCE the accelerator is part of the config property path, so writing a
    // new one without removing the old leaves BOTH bound to Lychi — the user
    // rebinds Super+Space to Ctrl+Space and finds the old key still works, with
    // nothing in the UI explaining why. Order matters: unregister the previous
    // combination first, then claim the new one.
    #[cfg(target_os = "linux")]
    {
        let old_hotkey = {
            let config = state.config.read().await;
            config.general.hotkey.clone()
        };
        if old_hotkey != hotkey {
            crate::hotkey_de::unregister(&old_hotkey);
        }
        match crate::hotkey_de::register(&hotkey) {
            crate::hotkey_de::Outcome::Conflict(owner) => {
                // Surfaced as an error so the Settings UI can tell the user,
                // rather than silently persisting a hotkey that won't fire.
                return Err(LychiError::Config(format!(
                    "{hotkey} is already used by {owner} — pick another combination"
                )));
            }
            outcome => tracing::debug!(?outcome, "[hotkey] desktop binding for {hotkey}"),
        }
    }

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
#[specta::specta]
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
    // (X11 only — the plugin shortcut is never registered on Wayland)
    if result.is_err() && !lychi_core::context::is_wayland() {
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
