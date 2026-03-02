use std::sync::atomic::Ordering;

use tauri::{Emitter, Manager, WebviewWindow};

use crate::state::AppState;

/// Short thread identifier for debug logs.
fn tid() -> String {
    std::thread::current().name().unwrap_or("?").to_string()
}

/// Toggle the launcher window visibility.
pub fn toggle_window(window: &WebviewWindow) {
    if window.is_visible().unwrap_or(false) {
        // Disarm dismiss so focus-out during hide doesn't re-trigger
        let state = window.app_handle().state::<AppState>();
        state.dismiss_armed.store(false, Ordering::SeqCst);
        let _ = window.hide();
    } else {
        show_window(window);
    }
}

/// Show the window, reposition to the correct monitor, focus it, and notify
/// the frontend. Public so lib.rs can call it at startup too.
pub fn show_window(window: &WebviewWindow) {
    // Read monitor_mode from config. blocking_read() is safe here because
    // callers are always on Tokio tasks (shortcut callback, IPC, tray handler),
    // never inside an async executor that would deadlock.
    let monitor_mode = {
        let state = window.app_handle().state::<AppState>();
        state.config.blocking_read().general.monitor_mode.clone()
    };

    // Increment summon sequence — makes stale focus events from previous
    // summon cycles harmless (focus handlers compare against current seq).
    let seq = {
        let state = window.app_handle().state::<AppState>();
        state.summon_seq.fetch_add(1, Ordering::SeqCst) + 1
    };
    tracing::info!("[show] === show_window BEGIN (seq={seq}, t={}) ===", tid());

    // Reset dismiss armed — will be re-armed by user interaction (key/click).
    {
        let state = window.app_handle().state::<AppState>();
        state.dismiss_armed.store(false, Ordering::SeqCst);
    }
    tracing::info!("[show] seq={seq} armed=false");

    // Snapshot the active window BEFORE we show Lychi (otherwise we'd detect ourselves).
    let pre_window = lychi_core::context::snapshot_active_window();

    // Reposition + show + focus all run on the GLib main thread in one invoke.
    {
        let window_clone = window.clone();
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        glib::MainContext::default().invoke(move || {
            let t = std::thread::current().name().unwrap_or("?").to_string();

            tracing::info!("[show] seq={seq} t={t} reposition BEGIN");
            if let Some(monitor) = crate::platform::get_monitor_for_mode(&monitor_mode) {
                crate::platform::reposition_to_monitor(&window_clone, &monitor);
            }
            tracing::info!("[show] seq={seq} t={t} reposition END");

            tracing::info!("[show] seq={seq} t={t} window.show()");
            if let Err(e) = window_clone.show() {
                tracing::error!("Failed to show window: {e}");
                let _ = tx.send(false);
                return;
            }

            tracing::info!("[show] seq={seq} t={t} focus_window()");
            crate::platform::focus_window(&window_clone);

            tracing::info!("[show] seq={seq} t={t} set_focus()");
            if let Err(e) = window_clone.set_focus() {
                tracing::error!("Failed to focus window: {e}");
            }

            let _ = tx.send(true);
        });
        let ok = rx.recv().unwrap_or(false);
        if !ok {
            return;
        }
    }

    tracing::info!("[show] seq={seq} emitting lychi://summon");
    let _ = window.emit("lychi://summon", ());
    tracing::info!("[show] === show_window END (seq={seq}) ===");

    // Fast path: emit last-known context immediately so suggestions appear
    // before the fresh gather completes (typically saves 50-200ms).
    // Skip if the active window changed — stale context causes a suggestion flash.
    {
        let state = window.app_handle().state::<AppState>();
        if let Ok(executor) = state.executor.try_read()
            && let Some(ref cached_ctx) = executor.context
        {
            let same_window = match (&pre_window, &cached_ctx.active_window) {
                (Some(pre), Some(cached)) => pre.wm_class == cached.wm_class,
                (None, None) => true,
                _ => false,
            };
            if same_window {
                tracing::debug!(
                    "Fast path: emitting cached context ({}ms old)",
                    cached_ctx.gather_ms
                );
                let _ = window.emit("lychi://context-ready", cached_ctx);
            } else {
                tracing::debug!("Fast path: skipped (window changed)");
            }
        }
    }

    // Gather fresh context asynchronously (never blocks summon).
    // Result replaces the cached context and is re-emitted to frontend.
    let ctx_handle = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let ctx = lychi_core::context::gather(pre_window);
        tracing::info!(
            "Context gathered in {}ms: window={}, cwd={}, terminal_cwd={}, git={}, project={}, docker={}",
            ctx.gather_ms,
            ctx.active_window
                .as_ref()
                .map(|w| w.wm_class.as_str())
                .unwrap_or("none"),
            ctx.cwd.as_deref().unwrap_or("none"),
            ctx.terminal_cwd.as_deref().unwrap_or("none"),
            ctx.git
                .as_ref()
                .map(|g| g.branch.as_str())
                .unwrap_or("none"),
            ctx.project
                .as_ref()
                .map(|p| format!("{:?}", p.kind))
                .unwrap_or_else(|| "none".into()),
            ctx.docker
                .as_ref()
                .map(|d| d.containers.len().to_string())
                .unwrap_or_else(|| "none".into()),
        );

        // Store in executor
        let state = ctx_handle.state::<AppState>();
        if let Ok(mut executor) = state.executor.try_write() {
            executor.context = Some(ctx.clone());
        }

        // Notify frontend that context is ready
        let _ = ctx_handle.emit("lychi://context-ready", &ctx);
    });
}
