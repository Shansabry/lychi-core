mod commands;
#[cfg(target_os = "linux")]
mod hotkey_de;
mod hotkey_portal;
#[cfg(unix)]
mod ipc_server;
mod logging;
mod platform;
mod reactors;
mod state;
mod window;

use state::AppState;
use tauri::Manager;

/// Socket path for the multi-call `--toggle` dispatch in main().
#[cfg(unix)]
pub fn ipc_socket_path() -> std::path::PathBuf {
    platform::ipc_path()
}

/// The single source of truth for which commands cross the IPC boundary.
///
/// Extracted from `run()` so the binding export can also be driven from a test
/// (`export_bindings`). tauri-specta documents both a debug-startup export and
/// a unit-test export; we do both, from this one list, so they cannot disagree.
fn specta_builder() -> tauri_specta::Builder<tauri::Wry> {
    tauri_specta::Builder::<tauri::Wry>::new().commands(tauri_specta::collect_commands![
        commands::execute::execute_command,
        commands::execute::confirm_execution,
        commands::execute::get_completions,
        commands::execute::classify_input,
        commands::execute::suggest_correction,
        commands::execute::get_command_catalog,
        commands::execute::get_trigger_catalog,
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
        commands::ai::get_model_vision,
        commands::ai::test_ai_connection,
        commands::ai::list_ollama_models,
        commands::ai::get_local_models,
        commands::ai::download_local_model,
        commands::ai::delete_local_model,
        commands::ai_chat::cancel_ai_chat,
        commands::agent_chat::agent_chat_start,
        commands::agent_chat::agent_approve,
        commands::config::get_general_config,
        commands::config::save_general_config,
        commands::config::get_commands_config,
        commands::config::get_reserved_keywords,
        commands::config::get_installed_fonts,
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
        commands::config::get_installed_terminals,
        commands::config::get_layer_shell_supported,
        commands::config::get_active_window_strategy,
        commands::config::get_hotkey_status,
        commands::config::get_autostart_enabled,
        commands::config::set_autostart_enabled,
        commands::config::hide_launcher,
        commands::agent::get_agent_plan,
        commands::agent::store_agent_plan,
        commands::agent::execute_agent_plan,
        commands::filesystem::list_path_completions,
        commands::filesystem::fuzzy_path_completions,
        commands::filesystem::list_directories,
        commands::filesystem::get_mount_points,
        commands::filesystem::start_file_search,
        commands::filesystem::cancel_file_search,
        commands::filesystem::classify_files,
        commands::filesystem::attach_from_clipboard,
        commands::media::media_get_status,
        commands::media::media_control,
        commands::media::media_control_all,
        commands::media::media_seek,
        commands::media::media_refresh,
        commands::open_uri::open_uri,
        commands::reveal_path::reveal_path,
        commands::reveal_path::open_path,
        commands::notes::get_all_notes,
        commands::notes::get_all_items,
        commands::notes::toggle_item,
        commands::notes::delete_item,
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
        commands::ai_presets::get_ai_presets,
        commands::ai_presets::add_ai_preset,
        commands::ai_presets::update_ai_preset,
        commands::ai_presets::delete_ai_preset,
        commands::ai_history::get_conversations,
        commands::ai_history::get_conversation,
        commands::ai_history::delete_conversation,
        commands::ai_history::clear_conversations,
        commands::ai_history::load_conversation,
        commands::context::get_context,
        commands::context::read_selection,
        commands::frontend_log::log_frontend,
        commands::firebase_auth::firebase_sign_in,
        commands::firebase_auth::firebase_sign_out,
        commands::firebase_auth::firebase_get_user,
        commands::firebase_auth::cloud_get_credits,
    ])
}

/// TypeScript export configuration, shared by the debug-startup export and the
/// test, so both produce byte-identical output.
///
/// Maps Rust u64/i64 (durations, timestamps, ids) to a JS `number`. Specta
/// forbids this by default (values past 2^53 lose precision), but our u64
/// fields are millisecond durations / small counts that never approach that
/// ceiling, so `number` is correct and ergonomic.
#[cfg(debug_assertions)]
fn ts_config() -> specta_typescript::Typescript {
    specta_typescript::Typescript::default().bigint(specta_typescript::BigIntExportBehavior::Number)
}

/// Where the bindings land, relative to the crate root (`src-tauri/`).
#[cfg(debug_assertions)]
const BINDINGS_PATH: &str = "../src/lib/bindings.ts";

