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

/// Owner-only permissions for anything holding user content.
///
/// This one file holds clipboard clips, notes, command history and AI
/// conversations. `Database::create` uses the process umask, which on a typical
/// distro yields `0644` — world-readable, so any other local user or any daemon
/// running as another uid can read the lot. Measured `0644` on a real install
/// before this was added.
#[cfg(unix)]
const OWNER_ONLY_FILE: u32 = 0o600;
#[cfg(unix)]
const OWNER_ONLY_DIR: u32 = 0o700;

/// Narrow `path` to owner-only. Applied to files we already created, so it
/// repairs existing installs rather than only protecting fresh ones.
///
/// Best-effort and non-fatal: on a filesystem that cannot represent the mode
/// (a FAT-formatted `$HOME`, some network mounts) refusing to start would be a
/// worse outcome than a warning.
#[cfg(unix)]
fn restrict(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(md) if md.permissions().mode() & 0o777 == mode => {}
        Ok(_) => {
            if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
                tracing::warn!(
                    "[db] could not restrict {} to {mode:o}: {e}",
                    path.display()
                );
            }
        }
        Err(e) => tracing::warn!("[db] could not stat {}: {e}", path.display()),
    }
}

#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) {}

/// Remove the empty `lychi.db` left beside `lychi.redb` by the pre-redb SQLite
/// era. Nothing has referenced it since the redb migration, so it is pure
/// confusion in the data directory — someone debugging reasonably assumes the
/// `.db` file is the database.
///
/// **Only ever deletes a zero-byte regular file.** If it has any content it is
/// left untouched and reported: a non-empty file is data this function has no
/// business destroying, whatever its name suggests.
fn remove_stale_sqlite_artifact(redb_path: &Path) {
    let stale = redb_path.with_extension("db");
    if stale == redb_path {
        return;
    }
    let Ok(meta) = std::fs::metadata(&stale) else {
        return; // Not present — the common case after the first cleanup.
    };
    if !meta.is_file() {
        return;
    }
    if meta.len() != 0 {
        tracing::warn!("[db] {} is not empty; leaving it alone", stale.display());
        return;
    }
    match std::fs::remove_file(&stale) {
        Ok(()) => tracing::info!("[db] removed empty legacy {}", stale.display()),
        Err(e) => tracing::warn!("[db] could not remove {}: {e}", stale.display()),
    }
}

/// Open (or create) the redb database at the given path.
/// If the file exists but uses an older format version, back it up and recreate.
pub fn open_database(path: &Path) -> Result<Arc<Database>, LychiError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        restrict(parent, OWNER_ONLY_DIR);
    }
    remove_stale_sqlite_artifact(path);

    let db = match Database::create(path) {
        Ok(db) => db,
        Err(e) if path.exists() => {
            tracing::warn!("[db] cannot open database ({e}), backing up and recreating");
            let backup = path.with_extension("redb.bak");
            let _ = std::fs::rename(path, &backup);
            // The backup is a full copy of the same user content.
            #[cfg(unix)]
            restrict(&backup, OWNER_ONLY_FILE);
            Database::create(path)?
        }
        Err(e) => return Err(e.into()),
    };
    #[cfg(unix)]
    restrict(path, OWNER_ONLY_FILE);

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

