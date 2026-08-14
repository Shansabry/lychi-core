pub mod frecency;
pub mod schema;

use std::path::Path;
use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};

use crate::error::LychiError;

/// Notes: key = UUID v7 string, value = postcard-serialized NoteEntry.
pub const NOTES: TableDefinition<&str, &[u8]> = TableDefinition::new("notes");

/// Todos: key = UUID v7 string, value = postcard-serialized TodoEntry.
pub const TODOS: TableDefinition<&str, &[u8]> = TableDefinition::new("todos");

/// Settings: key = dotted path (e.g. "general.theme"), value = postcard-serialized SettingEntry.
pub const SETTINGS: TableDefinition<&str, &[u8]> = TableDefinition::new("settings");

/// Frecency: key = normalized identifier (app name lowercase, file path),
/// value = postcard-serialized FrecencyEntry.
///
/// Lives in its OWN database (`frecency.redb`, opened by `frecency::open`), NOT
/// this user-data `lychi.redb` — see the `frecency` module. The table definition
/// stays here because it is the shared row codec's home, but it is created and
/// opened only against the frecency database.
pub const FRECENCY: TableDefinition<&str, &[u8]> = TableDefinition::new("frecency");

/// Aliases: key = alias name (lowercase), value = postcard-serialized AliasEntry.
pub const ALIASES: TableDefinition<&str, &[u8]> = TableDefinition::new("aliases");

/// Reminders: key = UUID v7 string (time-ordered), value = postcard-serialized ReminderEntry.
pub const REMINDERS: TableDefinition<&str, &[u8]> = TableDefinition::new("reminders");

/// Snippets: key = UUID v7 string, value = postcard-serialized SnippetEntry.
pub const SNIPPETS: TableDefinition<&str, &[u8]> = TableDefinition::new("snippets");

/// AI presets: key = UUID v7 string, value = postcard-serialized AiPresetEntry.
/// User-defined saved prompt templates invoked by keyword (Phase 3 AI Commands).
pub const AI_PRESETS: TableDefinition<&str, &[u8]> = TableDefinition::new("ai_presets");

/// User-pinned zero-state rows: key = normalized run string (lowercased,
/// whitespace-collapsed), value = postcard-serialized PinEntry. The user's
/// hand-chosen commands, always shown first on the empty prompt.
pub const PINS: TableDefinition<&str, &[u8]> = TableDefinition::new("pins");

/// Database metadata: key = a reserved name (only "schema_version" today),
/// value = raw bytes. NOT enveloped — this table is how the envelope is found.
pub const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

/// The schema generation this binary writes.
///
/// Every row value in every table (except `META`) is `[SCHEMA_VERSION][body]`.
/// Postcard is positional and not self-describing: without a tag, the first
/// struct-field addition after release makes every pre-existing row of that
/// table undecodable, and `decode_row`'s skip-and-warn turns that into
/// silently empty lists — total perceived data loss, produced by the exact
/// mechanism built to prevent it (verified empirically against pinned
/// postcard: a trailing `#[serde(default)]` field CANNOT fire on buffer
/// exhaustion; the attribute is JSON-era comfort, not postcard evolution).
///
/// The contract this tag buys, forever:
/// - **Shape change** ⇒ bump this constant and add a migration arm in
///   [`body_of`] (decode old shape, convert) or a rewrite step in
///   [`migrate`]. Old rows keep working.
/// - **Downgrade** ⇒ an older binary sees a newer tag, skips THAT row, and
///   keeps every row it does understand — mixed-generation tables degrade
///   per-row, never wholesale.
/// - All 60-odd codec sites go through [`encode_row`]/[`decode_row`]/
///   [`decode_value`] (a source-scan test bans raw `postcard::` row codecs
///   outside this module), so no writer can produce an untagged row again.
pub const SCHEMA_VERSION: u8 = 1;

