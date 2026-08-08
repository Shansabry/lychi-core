//! Backup and restore of everything a user would hate to lose.
//!
//! # Design, and why each choice was made
//!
//! **A `.tar.gz` of real files, not a bespoke dump format.** The archive is
//! openable with `tar -tzf` and repairable by hand. A format only Lychi can
//! read is a second thing that can fail when the first thing already has.
//!
//! **The database is copied through a read transaction, never as bytes off
//! disk.** redb is copy-on-write with MVCC, so a read transaction pins a
//! consistent snapshot while writes continue; `cp`-ing a live file can capture
//! a torn state mid-commit. Tables are enumerated with `list_tables()` rather
//! than from a hardcoded list, so a table added later is backed up without
//! anyone remembering to update this file.
//!
//! **Restore is atomic and reversible.** Everything is staged, verified, and
//! only then swapped in — and the pre-restore state is itself backed up first,
//! so "restore" is never a one-way door. Partial restores are the failure mode
//! that turns a recoverable problem into an unrecoverable one.
//!
//! **A manifest records what was captured and by which version.** Restoring a
//! backup from a future version into an older Lychi is refused rather than
//! attempted, because a schema it does not understand is exactly how a restore
//! turns into data loss.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition, TableHandle};
use serde::{Deserialize, Serialize};

use crate::error::LychiError;

/// Archive layout version. Bumped only when the *shape* of the archive changes
/// (new top-level entries, different manifest fields) — not on every release.
pub const ARCHIVE_VERSION: u32 = 1;

/// Name of the manifest inside the archive.
const MANIFEST_NAME: &str = "manifest.json";
/// Directory inside the archive holding the exported database tables.
const DB_DIR: &str = "db";

/// How many automatic backups to keep. Manual ones are never auto-pruned —
/// a user who clicked "Back up now" before doing something risky should not
/// find that backup evicted by a routine one.
const AUTO_RETAIN: usize = 10;

/// What a backup was taken for. Recorded in the manifest and the filename so
/// the reason survives into the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum BackupKind {
    /// The user asked for it.
    Manual,
    /// Taken automatically before something that could lose data (an upgrade,
    /// a restore, a migration).
    Automatic,
}

impl BackupKind {
    fn slug(self) -> &'static str {
        match self {
            BackupKind::Manual => "manual",
            BackupKind::Automatic => "auto",
        }
    }
}

/// What is inside an archive, written as `manifest.json` at its root.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct Manifest {
    /// Archive layout version — see [`ARCHIVE_VERSION`].
    pub archive_version: u32,
    /// The Lychi version that produced it.
    pub app_version: String,
    /// Unix millis when it was taken.
    pub created_at: u64,
    pub kind: BackupKind,
    /// Free-text note ("before restore", "before upgrade to 0.2.0").
    #[serde(default)]
    pub reason: String,
    /// Table name → row count, so the UI can say what is in a backup without
    /// unpacking it, and restore can verify it got everything.
    pub tables: Vec<(String, u64)>,
    /// Schema generation of the archived row VALUES. `0` (the serde
    /// default) marks archives from before the row envelope existed — their
    /// values are raw postcard, and restore must wrap them after applying, or
    /// every restored row decodes as garbage. Current archives stamp
    /// [`crate::db::SCHEMA_VERSION`].
    #[serde(default)]
    pub schema_version: u8,
    /// Which optional extras are present.
    #[serde(default)]
    pub has_config: bool,
    #[serde(default)]
    pub has_scripts: bool,
}

impl Manifest {
    /// Total rows across every table — the headline number for the UI.
    pub fn total_rows(&self) -> u64 {
        self.tables.iter().map(|(_, n)| n).sum()
    }
}

/// A backup on disk, as listed for the UI.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct BackupInfo {
    /// Absolute path to the `.tar.gz`.
    pub path: String,
    /// Bare filename, the stable id the UI passes back to restore/delete.
    pub name: String,
    /// Archive size in bytes.
    pub size_bytes: u64,
    /// `None` when the manifest could not be read — such an archive is listed
    /// (so the user can see and delete it) but must never be restored.
    pub manifest: Option<Manifest>,
}

