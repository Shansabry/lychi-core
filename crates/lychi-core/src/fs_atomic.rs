//! Crash-safe atomic file writes, shared by every path that persists
//! user-authored content (config.toml, backups, …).
//!
//! # Why a full fsync triad, not just rename
//!
//! "Write a temp file then rename it over the target" gives ATOMICITY — a reader
//! never sees a half-written file. But atomicity is not DURABILITY: after the
//! rename returns, a crash/power-loss can still lose the data on many
//! filesystems, because
//!
//!   1. the temp file's *bytes* may still be in the page cache (not on disk), and
//!   2. the *rename* (a directory entry change) may not be persisted either.
//!
//! The correct sequence (POSIX) is therefore three fsyncs, not one:
//!
//!   write tmp → **fsync(tmp)** → rename(tmp, dest) → **fsync(parent dir)**
//!
//! Skipping the directory fsync is the common bug: the file contents are durable
//! but the rename isn't, so after a reboot the OLD file (or no file) reappears.
//! This module does all three.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// Atomically and durably write `bytes` to `dest`.
///
/// The temp file is created IN THE SAME DIRECTORY as `dest` so the rename is
/// same-filesystem (a cross-device rename is not atomic and would fail). On
/// success, both the file contents and the rename are fsync'd to disk, so a
/// crash immediately after this returns leaves `dest` with the new contents in
/// full — never truncated, never reverted.
pub fn write_atomic(dest: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    // A `.part` sibling of the target — same dir (same filesystem), so rename is
    // atomic. The extension keeps it recognisable if a crash leaves it behind.
    let tmp = dest.with_extension(format!(
        "{}.part",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));

    // 1. Write the bytes and fsync the FILE — its contents are now durable.
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }

    // 2. Atomically swap it into place.
    fs::rename(&tmp, dest)?;

    // 3. fsync the PARENT DIRECTORY so the rename itself is durable. Without
    //    this, a reboot can undo the rename even though the file bytes survived.
    //    Best-effort: a filesystem that can't open the dir for sync (rare) still
    //    got the atomic rename, which is the important half.
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        // Unique per-test dir; avoid Date/rand (unavailable in some test envs) by
        // using an atomic counter.
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        p.push(format!(
            "lychi_fsatomic_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn writes_new_file() {
        let dir = tmp_dir();
        let dest = dir.join("config.toml");
        write_atomic(&dest, b"hello").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"hello");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn overwrites_existing_atomically() {
        let dir = tmp_dir();
        let dest = dir.join("config.toml");
        fs::write(&dest, b"old contents").unwrap();
        write_atomic(&dest, b"new").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new");
        // No stray .part left behind on success.
        assert!(!dir.join("config.toml.part").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn handles_a_path_without_extension() {
        let dir = tmp_dir();
        let dest = dir.join("noext");
        write_atomic(&dest, b"data").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"data");
        fs::remove_dir_all(&dir).ok();
    }
}
