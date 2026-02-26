use tauri::{Emitter, Manager, WebviewWindow};

use crate::state::AppState;

/// Toggle the launcher window visibility.
pub fn toggle_window(window: &WebviewWindow) {
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        let _ = window.emit("lychi://hidden", ());
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

    // Snapshot the active window BEFORE we show Lychi (otherwise we'd detect ourselves).
    let pre_window = lychi_core::context::snapshot_active_window();

    // Reposition the window to the target monitor BEFORE showing it.
    // GDK/GTK calls must run on the GLib main thread, so dispatch via
    // glib::MainContext and block until complete (same pattern as open_uri).
    {
        let window_clone = window.clone();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        glib::MainContext::default().invoke(move || {
            if let Some(monitor) = crate::platform::get_monitor_for_mode(&monitor_mode) {
                crate::platform::reposition_to_monitor(&window_clone, &monitor);
            }
            let _ = tx.send(());
        });
        // Wait for GLib to complete the reposition before calling show()
        let _ = rx.recv();
    }

    if let Err(e) = window.show() {
        tracing::error!("Failed to show window: {e}");
        return;
    }

    crate::platform::focus_window(window);

    if let Err(e) = window.set_focus() {
        tracing::error!("Failed to focus window: {e}");
    }

    // Tell frontend to reset state (clear input, refocus input element)
    let _ = window.emit("lychi://summon", ());

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