/// Every enveloped table — the migration and any future whole-table rewrite
/// iterate this list, so a new table added here is versioned from birth.
pub(crate) const ENVELOPED_TABLES: [&str; 8] = [
    "notes",
    "todos",
    "settings",
    "aliases",
    "reminders",
    "snippets",
    "ai_presets",
    "pins",
];

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
/// Whether a `DatabaseError` means the on-disk file is genuinely corrupt and
/// won't get better — the ONLY case where renaming it away and starting fresh is
/// the right call. Everything else (a pending format upgrade, a transient I/O
/// error like a full disk, a poisoned lock) is fixable, and must surface as an
/// error rather than cost the user their data.
///
/// The subtlety is redb's `Io` variant: it is OVERLOADED. A structurally-invalid
/// file (garbage / truncated header) surfaces as `Io(InvalidData)` /
/// `Io(UnexpectedEof)` — that IS corruption. But a genuine transient failure
/// (a full disk) surfaces as `Io(StorageFull)` and MUST NOT trigger deletion. So
/// we can't treat all `Io` the same: we inspect the `ErrorKind`.
fn is_unrecoverable_corruption(e: &redb::DatabaseError) -> bool {
    use redb::{DatabaseError as D, StorageError as S};
    match e {
        // Explicit corruption, or a repair redb attempted and gave up on.
        D::Storage(S::Corrupted(_)) | D::RepairAborted => true,
        // An `Io` error whose kind means "the bytes are wrong", not "the device
        // failed". redb reports a garbage/truncated file as Io(InvalidData).
        D::Storage(S::Io(io)) => matches!(
            io.kind(),
            std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof
        ),
        // UpgradeRequired (needs migration), a transient Io kind (StorageFull,
        // PermissionDenied, …), LockPoisoned, etc. — never delete over these.
        _ => false,
    }
}

pub fn open_database(path: &Path) -> Result<Arc<Database>, LychiError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        restrict(parent, OWNER_ONLY_DIR);
    }
    remove_stale_sqlite_artifact(path);

    let db = match Database::create(path) {
        Ok(db) => db,
        // A lock conflict is NOT corruption — it means another Lychi already
        // owns this database. Recreating here renames the live file to `.bak`
        // and hands the second instance an EMPTY database, which reads to the
        // user as "all my history and clipboard are gone". Measured: launching
        // a second instance while the first ran did exactly that.
        //
        // Refuse instead. The caller decides what to do with a second instance;
        // destroying the first one's data is never the answer.
        Err(redb::DatabaseError::DatabaseAlreadyOpen) => {
            return Err(LychiError::Database(format!(
                "another Lychi instance already has {} open",
                path.display()
            )));
        }
        // Recover (rename-and-recreate) ONLY on genuine, unrecoverable
        // corruption. The old branch caught EVERY error on an existing file, so
        // a full disk (Io/ENOSPC), a pending format migration (UpgradeRequired),
        // or a poisoned lock would ALSO rename the live DB away and hand the user
        // an empty one — destroying data over a transient or fixable condition.
        // Match narrowly on the two variants that actually mean "this file cannot
        // be opened and won't get better": Corrupted, and RepairAborted (redb
        // tried to repair and gave up).
        Err(e) if path.exists() && is_unrecoverable_corruption(&e) => {
            tracing::warn!("[db] database is corrupt ({e}), backing up and recreating");
            // Unique, timestamped .bak so a SECOND incident can't overwrite the
            // FIRST backup (the fixed `.redb.bak` name did exactly that).
            let backup = path.with_extension(format!("redb.bak.{}", now_millis()));
            let _ = std::fs::rename(path, &backup);
            // The backup is a full copy of the same (corrupt) user content.
            #[cfg(unix)]
            restrict(&backup, OWNER_ONLY_FILE);
            Database::create(path)?
        }
        // Anything else — UpgradeRequired, Io/ENOSPC, LockPoisoned, an unexpected
        // open error — is surfaced, NOT recovered-by-deletion. A transient or
        // fixable problem must never cost the user their data.
        Err(e) => return Err(e.into()),
    };
    #[cfg(unix)]
    restrict(path, OWNER_ONLY_FILE);

    // Ensure all tables exist by opening them in a write transaction.
    let txn = db.begin_write()?;
    txn.open_table(NOTES)?;
    txn.open_table(TODOS)?;
    txn.open_table(SETTINGS)?;
    txn.open_table(ALIASES)?;
    txn.open_table(REMINDERS)?;
    txn.open_table(SNIPPETS)?;
    txn.open_table(AI_PRESETS)?;
    txn.open_table(PINS)?;
    txn.open_table(META)?;
    txn.commit()?;

    let db = Arc::new(db);
    // BEFORE any store reads: bring the rows to the current schema generation.
    migrate(&db)?;
    Ok(db)
}

