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
    let (hotkey, window_strategy, shell_path, project_dirs) = {
        let config = app_state.config.blocking_read();
        lychi_core::context::active_window::register_extra_terminals(
            &config.commands.extra_terminals,
        );
        lychi_core::context::ide::register_extra_markers(
            &config.projects.extra_strong_markers,
            &config.projects.extra_soft_markers,
        );
        lychi_core::context::pin::set(config.projects.pinned_workspace.clone());
        (
            config.general.hotkey.clone(),
            config.general.window_strategy.clone(),
            config.commands.shell.clone(),
            config.projects.directories.clone(),
        )
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
            commands::config::get_all_settings,
            commands::config::get_hide_on_blur,
            commands::config::save_window_position,
            commands::ai::get_ai_config,
            commands::ai::save_ai_config,
            commands::ai::set_api_key,
            commands::ai::get_masked_api_key,
            commands::ai::get_ai_status,
            commands::ai::check_ai_health,
            commands::config::get_general_config,
            commands::config::save_general_config,
            commands::config::get_commands_config,
            commands::config::save_commands_config,
            commands::config::get_projects_config,
            commands::config::save_projects_config,
            commands::config::get_privacy_config,
            commands::config::save_privacy_config,
            commands::config::grant_privacy_consent,
            commands::config::get_keybindings_config,
            commands::config::save_keybindings_config,
            commands::config::restart_app,
            commands::config::set_hotkey,
            commands::config::record_hotkey,
            commands::config::get_layer_shell_supported,
            commands::config::get_active_window_strategy,
            commands::config::hide_launcher,
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
            commands::notes::get_all_notes,
            commands::notes::get_notes,
            commands::notes::add_note,
            commands::notes::update_note,
            commands::notes::delete_note,
            commands::notes::get_todos,
            commands::notes::add_todo,
            commands::notes::toggle_todo,
            commands::notes::delete_todo,
            commands::aliases::get_aliases,
            commands::aliases::add_alias,
            commands::aliases::update_alias,
            commands::aliases::delete_alias,
            commands::preview::get_file_preview,
            commands::timer::get_timers,
            commands::reminders::get_reminders,
            commands::reminders::add_reminder,
            commands::reminders::delete_reminder,
            commands::snippets::get_snippets,
            commands::snippets::add_snippet,
            commands::snippets::update_snippet,
            commands::snippets::delete_snippet,
            commands::context::get_context,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            // Platform-specific window setup (layer-shell, skip-taskbar, etc.)
            if let Some(win) = app.get_webview_window("main") {
                let app_state = app.state::<AppState>();
                platform::init_window(&win, &window_strategy);
                platform::setup_dismiss_on_blur(
                    &win,
                    app_state.dismiss_armed.clone(),
                    app_state.summon_seq.clone(),
                );
                platform::setup_escape_handler(&win);
                window::show_window(&win);
            }

            // Warm up caches in parallel for snappy first search
            tauri::async_runtime::spawn_blocking(|| {
                commands::filesystem::warmup_fs_cache();
            });
            tauri::async_runtime::spawn_blocking(|| {
                lychi_core::action_registry::handlers::app_launcher::AppLauncher::warmup();
            });
            let shell_for_warmup = shell_path.clone();
            tauri::async_runtime::spawn_blocking(move || {
                lychi_core::action_registry::handlers::shell_exec::ShellExec::warmup(
                    &shell_for_warmup,
                );
            });
            let dirs_for_warmup = project_dirs.clone();
            tauri::async_runtime::spawn_blocking(move || {
                lychi_core::action_registry::handlers::project_open::ProjectOpen::warmup(
                    &dirs_for_warmup,
                );
            });
            tauri::async_runtime::spawn_blocking(|| {
                lychi_core::history::HistoryStore::warmup();
            });
            tauri::async_runtime::spawn_blocking(|| {
                lychi_core::context::network::warmup();
            });
            tauri::async_runtime::spawn_blocking(|| {
                lychi_core::context::window_stack::warmup();
            });

            // Warm alias cache for transparent alias resolution in router
            let alias_db = app.state::<state::AppState>().db.clone();
            lychi_core::aliases::store::warm_cache(&alias_db);

            // Pre-fetch exchange rates for currency conversion (fire-and-forget)
            tauri::async_runtime::spawn(async {
                lychi_core::action_registry::handlers::calc::fetch_exchange_rates().await;
            });

            // Background clipboard monitor — owns its OS thread (not the Tokio blocking pool)
            let clip_db = app.state::<AppState>().db.clone();
            let clipboard_running = app.state::<AppState>().clipboard_running.clone();
            std::thread::Builder::new()
                .name("lychi-clipboard".to_string())
                .spawn(move || {
                    lychi_core::clipboard::store::run_clipboard_monitor(clip_db, clipboard_running);
                })
                .expect("failed to spawn clipboard monitor thread");

            // App-index filesystem watcher — rebuilds AppIndex when .desktop files change
            let watcher_running = app.state::<AppState>().app_index_watcher_running.clone();
            lychi_core::desktop_apps::watcher::start(watcher_running);

            // Background timer + reminder monitor — owns its OS thread
            // (all notify-rust D-Bus calls serialized in this single thread)
            let timer_state = app.state::<AppState>().timer_state.clone();
            let monitor_db = app.state::<AppState>().db.clone();
            let timer_running = app.state::<AppState>().timer_running.clone();
            std::thread::Builder::new()
                .name("lychi-timer".to_string())
                .spawn(move || {
                    lychi_core::action_registry::handlers::timer::run_timer_monitor(
                        timer_state,
                        monitor_db,
                        timer_running,
                    );
                })
                .expect("failed to spawn timer monitor thread");

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

            // Spawn KWin active-window watcher (keeps context cache warm on Wayland)
            #[cfg(target_os = "linux")]
            if lychi_core::context::is_wayland() {
                tauri::async_runtime::spawn(async move {
                    lychi_core::context::active_window::run_kwin_watcher(|ctx| {
                        if ctx.is_terminal {
                            lychi_core::context::window_stack::push_focus_entry(ctx);
                        }
                    })
                    .await;
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

                    // Cache immediately so media commands work while we set up the stream
                    {
                        let mut guard = mpris_state.write().await;
                        *guard = Some(manager);
                    }
                    tracing::info!("[mpris] Manager cached");

                    // Subscribe to changes — read back from cache
                    let guard = mpris_state.read().await;
                    let stream = match guard.as_ref().unwrap().subscribe_all_changes().await {
                        Err(e) => {
                            tracing::warn!("MPRIS subscribe error: {e}");
                            return;
                        }
                        Ok(s) => s,
                    };
                    drop(guard);

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
        match event {
            tauri::RunEvent::ExitRequested { .. } => {
                // Signal monitors to stop before Tauri begins teardown, so they
                // don't issue D-Bus calls (notify_rust / arboard) mid-shutdown.
                let state = _app.state::<AppState>();
                state
                    .clipboard_running
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                state
                    .timer_running
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                state
                    .app_index_watcher_running
                    .store(false, std::sync::atomic::Ordering::SeqCst);
            }
            tauri::RunEvent::Exit => {
                // Belt-and-suspenders: also set on Exit in case ExitRequested was skipped.
                {
                    let state = _app.state::<AppState>();
                    state
                        .clipboard_running
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    state
                        .timer_running
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    state
                        .app_index_watcher_running
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                }

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
            _ => {}
        }
    });
}
