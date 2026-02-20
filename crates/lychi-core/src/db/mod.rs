pub mod schema;

use std::path::Path;
use std::sync::Arc;

use redb::{Database, ReadableTableMetadata, TableDefinition};

use crate::error::LychiError;

/// History: key = UUID v7 string (time-ordered), value = postcard-serialized HistoryEntry.
pub const HISTORY: TableDefinition<&str, &[u8]> = TableDefinition::new("history");

/// Notes: key = UUID v7 string, value = postcard-serialized NoteEntry.
pub const NOTES: TableDefinition<&str, &[u8]> = TableDefinition::new("notes");

/// Todos: key = UUID v7 string, value = postcard-serialized TodoEntry.
pub const TODOS: TableDefinition<&str, &[u8]> = TableDefinition::new("todos");

/// Settings: key = dotted path (e.g. "general.theme"), value = postcard-serialized SettingEntry.
pub const SETTINGS: TableDefinition<&str, &[u8]> = TableDefinition::new("settings");

/// Open (or create) the redb database at the given path.
pub fn open_database(path: &Path) -> Result<Arc<Database>, LychiError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db = Database::create(path)?;

    // Ensure all tables exist by opening them in a write transaction.
    let txn = db.begin_write()?;
    txn.open_table(HISTORY)?;
    txn.open_table(NOTES)?;
    txn.open_table(TODOS)?;
    txn.open_table(SETTINGS)?;
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
    pub settings: u64,
}

/// Get row counts for all tables.
pub fn table_stats(db: &Arc<Database>) -> Result<TableStats, LychiError> {
    let txn = db.begin_read()?;
    Ok(TableStats {
        history: txn.open_table(HISTORY)?.len()?,
        notes: txn.open_table(NOTES)?.len()?,
        todos: txn.open_table(TODOS)?.len()?,
        settings: txn.open_table(SETTINGS)?.len()?,
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
