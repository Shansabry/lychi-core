//! Universal, per-IDE-agnostic workspace detection from `/proc`.
//!
//! Given the focused window's PID, walk its process tree and read each
//! process's `cwd` (symlink) and `cmdline` (launch args). A process launched
//! as `code /path/to/proj` (terminal launch) reveals the project directly —
//! this works for ANY editor (VS Code, JetBrains JVM, Zed, Neovide, or one
//! that doesn't exist yet), needing zero product-specific knowledge.
//!
//! It does NOT resolve GUI/menu-launched editors — those inherit `$HOME` as
//! cwd and carry no folder arg (that's the config-file tier's job). So every
//! candidate is validated against a real project marker and `$HOME`/`/` are
//! rejected, keeping this a high-precision primary signal.

use std::path::{Path, PathBuf};

/// Detect the open project directory for an IDE window from its process tree.
/// Returns a validated project path, or `None` if `/proc` yields no ground
/// truth (caller should fall through to config/title tiers).
pub fn detect(window_pid: u32) -> Option<String> {
    if window_pid == 0 {
        return None;
    }
    let home = std::env::var("HOME").ok();

    // Collect the window's process and its tree (ancestors + descendants).
    // The window PID from the compositor is often a wrapper/zygote/Electron
    // main; the useful cwd/cmdline may be on a relative.
    let pids = collect_tree(window_pid);

    // Prefer the "main" process (Electron main has no `--type=` arg; JVM/native
    // launchers have none either) — its cmdline/cwd is the likeliest to hold
    // the folder. Non-main renderers are sandboxed (garbage cwd) — try last.
    let (mains, rest): (Vec<u32>, Vec<u32>) = pids.iter().copied().partition(|&p| is_main_proc(p));
    let ordered: Vec<u32> = mains.into_iter().chain(rest).collect();

    for pid in ordered {
        // 1. cmdline args — authoritative when a folder was passed on launch.
        for cand in cmdline_paths(pid) {
            if let Some(valid) = validate(&cand, home.as_deref()) {
                return Some(valid);
            }
        }
        // 2. cwd — ground truth for terminal-launched editors.
        if let Some(cwd) = read_cwd(pid)
            && let Some(valid) = validate(&cwd, home.as_deref())
        {
            return Some(valid);
        }
    }
    None
}

/// Whether a path is a real project directory: exists, is a dir, is not a
/// noise root (`$HOME`, `/`), and contains a project marker.
fn validate(path: &Path, home: Option<&str>) -> Option<String> {
    if !path.is_dir() {
        return None;
    }
    // Reject noise roots that a GUI launch leaves as cwd.
    if path == Path::new("/") {
        return None;
    }
    if let Some(h) = home
        && path == Path::new(h)
    {
        return None;
    }
    // Must look like an actual project (reuses the shared marker check).
    super::ide::which_project_marker(path)?;
    Some(path.to_string_lossy().into_owned())
}

/// Read `/proc/<pid>/cwd` (a symlink to the working directory).
fn read_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Extract candidate project paths from `/proc/<pid>/cmdline`.
/// Handles plain dir args, `file://` URIs, and `--folder-uri <uri>`.
/// Remote windows (`vscode-remote://` and other non-`file:` schemes) yield no
/// LOCAL path — we skip them so we never return a misleading local dir.
fn cmdline_paths(pid: u32) -> Vec<PathBuf> {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return Vec::new();
    };
    let args: Vec<String> = raw
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();

    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        // `--folder-uri <uri>` or `--folder-uri=<uri>`
        if let Some(uri) = arg.strip_prefix("--folder-uri=").or_else(|| {
            (arg == "--folder-uri")
                .then(|| args.get(i + 1).map(|s| s.as_str()))
                .flatten()
        }) {
            if arg == "--folder-uri" {
                i += 1;
            }
            if let Some(p) = uri_to_local_path(uri) {
                out.push(p);
            }
        } else if let Some(p) = arg_to_path(arg) {
            out.push(p);
        }
        i += 1;
    }
    out
}