/// Decode one row of a list, skipping it if it cannot be read.
///
/// **A list must not vanish because one row of it is unreadable.** Every
/// `get_*` here used `?` inside the iteration, so a single undecodable row
/// aborted the whole query and the user saw "all my notes are gone" rather
/// than "one note is corrupt" — with the other 99 still perfectly intact on
/// disk, and no way to reach them.
///
/// This matters most on **downgrade**: postcard is not self-describing, so a
/// row written by a newer schema is not detectably different from garbage. A
/// user who tries a new version and rolls back should lose the rows that
/// changed shape, not the feature.
///
/// Returns `None` for a bad row and logs once with enough context to find it.
/// Callers use it in a `filter_map`, so skipping is the default and aborting
/// has to be spelled out.
///
/// `Config::load_or_default` follows the same principle at the file level: back
/// up, log, carry on with what still works.
///
/// # Not for single-row lookups
///
/// `update_note(id)`, `delete_alias(name)` and friends deliberately keep `?`.
/// There the user named one row, so "that row is corrupt" is both true and
/// usefully scoped — silently succeeding on a row we could not read would be
/// worse. The rule is about **lists**, where one bad row must not stand in for
/// all the good ones.
pub fn decode_row<'a, T: serde::Deserialize<'a>>(
    table: &'static str,
    key: &str,
    bytes: &'a [u8],
) -> Option<T> {
    match postcard::from_bytes(bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            // The key, not the value: the value may be user content, and this
            // goes to a log file. The key is enough to find or delete the row.
            tracing::warn!(
                "[db] skipping undecodable row in `{table}` (key {key}): {e} — \
                 the rest of the list is unaffected"
            );
            None
        }
    }
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

#[cfg(all(test, unix))]
mod permission_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_new_database_is_not_readable_by_other_users() {
        // This file holds clipboard clips, notes and history. The default umask
        // produced 0644 on a real install, which is what this pins shut.
        let dir = std::env::temp_dir().join(format!("lychi-perm-{}", new_id()));
        let path = dir.join("lychi.redb");
        let _db = open_database(&path).unwrap();

        assert_eq!(mode_of(&path), 0o600, "database must be owner-only");
        assert_eq!(mode_of(&dir), 0o700, "data dir must be owner-only");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_existing_world_readable_database_is_repaired_on_open() {
        // Installs created before this existed are already 0644 on disk; opening
        // must fix them, not just protect fresh ones.
        let dir = std::env::temp_dir().join(format!("lychi-perm-{}", new_id()));
        let path = dir.join("lychi.redb");
        drop(open_database(&path).unwrap());

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(mode_of(&path), 0o644, "precondition: loosened");

        let _db = open_database(&path).unwrap();
        assert_eq!(mode_of(&path), 0o600, "reopening must repair permissions");

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod stale_artifact_tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "lychi-stale-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn empty_legacy_file_is_removed() {
        let d = tmpdir("empty");
        let redb = d.join("lychi.redb");
        let stale = d.join("lychi.db");
        std::fs::write(&stale, b"").unwrap();

        remove_stale_sqlite_artifact(&redb);

        assert!(!stale.exists(), "empty legacy file should be removed");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The safety property that matters: a file with content is DATA, and this
    /// cleanup must never destroy it no matter what it is called.
    #[test]
    fn non_empty_legacy_file_is_left_alone() {
        let d = tmpdir("nonempty");
        let redb = d.join("lychi.redb");
        let stale = d.join("lychi.db");
        std::fs::write(&stale, b"SQLite format 3\0real user data").unwrap();

        remove_stale_sqlite_artifact(&redb);

        assert!(stale.exists(), "non-empty file must NOT be deleted");
        assert_eq!(
            std::fs::read(&stale).unwrap(),
            b"SQLite format 3\0real user data"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_legacy_file_is_a_no_op() {
        let d = tmpdir("missing");
        remove_stale_sqlite_artifact(&d.join("lychi.redb"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_real_database_is_never_touched() {
        let d = tmpdir("realdb");
        let redb = d.join("lychi.redb");
        std::fs::write(&redb, b"redb contents").unwrap();

        remove_stale_sqlite_artifact(&redb);

        assert!(redb.exists(), "the live database must survive");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_directory_named_lychi_db_is_left_alone() {
        let d = tmpdir("dir");
        let stale = d.join("lychi.db");
        std::fs::create_dir_all(&stale).unwrap();

        remove_stale_sqlite_artifact(&d.join("lychi.redb"));

        assert!(stale.is_dir(), "a directory must not be removed");
        let _ = std::fs::remove_dir_all(&d);
    }
}
