mod commands;
#[cfg(unix)]
mod ipc_server;
mod platform;
mod state;
mod window;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    platform::init_app();

    let app_state = AppState::new();
    let hotkey = {
        let config = app_state.config.blocking_read();
        config.general.hotkey.clone()
    };

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                window::toggle_window(&w);
            }
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::execute::execute_command,
            commands::execute::get_completions,
            commands::history::get_history,
            commands::history::clear_history,
            commands::config::get_hide_on_blur,
            commands::config::save_window_position,
            commands::ai::get_ai_config,
            commands::ai::save_ai_config,
            commands::ai::set_api_key,
            commands::ai::get_ai_status,
            commands::ai::check_ai_health,
            commands::config::get_general_config,
            commands::config::save_general_config,
            commands::config::get_commands_config,
            commands::config::save_commands_config,
            commands::config::get_projects_config,
            commands::config::save_projects_config,
            commands::config::restart_app,
            commands::config::set_hotkey,
            commands::config::record_hotkey,
            commands::agent::get_agent_plan,
            commands::agent::store_agent_plan,
            commands::agent::execute_agent_plan,
            commands::filesystem::list_path_completions,
            commands::filesystem::list_directories,
            commands::filesystem::get_mount_points,
            commands::filesystem::start_file_search,
            commands::filesystem::cancel_file_search,
            commands::media::media_get_status,
            commands::media::media_control,
            commands::media::media_control_all,
            commands::media::media_seek,
            commands::media::media_refresh,
            commands::open_uri::open_uri,
            commands::notes::get_notes,
            commands::notes::add_note,
            commands::notes::update_note,
            commands::notes::delete_note,
            commands::notes::get_todos,
            commands::notes::add_todo,
            commands::notes::toggle_todo,
            commands::notes::delete_todo,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            // Platform-specific window setup (layer-shell, skip-taskbar, etc.)
            if let Some(win) = app.get_webview_window("main") {
                platform::init_window(&win);
                let _ = win.show();
            }

            // Register global shortcut
            use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
            let hotkey_str = hotkey.clone();
            app.global_shortcut()
                .on_shortcut(hotkey.as_str(), move |app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed
                        && let Some(w) = app.get_webview_window("main")
                    {
                        window::toggle_window(&w);
                    }
                })?;
            tracing::info!("Global shortcut registered: {hotkey_str}");

            // System tray
            use tauri::menu::{MenuBuilder, MenuItemBuilder};
            use tauri::tray::TrayIconBuilder;
            let show = MenuItemBuilder::with_id("show", "Show").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app)
                .item(&show)
                .separator()
                .item(&quit)
                .build()?;
            let tray_icon = app
                .default_window_icon()
                .cloned()
                .ok_or_else(|| "No default window icon embedded in binary".to_string())?;
            TrayIconBuilder::new()
                .icon(tray_icon)
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            window::toggle_window(&w);
                        }
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            // Spawn IPC server (Unix domain sockets — Linux/macOS only)
            #[cfg(unix)]
            {
                let ipc_handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = ipc_server::run(ipc_handle).await {
                        tracing::error!("IPC server error: {e}");
                    }
                });
            }

            // Spawn MPRIS listener (pushes track changes from all players to frontend)
            #[cfg(feature = "mpris")]
            {
                let media_handle = handle;
                let mpris_state = app.state::<AppState>().mpris.clone();
                tauri::async_runtime::spawn(async move {
                    use futures_util::StreamExt;
                    use lychi_core::mpris::MprisManager;
                    use tauri::Emitter;

                    let manager = match MprisManager::connect().await {
                        Err(e) => {
                            tracing::warn!("MPRIS D-Bus connect error: {e}");
                            return;
                        }
                        Ok(m) => m,
                    };

                    if !manager.has_players() {
                        tracing::debug!("No MPRIS players found — listener idle");
                    }

                    // Subscribe returns an owned stream — no borrow on manager
                    let stream = match manager.subscribe_all_changes().await {
                        Err(e) => {
                            tracing::warn!("MPRIS subscribe error: {e}");
                            let mut guard = mpris_state.write().await;
                            *guard = Some(manager);
                            return;
                        }
                        Ok(s) => s,
                    };

                    // Cache the manager (stream is owned, no borrow conflict)
                    {
                        let mut guard = mpris_state.write().await;
                        *guard = Some(manager);
                    }

                    tokio::pin!(stream);
                    while let Some(track) = stream.next().await {
                        let _ = media_handle.emit("lychi://media-track", &track);
                    }
                    tracing::info!("MPRIS D-Bus stream ended");
                });
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Lychi");

    app.run(|_app, event| {
        if let tauri::RunEvent::Exit = event {
            // Clean up Unix domain socket
            #[cfg(unix)]
            {
                let path = platform::ipc_path();
                if path.exists() {
                    let _ = std::fs::remove_file(&path);
                }
            }

            // Drop the MPRIS manager cleanly so D-Bus subscriptions
            // are released before the process exits. This prevents
            // crashes in media players (e.g. Spotify) caused by
            // abrupt D-Bus peer disconnection.
            #[cfg(feature = "mpris")]
            {
                let state = _app.state::<AppState>();
                let mpris = state.mpris.clone();
                tauri::async_runtime::block_on(async {
                    let mut guard = mpris.write().await;
                    guard.take();
                });
            }
        }
    });
}