/// A bare cmdline arg → a local path candidate (plain path or `file://` URI).
/// Skips flags and non-`file:` remote URIs.
fn arg_to_path(arg: &str) -> Option<PathBuf> {
    if arg.starts_with('-') {
        return None;
    }
    if arg.contains("://") {
        return uri_to_local_path(arg);
    }
    // Only absolute paths are unambiguous ground truth.
    let p = PathBuf::from(arg);
    p.is_absolute().then_some(p)
}

/// `file:///abs/path` → `/abs/path`. Any non-`file:` scheme (e.g.
/// `vscode-remote://…`) → `None` (remote; no meaningful local path).
pub(crate) fn uri_to_local_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file:///path` → strip the host part (empty) leaving `/path`.
    let path = rest.strip_prefix('/').map(|_| rest).unwrap_or(rest);
    // Percent-decode the common cases (space, etc.) minimally.
    let decoded = percent_decode(path);
    let p = PathBuf::from(decoded);
    p.is_absolute().then_some(p)
}

/// Minimal percent-decoding for path URIs (no external dep).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Whether a process is a "main" process (no Electron `--type=` sub-process
/// arg). Renderers/gpu/utility/zygote all carry `--type=…` and are sandboxed.
fn is_main_proc(pid: u32) -> bool {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    !raw.split(|&b| b == 0).any(|s| s.starts_with(b"--type="))
}

/// Collect the window PID plus its ancestors and descendants (bounded), so we
/// find the process actually holding the folder regardless of wrapper layers.
fn collect_tree(window_pid: u32) -> Vec<u32> {
    let mut seen = vec![window_pid];

    // Ancestors via PPid, up to the session (bounded to avoid runaway).
    let mut cur = window_pid;
    for _ in 0..8 {
        match parent_pid(cur) {
            Some(ppid) if ppid > 1 && !seen.contains(&ppid) => {
                seen.push(ppid);
                cur = ppid;
            }
            _ => break,
        }
    }

    // Descendants via /proc/<pid>/task/<pid>/children (one level from each
    // known pid; bounded breadth). Enough to reach the Electron main / JVM
    // when the window pid is a wrapper above it.
    let mut frontier = seen.clone();
    for _ in 0..3 {
        let mut next = Vec::new();
        for &p in &frontier {
            for child in child_pids(p) {
                if !seen.contains(&child) {
                    seen.push(child);
                    next.push(child);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    seen
}

/// Parent PID from `/proc/<pid>/status` (`PPid:` line).
fn parent_pid(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("PPid:"))
        .and_then(|v| v.trim().parse().ok())
}

/// Child PIDs from `/proc/<pid>/task/<pid>/children` (space-separated).
fn child_pids(pid: u32) -> Vec<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .ok()
        .map(|s| {
            s.split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_conversion() {
        assert_eq!(
            uri_to_local_path("file:///home/u/proj"),
            Some(PathBuf::from("/home/u/proj"))
        );
        assert_eq!(
            uri_to_local_path("file:///home/u/my%20proj"),
            Some(PathBuf::from("/home/u/my proj"))
        );
        // Remote / non-file schemes yield no local path.
        assert_eq!(
            uri_to_local_path("vscode-remote://ssh-remote+host/root/x"),
            None
        );
        assert_eq!(uri_to_local_path("http://example.com"), None);
    }

    #[test]
    fn arg_to_path_filters() {
        assert_eq!(arg_to_path("/abs/dir"), Some(PathBuf::from("/abs/dir")));
        assert_eq!(arg_to_path("--flag"), None);
        assert_eq!(arg_to_path("relative/dir"), None); // not absolute
        assert_eq!(arg_to_path("file:///abs"), Some(PathBuf::from("/abs")));
    }

    #[test]
    fn percent_decode_basics() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("%2Fx"), "/x");
    }

    #[test]
    fn validate_rejects_noise_roots() {
        // $HOME and / are never valid detected projects even if they exist.
        assert_eq!(validate(Path::new("/"), Some("/home/u")), None);
        assert_eq!(validate(Path::new("/home/u"), Some("/home/u")), None);
        // A non-existent path is rejected.
        assert_eq!(validate(Path::new("/nonexistent/xyz"), None), None);
    }

    #[test]
    fn pid_zero_is_none() {
        assert_eq!(detect(0), None);
    }
}