impl BackupInfo {
    /// Whether this archive is safe to restore into the running version.
    pub fn is_restorable(&self, app_version: &str) -> bool {
        match &self.manifest {
            None => false,
            Some(m) => {
                m.archive_version <= ARCHIVE_VERSION && !is_newer(&m.app_version, app_version)
            }
        }
    }
}

/// Is `a` a strictly newer semver than `b`? Unparseable versions compare as
/// not-newer: refusing a restore because a version string looked odd would be
/// worse than allowing it.
fn is_newer(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> Option<(u32, u32, u32)> {
        let core = v.trim().trim_start_matches('v');
        let core = core.split(['-', '+']).next()?;
        let mut it = core.split('.');
        Some((
            it.next()?.parse().ok()?,
            it.next().unwrap_or("0").parse().ok()?,
            it.next().unwrap_or("0").parse().ok()?,
        ))
    }
    match (parts(a), parts(b)) {
        (Some(x), Some(y)) => x > y,
        _ => false,
    }
}

/// Every row of one table, as raw key/value bytes.
///
/// Values are stored exactly as redb holds them (postcard-encoded structs), so
/// a backup neither re-encodes nor interprets user data — it cannot corrupt
/// what it does not parse.
#[derive(Serialize, Deserialize)]
struct TableDump {
    name: String,
    rows: Vec<(String, Vec<u8>)>,
}

/// Create a backup archive and return its info.
///
/// The database is read through one transaction, so the snapshot is internally
/// consistent even if the user is actively typing.
pub fn create(
    db: &Arc<Database>,
    kind: BackupKind,
    reason: &str,
    app_version: &str,
) -> Result<BackupInfo, LychiError> {
    let dir = crate::paths::backups_dir();
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    restrict_dir(&dir);

    let created_at = crate::db::now_millis();
    // Second-resolution stamps collide when two backups land in the same
    // second (a manual click right after the hourly refresh, or the
    // pre-upgrade and hourly snapshots on the same startup) — and the second
    // would silently overwrite the first. Suffix on collision rather than
    // widening the stamp, which would make filenames harder to read for the
    // case that never collides.
    let base = format!("lychi-{}-{}", stamp(created_at), kind.slug());
    let mut name = format!("{base}.tar.gz");
    for n in 2..100 {
        if !dir.join(&name).exists() {
            break;
        }
        name = format!("{base}-{n}.tar.gz");
    }
    let path = dir.join(&name);

    // Build into a temp file, then rename. A crash mid-write must not leave a
    // truncated archive that looks like a real backup.
    let tmp = dir.join(format!(".{name}.part"));
    let dumps = dump_tables(db)?;
    let tables: Vec<(String, u64)> = dumps
        .iter()
        .map(|d| (d.name.clone(), d.rows.len() as u64))
        .collect();

    let config_path = crate::paths::config_file();
    let scripts_path = crate::paths::scripts_dir();
    let manifest = Manifest {
        archive_version: ARCHIVE_VERSION,
        app_version: app_version.to_string(),
        created_at,
        kind,
        reason: reason.to_string(),
        tables,
        schema_version: crate::db::SCHEMA_VERSION,
        has_config: config_path.is_file(),
        has_scripts: scripts_path.is_dir(),
    };

    {
        let file = fs::File::create(&tmp)?;
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| LychiError::ExecutionFailed(format!("manifest encode failed: {e}")))?;
        append_bytes(&mut tar, MANIFEST_NAME, &manifest_bytes)?;

        for dump in &dumps {
            let bytes = postcard::to_allocvec(dump)
                .map_err(|e| LychiError::ExecutionFailed(format!("table encode failed: {e}")))?;
            append_bytes(&mut tar, &format!("{DB_DIR}/{}.bin", dump.name), &bytes)?;
        }

        if manifest.has_config
            && let Ok(bytes) = fs::read(&config_path)
        {
            append_bytes(&mut tar, "config/config.toml", &bytes)?;
        }
        if manifest.has_scripts {
            append_dir(&mut tar, &scripts_path, "config/scripts")?;
        }

        // Finish the gzip stream and fsync before the rename, so the rename
        // cannot publish a file whose bytes are still in the page cache.
        let enc = tar
            .into_inner()
            .map_err(|e| LychiError::ExecutionFailed(format!("archive finalise failed: {e}")))?;
        let file = enc
            .finish()
            .map_err(|e| LychiError::ExecutionFailed(format!("compression failed: {e}")))?;
        file.sync_all()?;
    }

    fs::rename(&tmp, &path)?;
    #[cfg(unix)]
    restrict_file(&path);

    if kind == BackupKind::Automatic {
        prune_automatic(&dir);
    }

    Ok(BackupInfo {
        name,
        size_bytes: fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        path: path.to_string_lossy().into_owned(),
        manifest: Some(manifest),
    })
}

