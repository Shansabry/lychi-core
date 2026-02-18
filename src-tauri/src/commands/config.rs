use crate::state::AppState;
use crate::window;
use lychi_core::config::{CommandsConfig, GeneralConfig, ProjectsConfig};
use lychi_core::error::LychiError;
use lychi_core::paths;
use tauri::{AppHandle, Manager, State};
use tokio::sync::oneshot;

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
    config.save(&paths::config_file())
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

    // If shell changed, invalidate cached env and re-register handler
    if old_shell != new_shell {
        lychi_core::command::shell_exec::invalidate_shell_env();
        let mut registry = state.registry.write().await;
        registry.register(Box::new(
            lychi_core::command::shell_exec::ShellExec::with_shell(new_shell.clone()),
        ));
        tracing::info!("Shell changed to: {new_shell}");
    }

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

    // Re-register project handler with updated directories
    let mut registry = state.registry.write().await;
    registry.register(Box::new(
        lychi_core::command::project_open::ProjectOpen::with_directories(dirs),
    ));

    Ok(())
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    app.restart();
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

/// Grab keyboard via the main Tauri GTK window and wait for a modifier+key combo.
/// Returns the combo string (e.g. "Super+Space") or an error.
#[tauri::command]
pub async fn record_hotkey(app: AppHandle) -> Result<String, LychiError> {
    let (tx, rx) = oneshot::channel::<Result<String, String>>();

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

    // We need to pass the AppHandle into the GLib closure instead of the GTK window,
    // because GTK widgets are not Send. We'll get the GTK window inside the closure.
    let app_for_glib = app.clone();

    glib::MainContext::default().invoke(move || {
        use gdk::keys::constants as key;
        use gtk::prelude::*;
        use std::sync::{Arc, Mutex};

        let win = match app_for_glib.get_webview_window("main") {
            Some(w) => w,
            None => {
                let _ = tx.send(Err("No main window".into()));
                return;
            }
        };
        let gtk_win = match win.gtk_window() {
            Ok(w) => w,
            Err(e) => {
                let _ = tx.send(Err(e.to_string()));
                return;
            }
        };

        let tx = Arc::new(Mutex::new(Some(tx)));
        let handler_id: Arc<Mutex<Option<glib::SignalHandlerId>>> = Arc::new(Mutex::new(None));

        let tx_clone = tx.clone();
        let handler_id_clone = handler_id.clone();
        let gtk_win_ref = gtk_win.clone();

        let id = gtk_win.connect_key_press_event(move |_, ev| {
            let keyval = ev.keyval();

            // Ignore pure modifier presses
            if matches!(
                keyval,
                key::Control_L
                    | key::Control_R
                    | key::Alt_L
                    | key::Alt_R
                    | key::Shift_L
                    | key::Shift_R
                    | key::Super_L
                    | key::Super_R
                    | key::Meta_L
                    | key::Meta_R
                    | key::Hyper_L
                    | key::Hyper_R
                    | key::ISO_Level3_Shift
            ) {
                return glib::Propagation::Stop;
            }

            let state = ev.state();
            let mut parts = Vec::new();
            if state.contains(gdk::ModifierType::CONTROL_MASK) {
                parts.push("Ctrl");
            }
            if state.contains(gdk::ModifierType::MOD1_MASK) {
                parts.push("Alt");
            }
            if state.contains(gdk::ModifierType::SHIFT_MASK) {
                parts.push("Shift");
            }
            if state.contains(gdk::ModifierType::SUPER_MASK)
                || state.contains(gdk::ModifierType::MOD4_MASK)
            {
                parts.push("Super");
            }

            // Escape without modifiers = cancel
            if keyval == key::Escape && parts.is_empty() {
                if let Some(id) = handler_id_clone.lock().ok().and_then(|mut g| g.take()) {
                    gtk_win_ref.disconnect(id);
                }
                if let Some(tx) = tx_clone.lock().ok().and_then(|mut g| g.take()) {
                    let _ = tx.send(Err("Cancelled".into()));
                }
                return glib::Propagation::Stop;
            }

            // Require at least one modifier
            if parts.is_empty() {
                return glib::Propagation::Stop;
            }

            // Map the key name
            let key_name = gdk_keyval_to_tauri_name(keyval);
            if key_name.is_empty() {
                return glib::Propagation::Stop;
            }

            parts.push(&key_name);
            let combo = parts.join("+");

            // Disconnect this handler — one-shot capture complete
            if let Some(id) = handler_id_clone.lock().ok().and_then(|mut g| g.take()) {
                gtk_win_ref.disconnect(id);
            }

            if let Some(tx) = tx_clone.lock().ok().and_then(|mut g| g.take()) {
                let _ = tx.send(Ok(combo));
            }

            glib::Propagation::Stop
        });

        // Store the handler ID so the closure can disconnect itself
        if let Ok(mut guard) = handler_id.lock() {
            *guard = Some(id);
        }
    });

    let result = rx
        .await
        .map_err(|_| LychiError::Config("Recording cancelled".into()))?;

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

/// Convert a GDK keyval to the Tauri shortcut key name.
fn gdk_keyval_to_tauri_name(keyval: gdk::keys::Key) -> String {
    use gdk::keys::constants as key;
    match keyval {
        key::space => "Space".into(),
        key::Return | key::KP_Enter => "Enter".into(),
        key::Tab | key::ISO_Left_Tab => "Tab".into(),
        key::BackSpace => "Backspace".into(),
        key::Delete | key::KP_Delete => "Delete".into(),
        key::Up | key::KP_Up => "Up".into(),
        key::Down | key::KP_Down => "Down".into(),
        key::Left | key::KP_Left => "Left".into(),
        key::Right | key::KP_Right => "Right".into(),
        key::Home | key::KP_Home => "Home".into(),
        key::End | key::KP_End => "End".into(),
        key::Page_Up | key::KP_Page_Up => "PageUp".into(),
        key::Page_Down | key::KP_Page_Down => "PageDown".into(),
        key::Insert | key::KP_Insert => "Insert".into(),
        key::F1 => "F1".into(),
        key::F2 => "F2".into(),
        key::F3 => "F3".into(),
        key::F4 => "F4".into(),
        key::F5 => "F5".into(),
        key::F6 => "F6".into(),
        key::F7 => "F7".into(),
        key::F8 => "F8".into(),
        key::F9 => "F9".into(),
        key::F10 => "F10".into(),
        key::F11 => "F11".into(),
        key::F12 => "F12".into(),
        _ => {
            // Try to get a printable name (letters, digits, symbols)
            if let Some(ch) = keyval.to_unicode()
                && (ch.is_alphanumeric() || ch.is_ascii_punctuation())
            {
                return ch.to_uppercase().to_string();
            }
            // Fall back to GDK key name
            keyval.name().map(|n| n.to_string()).unwrap_or_default()
        }
    }
}
