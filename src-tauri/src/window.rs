use tauri::{Emitter, WebviewWindow};

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

    crate::platform::focus_window(window);

    if let Err(e) = window.set_focus() {
        tracing::error!("Failed to focus window: {e}");
    }

    // Tell frontend to reset state (clear input, refocus input element)
    let _ = window.emit("lychi://summon", ());
}
