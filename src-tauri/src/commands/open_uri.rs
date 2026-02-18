use lychi_core::error::LychiError;

/// Open a URI using the platform's native mechanism with proper
/// desktop activation (e.g. Wayland activation tokens on Linux).
#[tauri::command]
pub async fn open_uri(uri: String) -> Result<(), LychiError> {
    crate::platform::open_uri(&uri)
        .await
        .map_err(LychiError::ExecutionFailed)
}
