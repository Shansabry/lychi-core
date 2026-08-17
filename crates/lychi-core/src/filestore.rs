//! Crash-recoverable file-backed stores for data that does NOT belong in the
//! redb user-data database.
//!
//! The database is reserved for user-authored content (notes, todos, snippets,
//! aliases, reminders, pins, AI presets). Everything else — derived, device-
//! local, or machine-state (command history, frecency, clipboard, timers, AI
//! chat transcripts, learned model capabilities) — lives in files here, where
//! deleting a record actually reclaims disk immediately (redb reuses freed pages
//! but the file never shrinks) and where retention is a filesystem operation.
//!
//! Two access patterns, both crash-recoverable:
//!
//!   1. [`JsonlLog`] — an append-only JSON Lines log. One record per line. The
//!      right choice for anything you append to and occasionally prune (history,
//!      clipboard, chat transcripts).
//!   2. [`snapshot`] / [`load_snapshot`] — a single value written whole via the
//!      atomic-write triad. The right choice for small state rewritten in full
//!      (timers).
//!
//! # The JSONL contract (follows the JSON Lines / NDJSON standard)
//!
//! - **One compact JSON value per line, UTF-8, `\n`-terminated, no BOM.** serde's
//!   default (compact) output is single-line, and any `\n` inside a string field
//!   is escaped to `\n`, so a record never spans lines.
//! - **The terminating `\n` is the commit marker.** A record counts as written
//!   only once its newline is durably on disk. The JSON and its `\n` are written
//!   in a SINGLE `write_all` with `O_APPEND`, which on a local filesystem is an
//!   atomic append (concurrent appenders never interleave, and a line never
//!   exists without its terminator).
//! - **Torn trailing line → discarded for free.** A crash mid-append leaves a
//!   final line with no `\n`. `BufRead::lines()` never yields an unterminated
//!   final chunk, so the torn tail simply disappears on the next load — the
//!   standard "at most the last record is lost" guarantee.
//! - **Corrupt middle line → skipped and warned**, never fatal. Same policy as
//!   `db::decode_row`: one bad row must not lose the whole file.
//!
//! Durability: `sync_data()` after each append. At desktop volumes (a few lines
//! per user action) per-append fsync is cheap and buys real crash durability —
//! the group-commit batching that high-throughput logs need does not apply here.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::LychiError;

