use serde::{Deserialize, Serialize};
use tauri_plugin_updater::UpdaterExt;

use lychi_core::backup::{self, BackupKind};
use lychi_core::error::LychiError;
use lychi_core::install::InstallKind;

use crate::state::AppState;

/// What the Settings tab needs to say about updates, in one round trip.
#[derive(Debug, Serialize, Deserialize, specta::Type)]
pub struct UpdateStatus {
    /// Running version.
    pub current_version: String,
    /// How this copy was installed (`appimage` / `flatpak` / `system`).
    pub install_kind: String,
    /// Whether Lychi may update itself here. False for distro packages.
    pub can_self_update: bool,
    /// What to tell the user when it cannot — the command they'd actually run.
    pub hint: String,
    /// Newer version available, if a check has found one.
    pub available_version: Option<String>,
    /// Release notes for that version.
    pub notes: Option<String>,
    /// Set when a check could not complete (offline, no endpoint configured).
    pub error: Option<String>,
}

fn base_status() -> UpdateStatus {
    let kind = InstallKind::detect();
    UpdateStatus {
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        install_kind: kind.as_str().to_string(),
        can_self_update: kind.can_self_update(),
        hint: kind.update_hint().to_string(),
        available_version: None,
        notes: None,
        error: None,
    }
}

/// Current version + how this copy is managed. Never hits the network, so the
/// tab can render instantly and check for updates only when asked.
#[tauri::command]
#[specta::specta]
pub async fn update_status() -> Result<UpdateStatus, LychiError> {
    Ok(base_status())
}

/// Ask the update endpoint whether something newer exists.
///
/// On a distro package this returns the status unchanged rather than checking:
/// telling a `dnf` user that 0.2.0 exists, with no way to install it from here,
/// is noise — their package manager will tell them in its own time.
#[tauri::command]
#[specta::specta]
pub async fn check_for_update(app: tauri::AppHandle) -> Result<UpdateStatus, LychiError> {
    let mut status = base_status();
    if !status.can_self_update {
        return Ok(status);
    }

    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => {
            // The usual cause is no `pubkey`/`endpoints` configured yet — a
            // real state before the first signed release, not a user error.
            status.error = Some(format!("Updates are not configured: {e}"));
            return Ok(status);
        }
    };

    match updater.check().await {
        Ok(Some(update)) => {
            status.available_version = Some(update.version.clone());
            status.notes = update.body.clone();
        }
        Ok(None) => {}
        Err(e) => status.error = Some(format!("Could not check for updates: {e}")),
    }
    Ok(status)
}

/// Download and install the available update, then relaunch.
///
/// **Takes a backup first.** An update is the single most likely moment for a
/// migration to eat data, and it is precisely when the user is not thinking
/// about backups. The snapshot is stamped with the CURRENT version, because
/// that is whose data it holds.
#[tauri::command]
#[specta::specta]
pub async fn install_update(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), LychiError> {
    let kind = InstallKind::detect();
    if !kind.can_self_update() {
        return Err(LychiError::ExecutionFailed(kind.update_hint().to_string()));
    }

    let updater = app
        .updater()
        .map_err(|e| LychiError::ExecutionFailed(format!("Updates are not configured: {e}")))?;

    let update = updater
        .check()
        .await
        .map_err(|e| LychiError::ExecutionFailed(format!("Could not check for updates: {e}")))?
        .ok_or_else(|| LychiError::ExecutionFailed("Already up to date".into()))?;

    let current = env!("CARGO_PKG_VERSION");
    match backup::create(
        &state.db,
        BackupKind::Automatic,
        &format!("before updating to {}", update.version),
        current,
    ) {
        Ok(b) => tracing::info!("[update] pre-update backup saved: {}", b.name),
        // Do not block the update on a failed backup — but say so loudly. The
        // user asked to update; refusing over a backup they cannot see would be
        // its own surprise.
        Err(e) => tracing::error!("[update] pre-update backup FAILED, continuing: {e}"),
    }

    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| LychiError::ExecutionFailed(format!("Update failed: {e}")))?;

    app.restart();
}
