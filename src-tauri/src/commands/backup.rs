use tauri::State;

use lychi_core::backup::{self, BackupInfo, BackupKind, RestoreReport};
use lychi_core::error::LychiError;

use crate::state::AppState;

/// The running version, used to stamp new backups and to decide whether an
/// existing one is restorable.
fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[tauri::command]
#[specta::specta]
pub async fn list_backups() -> Result<Vec<BackupInfo>, LychiError> {
    Ok(backup::list())
}

#[tauri::command]
#[specta::specta]
pub async fn create_backup(
    reason: Option<String>,
    state: State<'_, AppState>,
) -> Result<BackupInfo, LychiError> {
    backup::create(
        &state.db,
        BackupKind::Manual,
        reason.as_deref().unwrap_or("manual backup"),
        app_version(),
    )
}

/// Restore a backup by its filename (the `name` field from `list_backups`).
///
/// The path is rebuilt from the backups directory rather than accepted from the
/// frontend, so a crafted argument cannot make this read an arbitrary file.
#[tauri::command]
#[specta::specta]
pub async fn restore_backup(
    name: String,
    state: State<'_, AppState>,
) -> Result<RestoreReport, LychiError> {
    let entry = backup::list()
        .into_iter()
        .find(|b| b.name == name)
        .ok_or_else(|| LychiError::ExecutionFailed(format!("no such backup: {name}")))?;

    backup::restore(&state.db, std::path::Path::new(&entry.path), app_version())
}

#[tauri::command]
#[specta::specta]
pub async fn delete_backup(name: String) -> Result<(), LychiError> {
    backup::delete(&name)
}

/// Absolute path of the backups directory, so the UI can offer "show in files".
#[tauri::command]
#[specta::specta]
pub async fn backups_dir() -> Result<String, LychiError> {
    Ok(lychi_core::paths::backups_dir()
        .to_string_lossy()
        .into_owned())
}

/// The running version — the UI needs it to mark which backups are restorable.
#[tauri::command]
#[specta::specta]
pub async fn app_version_string() -> Result<String, LychiError> {
    Ok(app_version().to_string())
}