/// Narrow a store file to owner-only (`0600`). These files hold clipboard text,
/// command history and AI transcripts — the same sensitivity as the databases,
/// which are already 0600. Best-effort and non-fatal: a filesystem that can't
/// represent the mode (FAT `$HOME`, some network mounts) is not a reason to fail
/// a write. No-op on non-Unix.
fn restrict_owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(md) = fs::metadata(path)
            && md.permissions().mode() & 0o777 != 0o600
        {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// An append-only JSON Lines log at a fixed path.
///
/// Cheap to construct (holds only the path); each operation opens the file for
/// the duration it needs. That keeps the type `Clone`/`Send` without a shared
/// handle, and matches the low call rate of the stores built on it.
#[derive(Debug, Clone)]
pub struct JsonlLog {
    path: PathBuf,
}

impl JsonlLog {
    /// Bind a log to `path`. Does not touch the filesystem until first use.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The backing file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record durably.
    ///
    /// Serializes to compact JSON, appends a `\n`, and writes both in a single
    /// `O_APPEND` `write_all` so the line is never torn from its terminator, then
    /// `sync_data()`s so a crash immediately after cannot lose it. Creates the
    /// file (and parent dir) on first append and fsyncs the directory once so the
    /// file's existence is durable too.
    pub fn append<T: Serialize>(&self, record: &T) -> Result<(), LychiError> {
        let mut buf = serde_json::to_vec(record)
            .map_err(|e| LychiError::Config(format!("jsonl encode: {e}")))?;
        // Invariant: compact serde output is single-line. Guard against a future
        // custom Serialize sneaking a raw newline in, which would split one
        // record into two on read.
        debug_assert!(
            !buf.contains(&b'\n'),
            "jsonl record must not contain a raw newline"
        );
        buf.push(b'\n');

        let is_new = !self.path.exists();
        if is_new && let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        // Owner-only from birth: these logs hold clipboard text, command history
        // and AI transcripts — the same sensitivity as the databases, which are
        // 0600. Without this the default umask makes them world-readable (0644).
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&self.path)?;
        f.write_all(&buf)?;
        f.sync_data()?;
        // An EXISTING file created before this (or by an older build) keeps its
        // old mode on open, so tighten it explicitly too.
        restrict_owner_only(&self.path);

        // On first creation, fsync the parent dir so the new file's directory
        // entry survives a crash (the append's own bytes are already fsync'd).
        if is_new
            && let Some(parent) = self.path.parent()
            && let Ok(dir) = File::open(parent)
        {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    /// Load every well-formed record, in file order.
    ///
    /// - A torn trailing line (crash mid-append) is partial JSON, so `from_str`
    ///   fails on it and it is skipped — the standard "at most the last record is
    ///   lost" guarantee. (`lines()` DOES surface an unterminated final chunk; it
    ///   is the parse failure, not line-splitting, that discards it.)
    /// - A corrupt line elsewhere (bit-rot, or a partial write that happened to
    ///   carry a `\n`) is skipped with a warning — one bad line never discards
    ///   the records around it.
    /// - A missing file is an empty log, not an error.
    pub fn load<T: DeserializeOwned>(&self) -> Result<Vec<T>, LychiError> {
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let reader = BufReader::new(file);
        let mut out = Vec::new();
        for (i, line) in reader.lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                // An I/O error mid-read (rare) ends the scan; keep what we have.
                Err(e) => {
                    tracing::warn!(
                        "{}: read error at line {}: {e} — keeping {} prior records",
                        self.path.display(),
                        i + 1,
                        out.len()
                    );
                    break;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<T>(&line) {
                Ok(rec) => out.push(rec),
                Err(e) => tracing::warn!(
                    "{}: skipping corrupt record at line {}: {e}",
                    self.path.display(),
                    i + 1
                ),
            }
        }
        Ok(out)
    }

    /// Replace the entire log with `records`, atomically.
    ///
    /// Used by pruning/compaction: load, drop what's expired, rewrite. The whole
    /// new file is written via the atomic triad (tmp → fsync → rename → dir
    /// fsync), so a crash during a rewrite leaves the OLD complete log, never a
    /// half-pruned one. Every line still ends in `\n`.
    pub fn rewrite<T: Serialize>(&self, records: &[T]) -> Result<(), LychiError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::new();
        for rec in records {
            serde_json::to_writer(&mut buf, rec)
                .map_err(|e| LychiError::Config(format!("jsonl encode: {e}")))?;
            buf.push(b'\n');
        }
        crate::fs_atomic::write_atomic(&self.path, &buf)?;
        restrict_owner_only(&self.path);
        Ok(())
    }

    /// Count committed records cheaply, without decoding.
    ///
    /// Counts newline (`\n`) terminators: a record is committed only once its
    /// terminator is on disk, so a torn trailing line (no `\n`) is correctly
    /// excluded, matching what [`load`](Self::load) keeps. Blank lines are
    /// over-counted and corrupt-but-terminated lines are counted — this is a fast
    /// upper bound for retention decisions, not an exact well-formed count. Use
    /// `load().len()` when exactness matters.
    pub fn approx_len(&self) -> Result<usize, LychiError> {
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        // Count `\n` bytes over a buffered read — a committed record per
        // terminator, torn tail excluded, without loading the file into memory.
        let mut reader = BufReader::new(file);
        let mut count = 0usize;
        let mut chunk = [0u8; 8192];
        loop {
            let n = reader.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            count += chunk[..n].iter().filter(|&&b| b == b'\n').count();
        }
        Ok(count)
    }

    /// True when the log has no records (missing file or empty).
    pub fn is_empty(&self) -> Result<bool, LychiError> {
        Ok(self.approx_len()? == 0)
    }

    /// Delete the whole log (e.g. a "clear history" action). Reclaims the disk
    /// immediately. A missing file is success.
    pub fn clear(&self) -> Result<(), LychiError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Belt-and-suspenders startup repair: if the file does not end in `\n`, a
    /// previous crash left a torn tail. Truncate back to the last complete record
    /// so future appends resume on a clean boundary. Readers already drop the
    /// torn tail, so this is optional — but it stops the partial bytes from ever
    /// being paired with a later append into one malformed line.
    pub fn repair_torn_tail(&self) -> Result<(), LychiError> {
        let mut f = match OpenOptions::new().read(true).write(true).open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let len = f.metadata()?.len();
        if len == 0 {
            return Ok(());
        }
        // Read the final byte.
        f.seek(SeekFrom::End(-1))?;
        let mut last = [0u8; 1];
        f.read_exact(&mut last)?;
        if last[0] == b'\n' {
            return Ok(()); // clean boundary already
        }
        // Scan backwards for the last newline; truncate just after it (or to
        // empty if there is none — the whole file was one torn record).
        let mut contents = Vec::new();
        f.seek(SeekFrom::Start(0))?;
        f.read_to_end(&mut contents)?;
        let keep = match contents.iter().rposition(|&b| b == b'\n') {
            Some(pos) => pos as u64 + 1,
            None => 0,
        };
        f.set_len(keep)?;
        f.sync_all()?;
        tracing::warn!(
            "{}: truncated a torn trailing record ({} → {} bytes)",
            self.path.display(),
            len,
            keep
        );
        Ok(())
    }
}

/// Write a single value as the entire content of `path`, atomically and durably.
///
/// For small state rewritten whole (not appended). A crash during the write
/// leaves either the old complete file or the new one — never a torn value.
pub fn snapshot<T: Serialize>(path: &Path, value: &T) -> Result<(), LychiError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(value)
        .map_err(|e| LychiError::Config(format!("snapshot encode: {e}")))?;
    crate::fs_atomic::write_atomic(path, &bytes)?;
    restrict_owner_only(path);
    Ok(())
}