/// Read every table through ONE read transaction — a consistent MVCC snapshot.
fn dump_tables(db: &Arc<Database>) -> Result<Vec<TableDump>, LychiError> {
    let txn = db.begin_read()?;

    // Enumerated, not hardcoded: a table added later is captured automatically.
    let names: Vec<String> = txn
        .list_tables()?
        .map(|h| h.name().to_string())
        .collect::<Vec<_>>();

    let mut dumps = Vec::with_capacity(names.len());
    for name in names {
        let def: TableDefinition<'_, &str, &[u8]> = TableDefinition::new(name.as_str());
        let Ok(table) = txn.open_table(def) else {
            tracing::warn!("[backup] skipping unreadable table {name}");
            continue;
        };
        let mut rows = Vec::new();
        for entry in table.iter()? {
            let (k, v) = entry?;
            rows.push((k.value().to_string(), v.value().to_vec()));
        }
        dumps.push(TableDump { name, rows });
    }
    Ok(dumps)
}

/// List backups, newest first. Unreadable archives are listed with
/// `manifest: None` so the user can see and delete them.
pub fn list() -> Vec<BackupInfo> {
    let dir = crate::paths::backups_dir();
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut out: Vec<BackupInfo> = rd
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.ends_with(".tar.gz") && !n.starts_with('.')
        })
        .map(|e| {
            let path = e.path();
            BackupInfo {
                name: e.file_name().to_string_lossy().into_owned(),
                size_bytes: e.metadata().map(|m| m.len()).unwrap_or(0),
                manifest: read_manifest(&path).ok(),
                path: path.to_string_lossy().into_owned(),
            }
        })
        .collect();

    out.sort_by(|a, b| {
        let ta = a.manifest.as_ref().map(|m| m.created_at).unwrap_or(0);
        let tb = b.manifest.as_ref().map(|m| m.created_at).unwrap_or(0);
        tb.cmp(&ta).then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// Read just the manifest out of an archive, without unpacking the rest.
pub fn read_manifest(archive: &Path) -> Result<Manifest, LychiError> {
    for entry in open_archive(archive)?.entries()? {
        let mut entry = entry?;
        if entry.path()?.to_string_lossy() == MANIFEST_NAME {
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut entry, &mut buf)?;
            return serde_json::from_str(&buf).map_err(|e| {
                LychiError::ExecutionFailed(format!("backup manifest is not readable: {e}"))
            });
        }
    }
    Err(LychiError::ExecutionFailed(
        "archive has no manifest — not a Lychi backup".into(),
    ))
}

fn open_archive(
    path: &Path,
) -> Result<tar::Archive<flate2::read::GzDecoder<fs::File>>, LychiError> {
    let file = fs::File::open(path)?;
    Ok(tar::Archive::new(flate2::read::GzDecoder::new(file)))
}

/// What a restore did, for reporting back to the user.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct RestoreReport {
    pub tables_restored: u64,
    pub rows_restored: u64,
    pub config_restored: bool,
    pub scripts_restored: bool,
    /// The safety backup taken of the pre-restore state.
    pub safety_backup: String,
}

