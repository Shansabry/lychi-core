//! Reveal a file or folder in the system file manager, highlighted in its
//! parent — the "Show in Finder" / "Reveal in Explorer" action.
//!
//! Uses the freedesktop `org.freedesktop.FileManager1.ShowItems` D-Bus method,
//! which every major Linux file manager implements (GNOME Files/Nautilus,
//! Dolphin, Nemo, Thunar, PCManFM). Unlike opening the URI directly — which
//! *enters* a folder or *opens* a file — ShowItems opens the PARENT and selects
//! the item, so the user sees it in context.
//!
//! Falls back to opening the parent directory if the interface is unavailable
//! (a minimal/headless file manager), so the action never silently no-ops.

use lychi_core::error::LychiError;
use std::path::{Path, PathBuf};

/// Expand a leading `~` to the home directory.
fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if path == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    PathBuf::from(path)
}

/// Open `path` with the default application IF it exists on disk. Returns
/// `true` if the path existed and was opened, `false` if it doesn't exist (so
/// the caller can fall back to a normal search). Used for the "paste an absolute
/// path and press Enter" flow, where a real path should open directly rather
/// than be interpreted as a scoped search term.
#[tauri::command]
#[specta::specta]
pub async fn open_path(path: String) -> Result<bool, LychiError> {
    let abs = expand_home(&path);
    // Central path decider: reject malformed / root-escaping paths and act on the
    // cleaned form. A traversal-laden string here signals input that didn't come
    // straight from the user.
    let abs = lychi_core::rules::path::check_path(&abs).map_err(LychiError::ExecutionFailed)?;
    if !abs.exists() {
        return Ok(false);
    }
    let uri = format!("file://{}", abs.display());
    crate::platform::open_uri(&uri)
        .await
        .map_err(LychiError::ExecutionFailed)?;
    Ok(true)
}

/// Reveal `path` in the file manager, selected within its parent directory.
#[tauri::command]
#[specta::specta]
pub async fn reveal_path(path: String) -> Result<(), LychiError> {
    let abs = expand_home(&path);
    let abs = lychi_core::rules::path::check_path(&abs).map_err(LychiError::ExecutionFailed)?;
    let uri = format!("file://{}", abs.display());

    match show_items(&uri).await {
        Ok(()) => Ok(()),
        Err(e) => {
            // Fall back to opening the parent directory (no selection) so the
            // user still lands in the right place even without FileManager1.
            tracing::debug!("[reveal] FileManager1.ShowItems failed ({e}); opening parent");
            let parent = abs.parent().unwrap_or(Path::new("/"));
            let parent_uri = format!("file://{}", parent.display());
            crate::platform::open_uri(&parent_uri)
                .await
                .map_err(LychiError::ExecutionFailed)
        }
    }
}

/// Call `org.freedesktop.FileManager1.ShowItems([uri], "")` on the session bus.
async fn show_items(uri: &str) -> Result<(), String> {
    let connection = zbus::Connection::session()
        .await
        .map_err(|e| format!("session bus: {e}"))?;

    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.FileManager1",
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1",
    )
    .await
    .map_err(|e| format!("proxy: {e}"))?;

    let uris = vec![uri.to_string()];
    let startup_id = String::new();
    proxy
        .call_method("ShowItems", &(uris, startup_id))
        .await
        .map(|_| ())
        .map_err(|e| format!("ShowItems: {e}"))
}
