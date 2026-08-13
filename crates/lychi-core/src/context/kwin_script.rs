//! One place to stage a KWin JS probe script on disk before `loadScript` runs
//! it. Every KWin scripting probe (active-window, window-stack) writes through
//! here so a fifth probe can't reintroduce the bug this fixes.
//!
//! The bug (PLAT-4): probes wrote their JS to a FIXED name in the shared temp
//! dir (`/tmp/lychi_ctx_active.js`). Two things went wrong:
//!
//! - **Self-collision.** The in-process watcher (200ms–8s) and the pre-summon
//!   snapshot raced the same path; the loser's file got overwritten mid-flight,
//!   its callback never arrived, and it burned its full deadline (a summon stall).
//! - **Cross-user surface.** A world-readable/writable fixed name in `/tmp` is a
//!   shared path other users can pre-create or swap (degraded to DoS by
//!   `fs.protected_regular` on modern systemd, but the fix is the same).
//!
//! Fix: a UNIQUE per-call file (`lychi_kwin_{pid}_{counter}.js`) in
//! `XDG_RUNTIME_DIR` — a per-user 0700 dir that is guaranteed to exist wherever
//! KWin runs (it's part of the same systemd/logind session). Callers delete the
//! file when the probe finishes; a leftover (process killed mid-probe) is
//! reclaimed by logind when the runtime dir is torn down at logout.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic per-call counter so two probes in the same process (and the same
/// millisecond) never pick the same file name.
static SCRIPT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Per-user runtime dir for staging scripts: `$XDG_RUNTIME_DIR` (0700, per the
/// XDG spec), falling back to the system temp dir only if it's unset — which
/// shouldn't happen on a logind session where KWin lives, but we degrade rather
/// than fail the probe.
fn script_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Write `contents` to a unique KWin-script file and return its path, or `None`
/// if the write fails. The caller passes the path to `loadScript` and should
/// remove the file once KWin has compiled the script.
pub fn write_temp_script(contents: &str) -> Option<PathBuf> {
    let seq = SCRIPT_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = script_dir().join(format!("lychi_kwin_{}_{seq}.js", std::process::id()));
    std::fs::write(&path, contents).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_call_gets_a_distinct_path() {
        let a = write_temp_script("// a").expect("write a");
        let b = write_temp_script("// b").expect("write b");
        assert_ne!(a, b, "two calls must not collide on one path");
        // Both readable back as written (no overwrite of one by the other).
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "// a");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "// b");
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }

    #[test]
    fn path_lives_under_the_runtime_dir_when_set() {
        // Only assert the relationship when XDG_RUNTIME_DIR is present (CI may not
        // set it); the fallback is deliberately the temp dir.
        if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
            let p = write_temp_script("// x").expect("write");
            assert!(p.starts_with(PathBuf::from(rt)));
            let _ = std::fs::remove_file(&p);
        }
    }
}