/// Restore an archive over the live database.
///
/// The order is deliberate: **verify, then snapshot the current state, then
/// write.** Every failure before the write leaves the user exactly where they
/// were, and the safety backup means even a successful restore is undoable.
pub fn restore(
    db: &Arc<Database>,
    archive: &Path,
    app_version: &str,
) -> Result<RestoreReport, LychiError> {
    // 1. Verify the archive BEFORE touching anything.
    let manifest = read_manifest(archive)?;
    if manifest.archive_version > ARCHIVE_VERSION {
        return Err(LychiError::ExecutionFailed(format!(
            "backup uses archive format v{} but this Lychi understands v{ARCHIVE_VERSION} — \
             update Lychi to restore it",
            manifest.archive_version
        )));
    }
    if is_newer(&manifest.app_version, app_version) {
        return Err(LychiError::ExecutionFailed(format!(
            "backup was made by Lychi {} and this is {app_version} — update Lychi to restore it",
            manifest.app_version
        )));
    }

    // 2. Read the whole archive into memory and check it against its own
    //    manifest. A truncated or tampered archive must fail here, before any
    //    live data has been touched.
    let mut dumps: Vec<TableDump> = Vec::new();
    let mut config: Option<Vec<u8>> = None;
    let mut scripts: Vec<(PathBuf, Vec<u8>)> = Vec::new();

    for entry in open_archive(archive)?.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let name = path.to_string_lossy().to_string();

        let mut buf = Vec::new();
        if entry.header().entry_type().is_file() {
            std::io::Read::read_to_end(&mut entry, &mut buf)?;
        } else {
            continue;
        }

        if name.starts_with(&format!("{DB_DIR}/")) && name.ends_with(".bin") {
            let dump: TableDump = postcard::from_bytes(&buf).map_err(|e| {
                LychiError::ExecutionFailed(format!("backup table {name} is corrupt: {e}"))
            })?;
            dumps.push(dump);
        } else if name == "config/config.toml" {
            config = Some(buf);
        } else if let Some(rest) = name.strip_prefix("config/scripts/") {
            // Reject anything that would escape the scripts directory. An
            // archive is untrusted input; `../` in a tar entry is the classic
            // path-traversal write-anywhere bug.
            let rel = Path::new(rest);
            if rel.is_absolute()
                || rel
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                tracing::warn!("[restore] refusing suspicious archive path: {name}");
                continue;
            }
            scripts.push((rel.to_path_buf(), buf));
        }
    }

    let expected: u64 = manifest.total_rows();
    let found: u64 = dumps.iter().map(|d| d.rows.len() as u64).sum();
    if found != expected {
        return Err(LychiError::ExecutionFailed(format!(
            "backup is incomplete: manifest says {expected} rows, archive holds {found}"
        )));
    }

    // 3. Snapshot the CURRENT state before overwriting it. Restore must not be
    //    a one-way door — this is what makes "restored the wrong backup"
    //    recoverable.
    let safety = create(db, BackupKind::Automatic, "before restore", app_version)?;

    // 4. Apply. One write transaction: it either all lands or none of it does,
    //    so an interrupted restore can never leave a half-replaced database.
    let mut rows_restored = 0u64;
    {
        let txn = db.begin_write()?;
        for dump in &dumps {
            let def: TableDefinition<'_, &str, &[u8]> = TableDefinition::new(dump.name.as_str());
            let mut table = txn.open_table(def)?;
            // Replace, not merge: the user asked for the state in the backup.
            table.retain(|_, _| false)?;
            for (k, v) in &dump.rows {
                table.insert(k.as_str(), v.as_slice())?;
                rows_restored += 1;
            }
        }
        // A pre-envelope archive holds raw postcard values; the live database
        // is stamped and its readers strip a version tag. Wrap the restored
        // rows in the same transaction — either the tagged rows land with the
        // data, or neither does.
        if manifest.schema_version < crate::db::SCHEMA_VERSION {
            let names: Vec<&str> = dumps.iter().map(|d| d.name.as_str()).collect();
            let wrapped = crate::db::envelope_raw_rows(&txn, &names)?;
            tracing::info!("[backup] enveloped {wrapped} row(s) from a pre-envelope archive");
        }
        txn.commit()?;
    }

    // 5. Files last — they are the least dangerous to get wrong, and doing
    //    them after the DB commit keeps the risky step first while the safety
    //    backup is freshest.
    let mut config_restored = false;
    if let Some(bytes) = config {
        let dest = crate::paths::config_file();
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        if write_atomic(&dest, &bytes).is_ok() {
            config_restored = true;
        }
    }

    let mut scripts_restored = false;
    if !scripts.is_empty() {
        let base = crate::paths::scripts_dir();
        fs::create_dir_all(&base)?;
        for (rel, bytes) in &scripts {
            let dest = base.join(rel);
            if let Some(parent) = dest.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if write_atomic(&dest, bytes).is_ok() {
                scripts_restored = true;
                #[cfg(unix)]
                make_executable(&dest);
            }
        }
    }

    Ok(RestoreReport {
        tables_restored: dumps.len() as u64,
        rows_restored,
        config_restored,
        scripts_restored,
        safety_backup: safety.name,
    })
}

