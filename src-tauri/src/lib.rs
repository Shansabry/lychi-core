mod commands;
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

    glib::set_application_name("Lychi");

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
            commands::media::media_get_status,
            commands::media::media_control,
            commands::media::media_control_all,
            commands::media::media_seek,
            commands::media::media_refresh,
            commands::open_uri::open_uri,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            // Ulauncher approach: transparent full-screen layer-shell window,
            // content positioned via CSS. Layer shell surfaces never appear
            // in the taskbar on any Wayland compositor.
            #[cfg(target_os = "linux")]
            if let Some(win) = app.get_webview_window("main") {
                if let Ok(gtk_win) = win.gtk_window() {
                    use gtk::prelude::{GtkWindowExt, MonitorExt, WidgetExt};

                    // Gracefully handle missing screen/monitor instead of panicking
                    let setup_ok = (|| -> Option<()> {
                        let screen = WidgetExt::screen(&gtk_win)?;
                        let display = screen.display();
                        let monitor = display.primary_monitor().or_else(|| display.monitor(0))?;
                        let geom = monitor.geometry();

                        // Ensure the window is transparent
                        if let Some(visual) = screen.rgba_visual() {
                            gtk_win.set_visual(Some(&visual));
                        }
                        gtk_win.set_app_paintable(true);

                        // Use layer shell if available (Wayland) — hides from taskbar
                        if gtk_layer_shell::is_supported() {
                            use gtk_layer_shell::LayerShell;
                            gtk_win.hide(); // Must unmap before init_layer_shell
                            gtk_win.init_layer_shell();
                            gtk_win.set_layer(gtk_layer_shell::Layer::Overlay);
                            gtk_win.set_keyboard_mode(gtk_layer_shell::KeyboardMode::OnDemand);
                            // Anchor to all edges = fullscreen on the layer
                            gtk_win.set_anchor(gtk_layer_shell::Edge::Top, true);
                            gtk_win.set_anchor(gtk_layer_shell::Edge::Bottom, true);
                            gtk_win.set_anchor(gtk_layer_shell::Edge::Left, true);
                            gtk_win.set_anchor(gtk_layer_shell::Edge::Right, true);
                            gtk_win.set_namespace("lychi");
                            tracing::info!("Layer shell initialized (Wayland)");
                        } else {
                            // X11 fallback — use regular window with skip-taskbar hints
                            gtk_win.set_size_request(geom.width(), geom.height());
                            gtk_win.set_skip_taskbar_hint(true);
                            gtk_win.set_skip_pager_hint(true);
                            tracing::info!("Using X11 skip-taskbar hints");
                        }
                        Some(())
                    })();

                    if setup_ok.is_none() {
                        tracing::error!(
                            "No GDK screen or monitor available — skipping window hints"
                        );
                    }
                }
                // Now show the window (visible: false in config, so we show after hints are set)
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

            // Spawn IPC server
            let ipc_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = ipc_server::run(ipc_handle).await {
                    tracing::error!("IPC server error: {e}");
                }
            });

            // Spawn MPRIS listener (pushes track changes from all players to frontend)
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
            let path = window::socket_path();
            if path.exists() {
                let _ = std::fs::remove_file(&path);
            }

            // Drop the MPRIS manager cleanly so D-Bus subscriptions
            // are released before the process exits. This prevents
            // crashes in media players (e.g. Spotify) caused by
            // abrupt D-Bus peer disconnection.
            let state = _app.state::<AppState>();
            let mpris = state.mpris.clone();
            tauri::async_runtime::block_on(async {
                let mut guard = mpris.write().await;
                guard.take();
            });
        }
    });
}

mod ipc_server {
    use crate::window;
    use tauri::Manager;
    use tokio::io::AsyncBufReadExt;
    use tokio::net::UnixListener;

    pub async fn run(handle: tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
        let path = window::socket_path();

        // Clean up stale socket
        if path.exists() {
            std::fs::remove_file(&path)?;
        }

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(&path)?;
        tracing::info!("IPC server listening on {}", path.display());

        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let handle = handle.clone();
                    tokio::spawn(async move {
                        let reader = tokio::io::BufReader::new(stream);
                        let mut lines = reader.lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            match line.trim() {
                                "toggle" => {
                                    if let Some(w) = handle.get_webview_window("main") {
                                        window::toggle_window(&w);
                                    }
                                }
                                other => {
                                    tracing::warn!("Unknown IPC command: {other}");
                                }
                            }
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("IPC accept error: {e}");
                }
            }
        }
    }
}