/// Bring every row to the current schema generation. Runs at open, before any
/// store reads, in ONE write transaction — an interrupted migration re-runs
/// whole next start, never half-applies.
///
/// Today there is exactly one step: a database from before the envelope
/// existed (no `META` version row) has raw postcard values, which become
/// `[1][raw]`. A future shape change bumps [`SCHEMA_VERSION`] and adds its
/// step here (or a per-row arm in [`decode_body`], whichever fits the change).
///
/// A version NEWER than this binary writes is left alone with a warning: the
/// user downgraded. Rows this binary understands keep working; rows with a
/// newer tag are skipped per-row by [`decode_row`] — degradation is per-row,
/// never wholesale, and nothing is destroyed.
fn migrate(db: &Arc<Database>) -> Result<(), LychiError> {
    let stored: Option<u8> = {
        let txn = db.begin_read()?;
        let table = txn.open_table(META)?;
        table
            .get("schema_version")?
            .and_then(|v| v.value().first().copied())
    };

    match stored {
        Some(v) if v >= SCHEMA_VERSION => {
            if v > SCHEMA_VERSION {
                tracing::warn!(
                    "[db] database schema v{v} is newer than this binary's v{SCHEMA_VERSION} \
                     (downgrade?) — rows with newer shapes are skipped, not destroyed"
                );
            }
            return Ok(());
        }
        Some(_v) => {
            // Older tagged generation: future migration steps chain here.
            // Unreachable while SCHEMA_VERSION == 1 (nothing writes tag 0).
        }
        None => {
            // Pre-envelope database (or fresh). Wrap every existing row and
            // stamp the version, atomically.
            let txn = db.begin_write()?;
            let wrapped = envelope_raw_rows(&txn, &ENVELOPED_TABLES)?;
            {
                let mut meta = txn.open_table(META)?;
                meta.insert("schema_version", [SCHEMA_VERSION].as_slice())?;
            }
            txn.commit()?;
            if wrapped > 0 {
                tracing::info!("[db] enveloped {wrapped} pre-v{SCHEMA_VERSION} row(s)");
            }
            return Ok(());
        }
    }

    // Stamp the current version after any chained steps ran.
    let txn = db.begin_write()?;
    txn.open_table(META)?
        .insert("schema_version", [SCHEMA_VERSION].as_slice())?;
    txn.commit()?;
    Ok(())
}

/// Prepend the current envelope tag to every row of the named tables, within
/// the caller's transaction. Used by [`migrate`] for pre-envelope databases,
/// and by backup restore when the ARCHIVE predates the envelope (its raw rows
/// were just written into a stamped database — without this they would all
/// decode as garbage and read as total data loss).
pub(crate) fn envelope_raw_rows(
    txn: &redb::WriteTransaction,
    tables: &[&str],
) -> Result<u64, LychiError> {
    let mut wrapped = 0u64;
    for name in tables {
        let def: TableDefinition<'_, &str, &[u8]> = TableDefinition::new(name);
        let mut table = txn.open_table(def)?;
        let rows: Vec<(String, Vec<u8>)> = table
            .iter()?
            .flatten()
            .map(|(k, v)| (k.value().to_string(), v.value().to_vec()))
            .collect();
        for (k, raw) in rows {
            table.insert(k.as_str(), wrap_body(&raw).as_slice())?;
            wrapped += 1;
        }
    }
    Ok(wrapped)
}

