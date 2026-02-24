use serde::{Deserialize, Serialize};

/// Sync status — reserved for cloud sync (paid feature, not yet implemented).
/// Currently always set to SYNC_LOCAL (0) on every write. When cloud sync ships,
/// the sync engine will transition entries through: 0 → 1 → 2.
///   0 = local only (not yet synced)
///   1 = pending sync (queued for upload)
///   2 = synced (confirmed by server)
pub type SyncStatus = u8;

pub const SYNC_LOCAL: SyncStatus = 0;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HistoryEntry {
    pub command: String,
    #[serde(default)]
    pub deleted_at: Option<u64>,
    #[serde(default)]
    pub sync_status: SyncStatus,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NoteEntry {
    pub text: String,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub deleted_at: Option<u64>,
    #[serde(default)]
    pub sync_status: SyncStatus,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TodoEntry {
    pub text: String,
    #[serde(default)]
    pub done: bool,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub deleted_at: Option<u64>,
    #[serde(default)]
    pub sync_status: SyncStatus,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ClipboardEntry {
    pub text: String,
    pub created_at: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SettingEntry {
    pub value: String,
    pub updated_at: u64,
    #[serde(default)]
    pub sync_status: SyncStatus,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AliasEntry {
    pub name: String,
    pub command: String,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub deleted_at: Option<u64>,
    #[serde(default)]
    pub sync_status: SyncStatus,
}