/// Settings key holding the version that last ran. Absent on a fresh install.
const LAST_VERSION_KEY: &str = "app.last_version";

/// Settings key holding when the rolling hourly backup was last taken.
const LAST_AUTO_BACKUP_KEY: &str = "app.last_auto_backup_ms";

/// How often the rolling automatic backup refreshes.
const AUTO_BACKUP_INTERVAL_MS: u64 = 60 * 60 * 1000;

/// Take the rolling hourly backup if an hour has passed, replacing the previous
/// one.
///
/// **Exactly one** automatic archive exists at a time by design: the Data tab
/// stays legible (one "Automatic" row, plus whatever the user took manually),
/// and the common loss — noticing within the hour that something is gone — is
/// covered without accumulating a wall of near-identical archives.
///
/// Pre-upgrade backups are separate and NOT pruned by this: an upgrade is the
/// riskiest moment for data, and its snapshot must outlive the next hourly
/// refresh. Manual backups are never touched.
///
/// Never fails startup; a backup that cannot be written is logged, because
/// refusing to launch over it would be the worse outcome.
pub fn hourly_backup(db: &Arc<Database>, app_version: &str) -> Option<BackupInfo> {
    let now = crate::db::now_millis();
    let last = crate::config::db::load_syncable(db)
        .ok()
        .and_then(|s| s.get(LAST_AUTO_BACKUP_KEY).cloned())
        .and_then(|v| v.parse::<u64>().ok());

    // `saturating_sub` so a clock that moved backwards defers rather than
    // firing on every summon.
    if let Some(last) = last
        && now.saturating_sub(last) < AUTO_BACKUP_INTERVAL_MS
    {
        return None;
    }

    let taken = match create(db, BackupKind::Automatic, "hourly", app_version) {
        Ok(info) => {
            // Replace, don't accumulate: drop every OTHER automatic archive
            // that is not a pre-upgrade one.
            prune_rolling(&info.name);
            Some(info)
        }
        Err(e) => {
            tracing::error!("[backup] hourly backup failed: {e}");
            None
        }
    };

    if let Err(e) = crate::config::db::save_setting(db, LAST_AUTO_BACKUP_KEY, &now.to_string()) {
        tracing::warn!("[backup] could not record backup time: {e}");
    }
    taken
}

/// Keep only `keep` among the rolling automatic backups.
///
/// Deliberately narrow: a backup whose reason is not exactly `"hourly"` — a
/// pre-upgrade or pre-restore snapshot — is left alone, because those mark
/// moments the user would want to return to and an hourly refresh must not
/// evict them.
fn prune_rolling(keep: &str) {
    for b in list() {
        let is_rolling = b
            .manifest
            .as_ref()
            .is_some_and(|m| m.kind == BackupKind::Automatic && m.reason == "hourly");
        if is_rolling && b.name != keep {
            let _ = delete(&b.name);
        }
    }
}