/// Wrap already-encoded body bytes in the current envelope. For the one store
/// whose body is not postcard (ai_history uses JSON); postcard callers use
/// [`encode_row`].
pub fn wrap_body(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 1);
    out.push(SCHEMA_VERSION);
    out.extend_from_slice(body);
    out
}

/// The body of an enveloped value, if this binary understands its generation.
///
/// `Err` for an empty value or an unknown (newer) tag — the caller decides
/// whether that skips a list row or fails a single-row lookup.
pub fn body_of(bytes: &[u8]) -> Result<&[u8], LychiError> {
    match bytes.split_first() {
        Some((&tag, body)) if tag == SCHEMA_VERSION => Ok(body),
        Some((&tag, _)) => Err(LychiError::Database(format!(
            "row written by schema v{tag}; this binary reads v{SCHEMA_VERSION}"
        ))),
        None => Err(LychiError::Database("empty row value".into())),
    }
}

/// [`body_of`] for JSON-bodied tables (ai_history): same envelope, plus the
/// stranded-legacy fallback — a pre-envelope row IS the JSON document, whose
/// first byte (`{`/`[`) reads as a bogus tag. Unlike postcard, JSON shape is
/// checkable without knowing the value's type, so the fallback lives here.
pub fn json_body_of(bytes: &[u8]) -> Result<&[u8], LychiError> {
    match body_of(bytes) {
        Ok(b) => Ok(b),
        Err(e) => match bytes.first() {
            Some(b'{') | Some(b'[') => Ok(bytes),
            _ => Err(e),
        },
    }
}

/// Encode a row value: `[SCHEMA_VERSION][postcard body]`. The ONE writer-side
/// codec — a source-scan test bans raw `postcard::to_allocvec` against table
/// values elsewhere, so no code path can produce an untagged row.
pub fn encode_row<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, LychiError> {
    let body = postcard::to_allocvec(value)
        .map_err(|e| LychiError::Database(format!("row serialize: {e}")))?;
    Ok(wrap_body(&body))
}

/// Strict single-row decode: envelope + postcard, as a `Result`.
///
/// For `update_note(id)`-style lookups where the user named one row and "that
/// row is corrupt" is the truthful answer. Lists use [`decode_row`].
///
/// An unknown envelope tag gets ONE fallback: try the whole value as a
/// legacy (pre-envelope) raw row. The one-shot migration wraps raw rows only
/// when the META stamp is absent — so a pre-envelope binary running against
/// an already-migrated DB (a downgrade, or the exact mixed-install incident
/// of 2026-08-11: dev build migrated, old AppImage kept writing) strands raw
/// rows the migration never revisits. Their first byte is arbitrary and reads
/// as a bogus "schema v9"-style tag. Only THIS typed layer can tell a
/// stranded legacy row from a genuinely newer generation: a legacy row
/// decodes as `T` from byte zero AND consumes every byte; a newer row's
/// tagged bytes will not. Exact consumption is load-bearing, not pedantry:
/// `postcard::from_bytes` tolerates trailing bytes, and a small all-integer
/// struct really did parse out of a tagged row in testing — the
/// `take_from_bytes` empty-remainder check closes that. On fallback failure
/// the ORIGINAL tag error is returned, so a real downgrade still says
/// "written by schema vN".
pub fn decode_value<'a, T: serde::Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, LychiError> {
    match body_of(bytes) {
        Ok(body) => {
            postcard::from_bytes(body).map_err(|e| LychiError::Database(format!("row decode: {e}")))
        }
        Err(tag_err) if !bytes.is_empty() => match postcard::take_from_bytes::<T>(bytes) {
            Ok((v, [])) => Ok(v),
            _ => Err(tag_err),
        },
        Err(e) => Err(e),
    }
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
    pub notes: u64,
    pub todos: u64,
    pub settings: u64,
    pub aliases: u64,
    pub reminders: u64,
    pub snippets: u64,
}