/// Regenerates `src/lib/bindings.ts`.
///
/// This is why the frontend needs no Rust toolchain and no display server to
/// get bindings: `cargo test -p lychi-app` writes them. CI regenerates and
/// diffs against the committed copy, so a command added without regenerating
/// fails loudly instead of drifting silently.
#[cfg(all(test, debug_assertions))]
#[test]
fn export_bindings() {
    specta_builder()
        .export(ts_config(), BINDINGS_PATH)
        .expect("failed to export typescript bindings");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Kept alive for the whole program — dropping it flushes and stops the
    // non-blocking file-log worker, so buffered logs would be lost on exit.
    let _log_guard = logging::init();
    // Periodic resource/health snapshot (memory, threads, fds) to the log, so
    // beta reports of sluggishness/RAM growth have data behind them.
    logging::spawn_health_monitor(std::time::Duration::from_secs(300));

    platform::init_app();

    let app_state = AppState::new();
    let (hotkey, window_strategy, shell_path, project_dirs) = {
        let config = app_state.config.blocking_read();
        // Apply all context-detection config (extra terminals/IDEs/markers +
        // pinned workspace) atomically through one owned entry point.
        lychi_core::context::config::ContextConfig {
            extra_terminals: config.commands.extra_terminals.clone(),
            extra_ides: config.commands.extra_ides.clone(),
            extra_strong_markers: config.projects.extra_strong_markers.clone(),
            extra_soft_markers: config.projects.extra_soft_markers.clone(),
            pinned_workspace: config.projects.pinned_workspace.clone(),
        }
        .apply();
        (
            config.general.hotkey.clone(),
            config.general.window_strategy.clone(),
            config.commands.shell.clone(),
            config.projects.directories.clone(),
        )
    };

    // Typesafe IPC: tauri-specta collects command + type signatures and (in debug)
    // exports them to `src/lib/bindings.ts`, so the frontend can't drift from Rust.
    let specta_builder = specta_builder();

    // Keeps bindings fresh during `cargo tauri dev`. The same export also runs
    // as a test (see export_bindings), which is what CI and a fresh clone use —
    // neither can start a GUI app.
    #[cfg(debug_assertions)]
    specta_builder
        .export(ts_config(), BINDINGS_PATH)
        .expect("failed to export typescript bindings");

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // Handle deep-link callbacks from another instance (Linux single-instance)
            for arg in &args {
                if arg.starts_with("lychi://") {
                    let app_handle = app.clone();
                    let url = arg.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) =
                            commands::firebase_auth::handle_auth_callback(&app_handle, &url).await
                        {
                            tracing::warn!("deep-link auth callback failed: {e}");
                        }
                    });
                    continue;
                }
            }
            if let Some(w) = app.get_webview_window("main") {
                window::toggle_window(&w);
            }
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_deep_link::init())
        // Autostart entry launches with --hidden so login doesn't pop the launcher
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .manage(app_state)
        // Command handlers come from the tauri-specta builder (single source of
        // truth for both dispatch and the generated TS bindings).
        .invoke_handler(specta_builder.invoke_handler())
        .setup(move |app| {
            // Wire specta events into the app (no-op today; ready for typed events).
            specta_builder.mount_events(app);
            let handle = app.handle().clone();

            // Deep-link handler for lychi:// callbacks (Firebase auth etc.)
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let auth_handle = handle.clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        let url_str = url.to_string();
                        if url_str.starts_with("lychi://auth-callback") {
                            let h = auth_handle.clone();
                            tauri::async_runtime::spawn(async move {
                                if let Err(e) =
                                    commands::firebase_auth::handle_auth_callback(&h, &url_str)
                                        .await
                                {
                                    tracing::warn!("auth callback handler failed: {e}");
                                }
                            });
                        }
                    }
                });

                // On Linux, register the URL scheme at runtime for dev builds
                #[cfg(target_os = "linux")]
                {
                    if let Err(e) = app.deep_link().register_all() {
                        tracing::warn!("deep-link register_all failed: {e}");
                    }
                }
            }

            // Platform-specific window setup (layer-shell, skip-taskbar, etc.)
            if let Some(win) = app.get_webview_window("main") {
                let app_state = app.state::<AppState>();
                // Before anything renders: turn off the WebKit subsystems we
                // don't use, so a host missing GStreamer can't kill the
                // WebProcess and leave a blank window behind.
                platform::harden_webview(&win);
                platform::init_window(&win, &window_strategy);
                platform::setup_dismiss_on_blur(
                    &win,
                    app_state.dismiss_armed.clone(),
                    app_state.summon_seq.clone(),
                    app_state.armed_seq.clone(),
                );
                platform::setup_escape_handler(&win);
                // --hidden: autostarted at login — stay in the background
                // (tray + hotkey/`lychi --toggle` summon it later).
                if !std::env::args().any(|a| a == "--hidden") {
                    window::show_window(&win);
                }
            }

            // Warm up caches in parallel for snappy first search
            tauri::async_runtime::spawn_blocking(|| {
                lychi_core::file_search::warmup_fs_cache();
            });
            // Eagerly walk the home corpus so the first `@` reference (and `/`
            // search) matches against a warm path set instead of triggering a
            // cold walk. Only the paths are shared; each search still gets its
            // own matcher.
            {
                let live = app.state::<AppState>().live_search.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    if let Some(home) = dirs::home_dir() {
                        live.corpus(&home.to_string_lossy());
                    }
                });
            }
            tauri::async_runtime::spawn_blocking(|| {
                lychi_core::action_registry::handlers::app_launcher::AppLauncher::warmup();
            });
            // Bulk icon-path resolution runs on its own low-priority OS thread —
            // NOT spawn_blocking — so this ~seconds-long pass (on desktops with
            // many installed icon themes) never contends for the Tokio blocking
            // pool and can't starve the first IPC. Query-time resolution is lazy
            // and self-caching, so this is purely a "keep first hits warm" pass.
            std::thread::Builder::new()
                .name("lychi-icon-prewarm".to_string())
                .spawn(|| {
                    lychi_core::action_registry::handlers::app_launcher::AppLauncher::warmup_icons_background();
                })
                .expect("failed to spawn icon prewarm thread");
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

            // Subscribe config reactors to the event bus (state-change fan-out).
            app.state::<state::AppState>().wire_reactors(app.handle().clone());

            // Warm alias cache for transparent alias resolution in router
            let alias_db = app.state::<state::AppState>().db.clone();
            lychi_core::aliases::store::warm_cache(&alias_db);

            // Pre-fetch exchange rates for currency conversion (fire-and-forget)
            tauri::async_runtime::spawn(async {
                lychi_core::action_registry::handlers::calc::fetch_exchange_rates().await;
            });

            // Ensure clipboard images directory exists
            std::fs::create_dir_all(lychi_core::paths::clipboard_images_dir()).ok();

            // Orphan cleanup: remove image files not referenced by any clipboard entry
            {
                let orphan_db = app.state::<AppState>().db.clone();
                let store = lychi_core::clipboard::store::ClipboardStore::new();
                if let Ok(paths) = store.collect_image_paths(&orphan_db) {
                    lychi_core::clipboard::image_utils::cleanup_orphans(&paths);
                }
            }

            // Background clipboard monitor — owns its OS thread (not the Tokio blocking pool)
            let clip_db = app.state::<AppState>().db.clone();
            let clipboard_running = app.state::<AppState>().clipboard_running.clone();
            std::thread::Builder::new()
                .name("lychi-clipboard".to_string())
                .spawn(move || {
                    lychi_core::clipboard::store::run_clipboard_monitor(clip_db, clipboard_running);
                })
                .expect("failed to spawn clipboard monitor thread");

            // Warm up local AI (if it's the active mode) so the first query isn't
            // slow — loads the multi-GB model in the background, status via event.
            {
                let ai = app.state::<AppState>().config.blocking_read().ai.clone();
                AppState::warmup_local_ai(app.handle(), &ai);
            }

            // App-index filesystem watcher — rebuilds AppIndex when .desktop files change
            let watcher_running = app.state::<AppState>().app_index_watcher_running.clone();
            lychi_core::desktop_apps::watcher::start(watcher_running);

            // Script Commands directory watcher — re-registers the `script` handler
            // + keywords when files in ~/.config/lychi/scripts/ change. The rebuild
            // action is injected (this keeps lychi-core Tauri-free): re-scan, then
            // take the executor write lock and re-register (mirrors CommandsReactor).
            {
                let scripts_running = app.state::<AppState>().scripts_watcher_running.clone();
                let executor = app.state::<AppState>().executor.clone();
                let config = app.state::<AppState>().config.clone();
                let on_change: std::sync::Arc<dyn Fn() + Send + Sync> =
                    std::sync::Arc::new(move || {
                        let scripts = lychi_core::script_commands::discover(
                            &lychi_core::paths::scripts_dir(),
                        );
                        let keywords: Vec<String> =
                            scripts.iter().map(|s| s.keyword.clone()).collect();
                        let shell = config.blocking_read().commands.shell.clone();
                        let mut ex = executor.blocking_write();
                        ex.registry.register(Box::new(
                            lychi_core::action_registry::handlers::script_commands::ScriptCommandsHandler::new(
                                scripts, shell,
                            ),
                        ));
                        ex.set_script_keywords(keywords);
                        tracing::info!("[scripts] reloaded ({} commands)", ex.script_keyword_count());
                    });
                lychi_core::script_commands::watcher::start(
                    lychi_core::paths::scripts_dir(),
                    scripts_running,
                    on_change,
                );
            }

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

            // Register the global shortcut.
            // - Wayland: via the XDG GlobalShortcuts portal (hotkey_portal.rs)
            //   — compositor-level, full coverage, one-time consent dialog.
            //   The X11 plugin is NOT used there (XWayland-only coverage and
            //   double-fires alongside portal/DE bindings).
            // - X11: via tauri-plugin-global-shortcut as before.
            if lychi_core::context::is_wayland() {
                // Self-register the app-id desktop file the portal needs (AppImage
                // installs nothing; RPM/deb do it at install time — harmless to
                // re-check). Must exist BEFORE the portal registration below.
                hotkey_portal::ensure_app_desktop_file();
                let portal_handle = app.handle().clone();
                let portal_hotkey = hotkey.clone();
                tauri::async_runtime::spawn(async move {
                    hotkey_portal::setup(portal_handle, portal_hotkey).await;
                });
            } else {
                use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
                let hotkey_str = hotkey.clone();
                let registration = app
                    .global_shortcut()
                    .on_shortcut(hotkey.as_str(), move |app, _shortcut, event| {
                        if event.state == ShortcutState::Pressed
                            && let Some(w) = app.get_webview_window("main")
                        {
                            window::toggle_window(&w);
                        }
                    });
                match registration {
                    Ok(()) => {
                        app.state::<AppState>()
                            .hotkey_registered
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        tracing::info!("Global shortcut registered: {hotkey_str}");
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Global shortcut registration failed for {hotkey_str}: {e} — use `lychi --toggle` via a system shortcut"
                        );
                    }
                }

                // An X11 grab succeeding is NOT the same as owning the key. A
                // grab cannot override a combination the window manager already
                // claims: the grab returns Ok, we log "registered", and the WM
                // keeps delivering the key to itself — so the launcher never
                // opens. A tester hit exactly this on XFCE with Super+Space.
                //
                // So also write the binding into the desktop's own settings,
                // where we become the owner rather than a competing grabber.
                // Cheap, idempotent, and it refuses to clobber a shortcut that
                // belongs to something else.
                match hotkey_de::register(&hotkey_str) {
                    hotkey_de::Outcome::Registered => {
                        app.state::<AppState>()
                            .hotkey_registered
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        tracing::info!("[hotkey] {hotkey_str} bound in the desktop's settings");
                    }
                    hotkey_de::Outcome::Conflict(owner) => {
                        tracing::warn!(
                            "[hotkey] {hotkey_str} is already bound to {owner:?} — left alone. \
                             Pick another key in Settings, or bind `lychi --toggle` yourself"
                        );
                    }
                    hotkey_de::Outcome::Unsupported => {
                        tracing::debug!(
                            "[hotkey] no desktop-settings integration for this session; \
                             relying on the X11 grab"
                        );
                    }
                    hotkey_de::Outcome::Failed(e) => {
                        tracing::warn!("[hotkey] could not write the desktop shortcut: {e}");
                    }
                }
            }

            // System tray. Non-fatal: libayatana-appindicator is dlopen'd at
            // runtime and not bundled into the AppImage — on distros without
            // it the tray fails, but the launcher itself must keep working.
            let tray_result = (|| -> Result<(), Box<dyn std::error::Error>> {
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
                Ok(())
            })();
            if let Err(e) = tray_result {
                tracing::warn!(
                    "System tray unavailable: {e} — continuing without tray (is libayatana-appindicator installed?)"
                );
            }

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

            // Spawn KWin active-window watcher (keeps context cache warm).
            // KDE Wayland only — the KWin D-Bus scripting API doesn't exist on
            // GNOME/wlroots, and the watcher would burn a temp-file write plus
            // a doomed D-Bus call every poll.
            #[cfg(target_os = "linux")]
            if lychi_core::context::is_kde_wayland_session() {
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
                state
                    .scripts_watcher_running
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
                    state
                        .scripts_watcher_running
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
