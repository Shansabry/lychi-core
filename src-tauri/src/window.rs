use std::path::PathBuf;
use tauri::{Emitter, WebviewWindow};

/// Returns the path for the Unix domain socket.
/// Matches the path used by lychi-cli.
pub fn socket_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/tmp/lychi-{}", unsafe { libc::getuid() }));
    PathBuf::from(runtime_dir).join("lychi.sock")
}

/// Toggle the launcher window visibility.
pub fn toggle_window(window: &WebviewWindow) {
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
    } else {
        show_window(window);
    }
}

/// Show the window, focus it, and notify the frontend.
fn show_window(window: &WebviewWindow) {
    if let Err(e) = window.show() {
        tracing::error!("Failed to show window: {e}");
        return;
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(gtk_win) = window.gtk_window() {
            use gtk::prelude::GtkWindowExt;
            gtk_win.set_keep_above(true);
            gtk_win.present();
        }
    }

    if let Err(e) = window.set_focus() {
        tracing::error!("Failed to focus window: {e}");
    }

    // Tell frontend to reset state (clear input, refocus input element)
    let _ = window.emit("lychi://summon", ());
}
