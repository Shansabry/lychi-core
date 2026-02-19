use lychi_core::error::LychiError;

/// Expand `file://~/...` to `file:///home/user/...`.
fn expand_file_uri(uri: &str) -> String {
    if let Some(rest) = uri.strip_prefix("file://~/")
        && let Some(home) = dirs::home_dir()
    {
        return format!("file://{}/{rest}", home.display());
    }
    uri.to_string()
}

/// Open a URI using the platform's native mechanism with proper
/// desktop activation (e.g. Wayland activation tokens on Linux).
#[tauri::command]
pub async fn open_uri(uri: String) -> Result<(), LychiError> {
    let uri = expand_file_uri(&uri);
    crate::platform::open_uri(&uri)
        .await
        .map_err(LychiError::ExecutionFailed)
}
