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
    txn.commit()?;

    Ok(Arc::new(db))
}

/// Create an in-memory database for testing.
#[cfg(test)]
pub fn open_test_database() -> Arc<Database> {
    let dir = std::env::temp_dir().join(format!(
        "lychi-test-{}-{}.redb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    open_database(&dir).expect("Failed to create test database")
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