/// Get row counts for all tables.
pub fn table_stats(db: &Arc<Database>) -> Result<TableStats, LychiError> {
    let txn = db.begin_read()?;
    Ok(TableStats {
        notes: txn.open_table(NOTES)?.len()?,
        todos: txn.open_table(TODOS)?.len()?,
        settings: txn.open_table(SETTINGS)?.len()?,
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
    // One decode path with decode_value: envelope, then the stranded-legacy-
    // row fallback (see its doc). The error text preserves the distinction —
    // a real downgrade says "written by schema vN", corruption says "decode".
    match decode_value(bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            // The key, not the value: the value may be user content, and this
            // goes to a log file. The key is enough to find or delete the row.
            tracing::warn!(
                "[db] skipping row in `{table}` (key {key}): {e} — \
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

#[cfg(test)]
mod envelope_tests {
    use super::*;

    fn temp_db_path(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("lychi-envelope-{tag}-{}", new_id()));
        let path = dir.join("lychi.redb");
        (dir, path)
    }

    /// The v0.1.0 upgrade path, end to end: a database whose rows are raw
    /// postcard (what every pre-envelope install holds) must come out of
    /// `open_database` fully readable — and stay byte-stable across reopens
    /// (a re-run migration that re-wrapped would double-tag every row into
    /// garbage, which is why the version stamp and the wrap share one txn).
    #[test]
    fn a_pre_envelope_database_migrates_once_and_reads_back() {
        let (dir, path) = temp_db_path("migrate");
        std::fs::create_dir_all(&dir).unwrap();

        // What v0.1.0 wrote: raw postcard, no tag, no META table. Uses NOTES (a
        // table that stays in lychi.redb) as the migration sample — the envelope
        // behaviour is table-agnostic.
        let raw = postcard::to_allocvec(&schema::NoteEntry {
            text: "hello".into(),
            created_at: now_millis(),
            updated_at: now_millis(),
            deleted_at: None,
            sync_status: schema::SYNC_LOCAL,
        })
        .unwrap();
        {
            let db = Database::create(&path).unwrap();
            let txn = db.begin_write().unwrap();
            {
                let mut t = txn.open_table(NOTES).unwrap();
                t.insert("note-1", raw.as_slice()).unwrap();
            }
            txn.commit().unwrap();
        }

        let read_len = |db: &Database| -> usize {
            let txn = db.begin_read().unwrap();
            let t = txn.open_table(NOTES).unwrap();
            t.get("note-1").unwrap().unwrap().value().len()
        };
        let decodes = |db: &Database| -> bool {
            let txn = db.begin_read().unwrap();
            let t = txn.open_table(NOTES).unwrap();
            let v = t.get("note-1").unwrap().unwrap();
            decode_row::<schema::NoteEntry>("notes", "note-1", v.value()).is_some()
        };

        let db = open_database(&path).unwrap();
        assert!(
            decodes(&db),
            "a migrated row must decode through the envelope"
        );
        let stored_len = read_len(&db);
        assert_eq!(stored_len, raw.len() + 1, "exactly one tag byte prepended");
        drop(db);

        // Reopen: the stamp must prevent a second wrap.
        let db = open_database(&path).unwrap();
        assert_eq!(read_len(&db), stored_len, "migration must be idempotent");
        assert!(decodes(&db));
        drop(db);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Downgrade shape: a row tagged by a NEWER schema is skipped per-row —
    /// the rows this binary understands keep working beside it. This is the
    /// property the envelope buys over bare postcard, where a foreign-shape
    /// row is indistinguishable from garbage and, worse, the first shape
    /// change made 100% of OLD rows unreadable at once.
    #[test]
    fn a_newer_generation_row_is_skipped_beside_readable_ones() {
        let db = open_test_database();
        // A current-generation row and a future-tagged one, side by side in a
        // table that stays in lychi.redb (NOTES). The list decode must keep the
        // one it understands and skip the newer-tagged one.
        let current = encode_row(&schema::NoteEntry {
            text: "current".into(),
            created_at: now_millis(),
            updated_at: now_millis(),
            deleted_at: None,
            sync_status: schema::SYNC_LOCAL,
        })
        .unwrap();
        {
            let txn = db.begin_write().unwrap();
            {
                let mut t = txn.open_table(NOTES).unwrap();
                t.insert("current-row", current.as_slice()).unwrap();
                t.insert("future-row", [SCHEMA_VERSION + 1, 0xDE, 0xAD].as_slice())
                    .unwrap();
            }
            txn.commit().unwrap();
        }
        let txn = db.begin_read().unwrap();
        let t = txn.open_table(NOTES).unwrap();
        let current_ok = decode_row::<schema::NoteEntry>(
            "notes",
            "current-row",
            t.get("current-row").unwrap().unwrap().value(),
        );
        let future_ok = decode_row::<schema::NoteEntry>(
            "notes",
            "future-row",
            t.get("future-row").unwrap().unwrap().value(),
        );
        assert!(current_ok.is_some(), "known rows keep working");
        assert!(
            future_ok.is_none(),
            "a newer-tagged row must be skipped, never garbage-decoded"
        );
    }

    /// A fresh database is stamped immediately: rows are versioned from the
    /// first write, so the pre-envelope migration can never run on it again.
    #[test]
    fn a_fresh_database_is_stamped_with_the_current_version() {
        let db = open_test_database();
        let txn = db.begin_read().unwrap();
        let meta = txn.open_table(META).unwrap();
        let v = meta.get("schema_version").unwrap().expect("stamp missing");
        assert_eq!(v.value(), [SCHEMA_VERSION]);
    }

    /// THE STRANDED-ROW INCIDENT (2026-08-11): the dev build migrated the DB
    /// (META stamped v1), then the old pre-envelope AppImage kept running and
    /// wrote RAW rows the one-shot migration never revisits — 3 AI presets,
    /// ~50 clipboard entries and 2 history rows invisible, their first bytes
    /// read as bogus "schema v9"-style tags. The typed fallback recovers any
    /// value that parses as `T` from byte zero and consumes every byte.
    #[test]
    fn a_stranded_pre_envelope_row_is_recovered_not_skipped() {
        let entry = schema::TimerEntry {
            name: "tea".into(),
            duration_secs: 300,
            elapsed_before_secs: 0.0,
            running_since_epoch_ms: Some(123),
        };
        // Raw postcard, no envelope — what a pre-envelope binary wrote.
        // (postcard:: is legal here: db/mod.rs is the codec's whitelisted home.)
        let raw = postcard::to_allocvec(&entry).unwrap();

        let via_value: schema::TimerEntry = decode_value(&raw).expect("legacy row recovers");
        assert_eq!(via_value.name, "tea");
        let via_row: Option<schema::TimerEntry> = decode_row("timers", "k", &raw);
        assert_eq!(via_row.unwrap().duration_secs, 300);

        // Garbage that decodes as nothing still errors/skips.
        assert!(decode_value::<schema::TimerEntry>(&[9, 200, 200, 200]).is_err());
    }

    /// JSON tables get the same recovery via shape: a legacy raw JSON document
    /// starts with `{`/`[`, which reads as a bogus tag.
    #[test]
    fn a_stranded_legacy_json_row_is_recovered() {
        let legacy = br#"{"id":"c1"}"#;
        assert_eq!(json_body_of(legacy).unwrap(), legacy.as_slice());
        // Properly enveloped JSON still unwraps to its body.
        let wrapped = wrap_body(legacy);
        assert_eq!(json_body_of(&wrapped).unwrap(), legacy.as_slice());
        // Non-JSON unknown tags still error.
        assert!(json_body_of(&[9, 1, 2, 3]).is_err());
    }

    /// The strict/single-row contract: current-tag decodes, wrong-tag and
    /// empty are errors that SAY what happened.
    #[test]
    fn decode_value_reports_generation_mismatches() {
        let entry = frecency::FrecencyEntry {
            count: 1,
            recent_timestamps: vec![1],
        };
        let good = encode_row(&entry).unwrap();
        assert!(decode_value::<frecency::FrecencyEntry>(&good).is_ok());

        let mut future = good.clone();
        future[0] = SCHEMA_VERSION + 1;
        let err = decode_value::<frecency::FrecencyEntry>(&future).unwrap_err();
        assert!(format!("{err:?}").contains("schema v"), "{err:?}");

        assert!(decode_value::<frecency::FrecencyEntry>(&[]).is_err());
    }
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

    /// The data-loss bug this guards: a SECOND instance used to treat the
    /// first's lock as corruption, rename the live database to `.bak` and hand
    /// itself an empty one. The user saw every note, clip and history row gone.
    #[test]
    fn a_locked_database_is_refused_not_recreated() {
        let d = tmpdir("locked");
        let path = d.join("lychi.redb");

        // First instance owns the lock and holds real content.
        let first = open_database(&path).expect("first open should succeed");
        {
            let txn = first.begin_write().unwrap();
            {
                let mut t = txn.open_table(NOTES).unwrap();
                t.insert("row-1", b"user data".as_slice()).unwrap();
            }
            txn.commit().unwrap();
        }

        // Second instance must REFUSE, not recreate.
        let second = open_database(&path);
        assert!(second.is_err(), "a locked database must not open");

        // The live file is untouched and no backup was minted from it.
        assert!(path.exists(), "the live database must still be there");
        assert!(
            !path.with_extension("redb.bak").exists(),
            "a lock conflict must NOT produce a .bak — that is the data loss"
        );

        // And the first instance still sees its row.
        let txn = first.begin_read().unwrap();
        let t = txn.open_table(NOTES).unwrap();
        assert!(t.get("row-1").unwrap().is_some(), "user data survived");

        drop(t);
        drop(txn);
        drop(first);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_corruption_variants_trigger_recovery() {
        use redb::{DatabaseError, StorageError};
        // Recover: genuine, unrecoverable corruption.
        assert!(is_unrecoverable_corruption(&DatabaseError::Storage(
            StorageError::Corrupted("bad magic".into())
        )));
        assert!(is_unrecoverable_corruption(&DatabaseError::RepairAborted));
        // A structurally-invalid file surfaces as Io(InvalidData/UnexpectedEof)
        // — that IS corruption and SHOULD recover.
        assert!(
            is_unrecoverable_corruption(&DatabaseError::Storage(StorageError::Io(
                std::io::Error::from(std::io::ErrorKind::InvalidData)
            ))),
            "a garbage/truncated file (Io InvalidData) is corruption"
        );
        // NEVER recover (would destroy data over a transient/fixable condition):
        assert!(
            !is_unrecoverable_corruption(&DatabaseError::UpgradeRequired(3)),
            "a pending format upgrade must NOT nuke the DB"
        );
        assert!(
            !is_unrecoverable_corruption(&DatabaseError::Storage(StorageError::Io(
                std::io::Error::from(std::io::ErrorKind::StorageFull)
            ))),
            "a full disk (Io StorageFull) must NOT nuke the DB"
        );
        assert!(!is_unrecoverable_corruption(&DatabaseError::Storage(
            StorageError::PreviousIo
        )));
    }

    #[test]
    fn a_corrupt_database_is_recovered_with_a_timestamped_bak() {
        let d = tmpdir("corrupt");
        let path = d.join("lychi.redb");
        // Garbage that isn't a valid redb file → redb reports Corrupted on open.
        std::fs::write(&path, b"this is not a redb database, just garbage bytes").unwrap();

        let db = open_database(&path);
        assert!(db.is_ok(), "a corrupt file must recover to a fresh DB");

        // The corrupt original was moved aside under a UNIQUE, timestamped name
        // (not the old fixed `redb.bak`, which a second incident would clobber).
        let baks: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("lychi.redb.bak."))
            .collect();
        assert_eq!(baks.len(), 1, "exactly one timestamped .bak, got: {baks:?}");
        assert!(
            !path.with_extension("redb.bak").exists(),
            "the fixed-name .bak must no longer be used"
        );

        drop(db);
        let _ = std::fs::remove_dir_all(&d);
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
