use tauri::{Emitter, Manager, WebviewWindow};

use crate::state::AppState;

/// Toggle the launcher window visibility.
pub fn toggle_window(window: &WebviewWindow) {
    if window.is_visible().unwrap_or(false) {
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
}