/// Take an automatic backup if this is the first run of a new version, then
/// record the current one.
///
/// Called once at startup. An upgrade is the moment a schema migration or a
/// new bug can eat data, and it is the moment the user is least expecting to
/// need a backup — so the snapshot is taken before any of the new code has
/// written anything.
///
/// Returns the backup taken, or `None` if this version has already run.
/// Never fails startup: a backup that cannot be written is logged, because
/// refusing to launch over it would be a worse outcome than running without.
pub fn backup_if_upgraded(db: &Arc<Database>, app_version: &str) -> Option<BackupInfo> {
    let previous = crate::config::db::load_syncable(db)
        .ok()
        .and_then(|s| s.get(LAST_VERSION_KEY).cloned());

    let taken = match previous.as_deref() {
        // Same version — nothing to do. The common path, and it costs one read.
        Some(v) if v == app_version => return None,
        // A fresh install has nothing worth preserving yet.
        None => None,
        Some(old) => {
            tracing::info!("[backup] upgrade detected: {old} → {app_version}");
            match create(
                db,
                BackupKind::Automatic,
                &format!("before upgrade to {app_version}"),
                // Stamped with the version that MADE it, which is the old one:
                // the archive holds that version's data.
                old,
            ) {
                Ok(info) => Some(info),
                Err(e) => {
                    tracing::error!("[backup] pre-upgrade backup failed: {e}");
                    None
                }
            }
        }
    };

    if let Err(e) = crate::config::db::save_setting(db, LAST_VERSION_KEY, app_version) {
        tracing::warn!("[backup] could not record version: {e}");
    }
    taken
}

/// Delete a backup by filename. Refuses anything that is not a plain name
/// inside the backups directory, so a crafted id cannot delete arbitrary files.
pub fn delete(name: &str) -> Result<(), LychiError> {
    let dir = crate::paths::backups_dir();
    let path = dir.join(name);
    if Path::new(name).components().count() != 1 || !path.starts_with(&dir) {
        return Err(LychiError::ExecutionFailed(format!(
            "refusing to delete outside the backups directory: {name}"
        )));
    }
    fs::remove_file(path)?;
    Ok(())
}

/// Keep the newest [`AUTO_RETAIN`] automatic backups; manual ones are never
/// pruned. A user who deliberately took a backup should not lose it to routine
/// housekeeping.
fn prune_automatic(dir: &Path) {
    let mut autos: Vec<(u64, PathBuf)> = list()
        .into_iter()
        .filter(|b| {
            b.manifest
                .as_ref()
                .is_some_and(|m| m.kind == BackupKind::Automatic)
        })
        .map(|b| {
            (
                b.manifest.as_ref().map(|m| m.created_at).unwrap_or(0),
                PathBuf::from(b.path),
            )
        })
        .collect();
    if autos.len() <= AUTO_RETAIN {
        return;
    }
    // Newest first, so `skip(AUTO_RETAIN)` drops the oldest.
    autos.sort_by_key(|(created_at, _)| std::cmp::Reverse(*created_at));
    for (_, path) in autos.into_iter().skip(AUTO_RETAIN) {
        if path.starts_with(dir) {
            let _ = fs::remove_file(&path);
        }
    }
}

/// Write via temp file + rename, so an interrupted write cannot truncate the
/// file it was replacing.
fn write_atomic(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = dest.with_extension(format!(
        "{}.part",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dest)
}

fn append_bytes<W: Write>(
    tar: &mut tar::Builder<W>,
    name: &str,
    bytes: &[u8],
) -> Result<(), LychiError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    tar.append_data(&mut header, name, bytes)
        .map_err(|e| LychiError::ExecutionFailed(format!("archive write failed: {e}")))
}

fn append_dir<W: Write>(
    tar: &mut tar::Builder<W>,
    dir: &Path,
    prefix: &str,
) -> Result<(), LychiError> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Ok(());
    };
    for e in rd.flatten() {
        let path = e.path();
        if !path.is_file() {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        let name = format!("{prefix}/{}", e.file_name().to_string_lossy());
        append_bytes(tar, &name, &bytes)?;
    }
    Ok(())
}

/// `YYYYMMDD-HHMMSS` in UTC, so filenames sort chronologically.
fn stamp(millis: u64) -> String {
    let secs = (millis / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Howard Hinnant's days-from-civil, inverted. Avoids pulling in `chrono`
/// for one filename.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(unix)]
fn restrict_dir(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
}

/// Backups hold the same clipboard, notes and history as the database, so they
/// get the same owner-only treatment (see `db::OWNER_ONLY_FILE`).
#[cfg(unix)]
fn restrict_file(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o600));
}

#[cfg(unix)]
fn make_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
}

#[cfg(test)]
mod tests;