/// Load a value written by [`snapshot`]. A missing file yields `Ok(None)`; a
/// corrupt file is an error the caller can choose to recover from (e.g. by
/// starting from a default) rather than a silent reset.
pub fn load_snapshot<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, LychiError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let value = serde_json::from_slice(&bytes)
        .map_err(|e| LychiError::Config(format!("snapshot decode {}: {e}", path.display())))?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Rec {
        id: u32,
        text: String,
    }

    static N: AtomicU64 = AtomicU64::new(0);
    fn temp(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "lychi_filestore_{}_{}_{}",
            tag,
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        p
    }

    fn rec(id: u32, text: &str) -> Rec {
        Rec {
            id,
            text: text.to_string(),
        }
    }

    #[test]
    fn append_then_load_round_trips_in_order() {
        let log = JsonlLog::new(temp("roundtrip"));
        log.append(&rec(1, "one")).unwrap();
        log.append(&rec(2, "two")).unwrap();
        log.append(&rec(3, "three")).unwrap();
        let got: Vec<Rec> = log.load().unwrap();
        assert_eq!(got, vec![rec(1, "one"), rec(2, "two"), rec(3, "three")]);
        log.clear().unwrap();
    }

    #[test]
    fn missing_file_is_an_empty_log() {
        let log = JsonlLog::new(temp("missing"));
        let got: Vec<Rec> = log.load().unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn a_record_with_a_newline_in_a_field_stays_one_line() {
        let log = JsonlLog::new(temp("embedded_nl"));
        log.append(&rec(1, "line one\nline two")).unwrap();
        log.append(&rec(2, "next")).unwrap();
        // The embedded newline is escaped, so this is still two records.
        let got: Vec<Rec> = log.load().unwrap();
        assert_eq!(got, vec![rec(1, "line one\nline two"), rec(2, "next")]);
        // And physically two lines.
        let raw = fs::read_to_string(log.path()).unwrap();
        assert_eq!(raw.lines().count(), 2);
        log.clear().unwrap();
    }

    #[test]
    fn torn_trailing_line_is_dropped_on_load() {
        // Simulate a crash mid-append: a valid log plus a partial final line
        // with no terminating newline.
        let log = JsonlLog::new(temp("torn"));
        log.append(&rec(1, "good")).unwrap();
        log.append(&rec(2, "also good")).unwrap();
        // Manually append a torn record (no newline).
        {
            let mut f = OpenOptions::new().append(true).open(log.path()).unwrap();
            f.write_all(b"{\"id\":3,\"text\":\"tor").unwrap();
        }
        let got: Vec<Rec> = log.load().unwrap();
        assert_eq!(
            got,
            vec![rec(1, "good"), rec(2, "also good")],
            "the torn tail must be discarded, the good records kept"
        );
        log.clear().unwrap();
    }

    #[test]
    fn corrupt_middle_line_is_skipped_not_fatal() {
        let log = JsonlLog::new(temp("corrupt_mid"));
        // Write: good, garbage-with-newline, good.
        {
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(log.path())
                .unwrap();
            f.write_all(b"{\"id\":1,\"text\":\"a\"}\n").unwrap();
            f.write_all(b"this is not json at all\n").unwrap();
            f.write_all(b"{\"id\":2,\"text\":\"b\"}\n").unwrap();
        }
        let got: Vec<Rec> = log.load().unwrap();
        assert_eq!(
            got,
            vec![rec(1, "a"), rec(2, "b")],
            "a corrupt middle line is skipped; records around it survive"
        );
        log.clear().unwrap();
    }

    #[test]
    fn blank_lines_are_ignored() {
        let log = JsonlLog::new(temp("blanks"));
        {
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(log.path())
                .unwrap();
            f.write_all(b"{\"id\":1,\"text\":\"a\"}\n\n").unwrap();
            f.write_all(b"   \n").unwrap();
            f.write_all(b"{\"id\":2,\"text\":\"b\"}\n").unwrap();
        }
        let got: Vec<Rec> = log.load().unwrap();
        assert_eq!(got, vec![rec(1, "a"), rec(2, "b")]);
        log.clear().unwrap();
    }

    #[test]
    fn rewrite_replaces_atomically() {
        let log = JsonlLog::new(temp("rewrite"));
        log.append(&rec(1, "one")).unwrap();
        log.append(&rec(2, "two")).unwrap();
        log.append(&rec(3, "three")).unwrap();
        // Prune to the last two.
        let kept = vec![rec(2, "two"), rec(3, "three")];
        log.rewrite(&kept).unwrap();
        let got: Vec<Rec> = log.load().unwrap();
        assert_eq!(got, kept);
        // No stray .part left behind.
        assert!(!log.path().with_extension("jsonl.part").exists());
        log.clear().unwrap();
    }

    #[test]
    fn approx_len_and_is_empty() {
        let log = JsonlLog::new(temp("approxlen"));
        assert!(log.is_empty().unwrap());
        assert_eq!(log.approx_len().unwrap(), 0);
        log.append(&rec(1, "a")).unwrap();
        log.append(&rec(2, "b")).unwrap();
        assert!(!log.is_empty().unwrap());
        assert_eq!(log.approx_len().unwrap(), 2);
        // A torn tail is not counted as a committed record.
        {
            let mut f = OpenOptions::new().append(true).open(log.path()).unwrap();
            f.write_all(b"{\"id\":3,\"text\":\"tor").unwrap();
        }
        assert_eq!(log.approx_len().unwrap(), 2);
        log.clear().unwrap();
    }

    #[test]
    fn clear_removes_and_is_idempotent() {
        let log = JsonlLog::new(temp("clear"));
        log.append(&rec(1, "x")).unwrap();
        log.clear().unwrap();
        assert!(!log.path().exists());
        // Clearing again is fine.
        log.clear().unwrap();
        let got: Vec<Rec> = log.load().unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn repair_truncates_a_torn_tail_to_a_clean_boundary() {
        let log = JsonlLog::new(temp("repair"));
        log.append(&rec(1, "good")).unwrap();
        {
            let mut f = OpenOptions::new().append(true).open(log.path()).unwrap();
            f.write_all(b"{\"id\":2,\"partial").unwrap();
        }
        log.repair_torn_tail().unwrap();
        // File now ends in \n and holds exactly the one good record.
        let raw = fs::read_to_string(log.path()).unwrap();
        assert!(raw.ends_with('\n'));
        let got: Vec<Rec> = log.load().unwrap();
        assert_eq!(got, vec![rec(1, "good")]);
        // A subsequent append lands cleanly as its own record.
        log.append(&rec(3, "after repair")).unwrap();
        let got2: Vec<Rec> = log.load().unwrap();
        assert_eq!(got2, vec![rec(1, "good"), rec(3, "after repair")]);
        log.clear().unwrap();
    }

    #[test]
    fn repair_of_a_file_with_no_newline_at_all_empties_it() {
        let log = JsonlLog::new(temp("repair_all_torn"));
        {
            let mut f = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(log.path())
                .unwrap();
            f.write_all(b"{\"id\":1,\"never finished").unwrap();
        }
        log.repair_torn_tail().unwrap();
        assert_eq!(fs::metadata(log.path()).unwrap().len(), 0);
        log.clear().unwrap();
    }

    #[test]
    fn snapshot_round_trips() {
        let path = temp("snap");
        let v = vec![rec(1, "a"), rec(2, "b")];
        snapshot(&path, &v).unwrap();
        let got: Option<Vec<Rec>> = load_snapshot(&path).unwrap();
        assert_eq!(got, Some(v));
        fs::remove_file(&path).ok();
    }

    #[cfg(unix)]
    #[test]
    fn store_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        // Append log: 0600 from birth.
        let log = JsonlLog::new(temp("perm_log"));
        log.append(&rec(1, "secret clip")).unwrap();
        let m = fs::metadata(log.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(m, 0o600, "jsonl log must be owner-only, got {m:o}");
        // Rewrite keeps it 0600.
        log.rewrite(&[rec(1, "a")]).unwrap();
        let m = fs::metadata(log.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(m, 0o600, "rewritten log must stay owner-only, got {m:o}");
        log.clear().unwrap();

        // Snapshot: 0600 too.
        let path = temp("perm_snap");
        snapshot(&path, &vec![rec(1, "a")]).unwrap();
        let m = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(m, 0o600, "snapshot must be owner-only, got {m:o}");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn load_snapshot_missing_is_none() {
        let path = temp("snap_missing");
        let got: Option<Vec<Rec>> = load_snapshot(&path).unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn load_snapshot_corrupt_is_an_error_not_a_silent_default() {
        let path = temp("snap_corrupt");
        fs::write(&path, b"{ not valid json").unwrap();
        let got: Result<Option<Vec<Rec>>, _> = load_snapshot(&path);
        assert!(got.is_err(), "a corrupt snapshot must surface, not reset");
        fs::remove_file(&path).ok();
    }
}
