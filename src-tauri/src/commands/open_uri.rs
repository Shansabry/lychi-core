use lychi_core::error::LychiError;

/// Open a URI using GDK's AppLaunchContext, which provides proper
/// XDG activation tokens on Wayland so the browser gets focus.
#[tauri::command]
pub async fn open_uri(uri: String) -> Result<(), LychiError> {
    // GDK must be called from the main thread (GLib main loop).
    // Use a oneshot channel to get the result back to the async context.
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

    glib::MainContext::default().invoke(move || {
        let result = (|| {
            let display = gdk::Display::default().ok_or("No GDK display")?;
            let context = display.app_launch_context().ok_or("No AppLaunchContext")?;
            gio::AppInfo::launch_default_for_uri(&uri, Some(&context))
                .map_err(|e| format!("Failed to open URI: {e}"))
        })();
        let _ = tx.send(result);
    });

    rx.await
        .map_err(|_| LychiError::ExecutionFailed("Channel closed".into()))?
        .map_err(LychiError::ExecutionFailed)
}
