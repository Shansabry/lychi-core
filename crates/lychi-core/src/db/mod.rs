pub mod frecency;
pub mod schema;

use std::path::Path;
use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTableMetadata, TableDefinition};

use crate::error::LychiError;

/// History: key = UUID v7 string (time-ordered), value = postcard-serialized HistoryEntry.
pub const HISTORY: TableDefinition<&str, &[u8]> = TableDefinition::new("history");

/// Notes: key = UUID v7 string, value = postcard-serialized NoteEntry.
pub const NOTES: TableDefinition<&str, &[u8]> = TableDefinition::new("notes");

/// Todos: key = UUID v7 string, value = postcard-serialized TodoEntry.
pub const TODOS: TableDefinition<&str, &[u8]> = TableDefinition::new("todos");

/// Clipboard history: key = UUID v7 string (time-ordered), value = postcard-serialized ClipboardEntry.
pub const CLIPBOARD: TableDefinition<&str, &[u8]> = TableDefinition::new("clipboard");

/// Settings: key = dotted path (e.g. "general.theme"), value = postcard-serialized SettingEntry.
pub const SETTINGS: TableDefinition<&str, &[u8]> = TableDefinition::new("settings");

/// Frecency: key = normalized identifier (app name lowercase, file path),
/// value = postcard-serialized FrecencyEntry.
pub const FRECENCY: TableDefinition<&str, &[u8]> = TableDefinition::new("frecency");

/// Aliases: key = alias name (lowercase), value = postcard-serialized AliasEntry.
pub const ALIASES: TableDefinition<&str, &[u8]> = TableDefinition::new("aliases");

/// Reminders: key = UUID v7 string (time-ordered), value = postcard-serialized ReminderEntry.
pub const REMINDERS: TableDefinition<&str, &[u8]> = TableDefinition::new("reminders");

/// Snippets: key = UUID v7 string, value = postcard-serialized SnippetEntry.
pub const SNIPPETS: TableDefinition<&str, &[u8]> = TableDefinition::new("snippets");

/// Timers: key = timer id, value = postcard-serialized TimerEntry. Persisted so
/// running countdowns/stopwatches survive an app restart (rehydrated on boot).
pub const TIMERS: TableDefinition<&str, &[u8]> = TableDefinition::new("timers");

/// AI presets: key = UUID v7 string, value = postcard-serialized AiPresetEntry.
/// User-defined saved prompt templates invoked by keyword (Phase 3 AI Commands).
pub const AI_PRESETS: TableDefinition<&str, &[u8]> = TableDefinition::new("ai_presets");

/// AI conversation history: key = UUID v7 string, value = postcard-serialized
/// ConversationEntry. Completed agent conversations, recallable via `chat`
/// (Phase 4). Capped + pruned so the DB doesn't grow unbounded.
pub const AI_CONVERSATIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("ai_conversations");

/// Learned model capabilities: key = `<provider>/<model>`, value =
/// postcard-serialized `ModelCapability`. Populated from provider metadata when
/// the endpoint reports it, and from observed failures otherwise — so Lychi
/// stops re-sending requests a model has already rejected.
pub const MODEL_CAPS: TableDefinition<&str, &[u8]> = TableDefinition::new("model_caps");

/// Open (or create) the redb database at the given path.
/// If the file exists but uses an older format version, back it up and recreate.
pub fn open_database(path: &Path) -> Result<Arc<Database>, LychiError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db = match Database::create(path) {
        Ok(db) => db,
        Err(e) if path.exists() => {
            tracing::warn!("[db] cannot open database ({e}), backing up and recreating");
            let backup = path.with_extension("redb.bak");
            let _ = std::fs::rename(path, &backup);
            Database::create(path)?
        }
        Err(e) => return Err(e.into()),
    };

    // Ensure all tables exist by opening them in a write transaction.
    let txn = db.begin_write()?;
    txn.open_table(HISTORY)?;
    txn.open_table(NOTES)?;
    txn.open_table(TODOS)?;
    txn.open_table(CLIPBOARD)?;
    txn.open_table(SETTINGS)?;
    txn.open_table(FRECENCY)?;
    txn.open_table(ALIASES)?;
    txn.open_table(REMINDERS)?;
    txn.open_table(SNIPPETS)?;
    txn.open_table(TIMERS)?;
    txn.open_table(AI_PRESETS)?;
    txn.open_table(AI_CONVERSATIONS)?;
    txn.commit()?;

    Ok(Arc::new(db))
}

/// Create a throwaway database for testing.
///
/// Uniqueness comes from a process-wide ATOMIC COUNTER, not a timestamp. Two
/// parallel tests can read the same nanosecond, and a shared path means both
/// open the same file — one of them then trips `open_database`'s recover branch
/// (which renames to a single `.redb.bak` path shared by every test) and the
/// whole thing races. A counter cannot collide by construction.
///
/// The returned handle owns its file: when the last `Arc` drops, the file and
/// its siblings are removed. Tests used to leak one database per call — ~2000
/// files and 78 MB of `/tmp` had accumulated, which is also what kept feeding
/// the recover branch.
#[cfg(test)]
pub fn open_test_database() -> Arc<Database> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let path = std::env::temp_dir().join(format!(
        "lychi-test-{}-{}.redb",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    // A previous aborted run may have left this exact path behind (same pid is
    // possible after a crash); start clean so we never hit the recover branch.
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("redb.bak"));

    // Sweep debris from runs that have already exited. Done once per process,
    // and only for OTHER pids — our own files are still open.
    sweep_stale_test_databases();

    open_database(&path).expect("Failed to create test database")
}

/// Remove `lychi-test-*` files left by earlier (already-exited) test runs.
///
/// redb keeps its file open for the life of the `Database`, and tests share the
/// `Arc` freely, so deleting per-test isn't reliable. Sweeping other processes'
/// leftovers on startup is — and it's what stops `/tmp` growing without bound
/// (this had reached ~2000 files / 78 MB before the sweep existed).
#[cfg(test)]
fn sweep_stale_test_databases() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let mine = format!("lychi-test-{}-", std::process::id());
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with("lychi-test-") && !name.starts_with(&mine) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    });
}

/// Row counts for each table (includes soft-deleted rows).
pub struct TableStats {
    pub history: u64,
    pub notes: u64,
    pub todos: u64,
    pub clipboard: u64,
    pub settings: u64,
    pub frecency: u64,
    pub aliases: u64,
    pub reminders: u64,
    pub snippets: u64,
}

/// Get row counts for all tables.
pub fn table_stats(db: &Arc<Database>) -> Result<TableStats, LychiError> {
    let txn = db.begin_read()?;
    Ok(TableStats {
        history: txn.open_table(HISTORY)?.len()?,
        notes: txn.open_table(NOTES)?.len()?,
        todos: txn.open_table(TODOS)?.len()?,
        clipboard: txn.open_table(CLIPBOARD)?.len()?,
        settings: txn.open_table(SETTINGS)?.len()?,
        frecency: txn.open_table(FRECENCY)?.len()?,
        aliases: txn.open_table(ALIASES)?.len()?,
        reminders: txn.open_table(REMINDERS)?.len()?,
        snippets: txn.open_table(SNIPPETS)?.len()?,
    })
}

/// Generate a new UUID v7 string.
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Current time in milliseconds since UNIX epoch.
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
