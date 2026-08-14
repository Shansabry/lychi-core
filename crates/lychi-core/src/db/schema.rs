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
    #[serde(default)]
    pub image: Option<ClipboardImageMeta>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ClipboardImageMeta {
    /// Absolute path to PNG file on disk.
    pub path: String,
    pub width: u32,
    pub height: u32,
    /// Base64-encoded PNG thumbnail (max 48x48) for inline display.
    pub thumb_b64: String,
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

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ReminderEntry {
    pub text: String,
    pub due_at: u64,
    #[serde(default)]
    pub fired: bool,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub deleted_at: Option<u64>,
    #[serde(default)]
    pub sync_status: SyncStatus,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SnippetEntry {
    pub name: String,
    pub body: String,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub deleted_at: Option<u64>,
    #[serde(default)]
    pub sync_status: SyncStatus,
}

/// A user-defined AI prompt preset (Phase 3 "AI Commands"). `keyword` is the
/// typed shortcut that invokes it; `template` is a prompt with a `{input}`
/// placeholder replaced by whatever the user typed after the keyword.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AiPresetEntry {
    pub keyword: String,
    pub name: String,
    pub template: String,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub deleted_at: Option<u64>,
    #[serde(default)]
    pub sync_status: SyncStatus,
}

/// A user-pinned zero-state row. `run` keeps the exact-case command to execute;
/// the table key is its normalized form (see `pins::normalize_run`), so unpin
/// and is-pinned are O(1) lookups from any row's run string. `position` carries
/// explicit pin order — table keys are lexicographic, not insertion-ordered.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PinEntry {
    pub run: String,
    pub label: String,
    pub position: u32,
    pub created_at: u64,
    #[serde(default)]
    pub deleted_at: Option<u64>,
    #[serde(default)]
    pub sync_status: SyncStatus,
}

/// Persisted timer/stopwatch, so a running countdown survives an app restart.
///
/// The live `Timer` uses a monotonic `Instant` (not persistable across process
/// restarts), so we serialize the equivalent in **wall-clock** terms:
/// `elapsed_before_ms` is time already accumulated (from prior runs/pauses), and
/// `running_since_epoch_ms` is the wall-clock instant the current run began — or
/// `None` if paused. On load we reconstruct the `Instant` by subtracting the
/// wall-clock elapsed-since-that-epoch from `Instant::now()`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TimerEntry {
    pub name: String,
    /// Total countdown duration in seconds; 0 means a stopwatch (counts up).
    pub duration_secs: u64,
    /// Elapsed seconds accumulated before the current run (from pauses).
    pub elapsed_before_secs: f64,
    /// Wall-clock epoch (ms) when the current run started; None if paused.
    pub running_since_epoch_ms: Option<u64>,
}
