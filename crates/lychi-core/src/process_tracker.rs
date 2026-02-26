//! Process tracker — tracks terminal processes spawned by `run` commands.
//!
//! When a `run` command opens a terminal, we record the PID so the user can
//! list running processes and kill them from Lychi.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// A process spawned by Lychi's `run` command.
#[derive(Debug, Clone, Serialize)]
pub struct TrackedProcess {
    pub pid: u32,
    pub command: String,
    pub cwd: Option<String>,
    pub started_at: u64,
}

static TRACKED: Mutex<Vec<TrackedProcess>> = Mutex::new(Vec::new());

/// Register a spawned process.
pub fn track(pid: u32, command: &str, cwd: Option<&str>) {
    let entry = TrackedProcess {
        pid,
        command: command.to_string(),
        cwd: cwd.map(|s| s.to_string()),
        started_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    if let Ok(mut guard) = TRACKED.lock() {
        guard.push(entry);
    }
    tracing::debug!("process_tracker: tracking pid={} cmd={}", pid, command);
}

/// Remove a process from tracking.
pub fn untrack(pid: u32) {
    if let Ok(mut guard) = TRACKED.lock() {
        guard.retain(|p| p.pid != pid);
    }
}

/// List all tracked processes, pruning dead ones.
///
/// Checks `/proc/<pid>` to determine if a process is still alive.
/// Dead processes are automatically removed.
pub fn list() -> Vec<TrackedProcess> {
    let mut guard = match TRACKED.lock() {
        Ok(g) => g,
        Err(_) => return Vec::new(),
    };

    // Prune dead processes
    guard.retain(|p| is_alive(p.pid));

    guard.clone()
}

/// Kill a tracked process by PID or command substring match.
///
/// Returns a description of what was killed, or an error message.
pub fn kill_by(query: &str) -> Result<String, String> {
    let query = query.trim();

    // Try as PID first
    if let Ok(pid) = query.parse::<u32>() {
        return kill_pid(pid);
    }

    // Search by command substring
    let processes = list();
    let matches: Vec<&TrackedProcess> = processes
        .iter()
        .filter(|p| p.command.contains(query))
        .collect();

    match matches.len() {
        0 => Err(format!("No running process matching '{query}'")),
        1 => kill_pid(matches[0].pid),
        n => {
            let names: Vec<String> = matches
                .iter()
                .map(|p| format!("  {} (pid={})", p.command, p.pid))
                .collect();
            Err(format!(
                "Multiple matches ({n}), specify PID:\n{}",
                names.join("\n")
            ))
        }
    }
}

/// Send SIGTERM to a process (and its process group), then SIGKILL if still alive.
fn kill_pid(pid: u32) -> Result<String, String> {
    use nix::sys::signal::Signal;

    if !is_alive(pid) {
        untrack(pid);
        return Err(format!("Process {pid} already exited"));
    }

    // Get command name before killing
    let cmd_name = TRACKED
        .lock()
        .ok()
        .and_then(|g| g.iter().find(|p| p.pid == pid).map(|p| p.command.clone()))
        .unwrap_or_else(|| format!("pid {pid}"));

    if let Err(e) = signal_kill(pid, Signal::SIGTERM) {
        untrack(pid);
        return Err(format!("Failed to kill {pid}: {e}"));
    }

    std::thread::sleep(std::time::Duration::from_millis(200));

    if is_alive(pid) {
        let _ = signal_kill(pid, Signal::SIGKILL);
    }

    untrack(pid);
    Ok(format!("Killed: {cmd_name} (pid={pid})"))
}

/// Check if a process is alive via `/proc/<pid>`.
fn is_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

// ── System-wide process scanning ────────────────────────────────────────

/// A process discovered from `/proc` (not necessarily spawned by Lychi).
#[derive(Debug, Clone, Serialize)]
pub struct SystemProcess {
    pub pid: u32,
    /// Full command line (args joined by spaces).
    pub cmdline: String,
    /// Short process name from `/proc/<pid>/comm`.
    pub comm: String,
}

/// Scan `/proc` for user-owned processes matching `query`.
///
/// Uses a 500ms TTL cache to avoid re-walking `/proc` on every keystroke.
/// Matches case-insensitively against both `cmdline` and `comm`.
/// Returns up to 20 matches.
pub fn scan_system(query: &str) -> Vec<SystemProcess> {
    let query_lower = query.trim().to_lowercase();
    if query_lower.is_empty() {
        return Vec::new();
    }

    let all = get_all_user_processes();

    all.into_iter()
        .filter(|p| {
            p.comm.to_lowercase().contains(&query_lower)
                || p.cmdline.to_lowercase().contains(&query_lower)
        })
        .take(20)
        .collect()
}

/// Kill a system process by PID (not necessarily tracked by Lychi).
///
/// Sends SIGTERM to the process group, waits briefly, then SIGKILL if still alive.
pub fn kill_system_pid(pid: u32) -> Result<String, String> {
    use nix::sys::signal::Signal;

    if !is_alive(pid) {
        return Err(format!("Process {pid} already exited"));
    }

    // Read command name for the response message
    let cmd_name = std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let label = if cmd_name.is_empty() {
        format!("pid {pid}")
    } else {
        format!("{cmd_name} (pid={pid})")
    };

    if let Err(e) = signal_kill(pid, Signal::SIGTERM) {
        return Err(format!("Failed to kill {label}: {e}"));
    }

    std::thread::sleep(std::time::Duration::from_millis(200));

    if is_alive(pid) {
        let _ = signal_kill(pid, Signal::SIGKILL);
    }

    // Invalidate /proc scan cache after a kill
    invalidate_proc_cache();

    Ok(format!("Killed: {label}"))
}

/// Send a signal to a process, preferring the process group when safe.
///
/// Tries to kill the entire process group (PGID) so child processes are cleaned
/// up too (e.g. `pnpm` spawning `node`). Falls back to single-PID kill when:
/// - PGID lookup fails
/// - PGID is 1 (init) or Lychi's own PID
fn signal_kill(pid: u32, signal: nix::sys::signal::Signal) -> Result<(), nix::errno::Errno> {
    use nix::sys::signal::kill;
    use nix::unistd::{Pid, getpgid};

    let nix_pid = Pid::from_raw(pid as i32);
    let my_pid = std::process::id() as i32;

    // Try process-group kill for broader cleanup
    if let Ok(pgid) = getpgid(Some(nix_pid)) {
        let pgid_raw = pgid.as_raw();
        // Only kill the group if it's safe: not init, not our own group
        if pgid_raw > 1 && pgid_raw != my_pid {
            let group_pid = Pid::from_raw(-pgid_raw);
            return kill(group_pid, signal);
        }
    }

    // Fallback: kill just the single process
    kill(nix_pid, signal)
}

// ── /proc scan cache ────────────────────────────────────────────────────

struct ProcCache {
    processes: Vec<SystemProcess>,
    fetched_at: Instant,
}

static PROC_CACHE: Mutex<Option<ProcCache>> = Mutex::new(None);
const PROC_CACHE_TTL_MS: u64 = 500;

/// Invalidate the /proc scan cache (call after killing a process).
fn invalidate_proc_cache() {
    if let Ok(mut guard) = PROC_CACHE.lock() {
        *guard = None;
    }
}

/// Get all user-owned processes, using cache if fresh.
fn get_all_user_processes() -> Vec<SystemProcess> {
    if let Ok(cache) = PROC_CACHE.lock()
        && let Some(ref c) = *cache
        && c.fetched_at.elapsed().as_millis() < PROC_CACHE_TTL_MS as u128
    {
        return c.processes.clone();
    }

    let processes = scan_all_user_processes();

    if let Ok(mut cache) = PROC_CACHE.lock() {
        *cache = Some(ProcCache {
            processes: processes.clone(),
            fetched_at: Instant::now(),
        });
    }

    processes
}

/// Walk /proc and collect all user-owned processes (unfiltered).
fn scan_all_user_processes() -> Vec<SystemProcess> {
    let my_pid = std::process::id();
    let mut results = Vec::new();

    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Ok(pid) = name_str.parse::<u32>() else {
            continue;
        };

        if pid <= 2 || pid == my_pid {
            continue;
        }

        let proc_path = format!("/proc/{pid}");

        if is_root_owned(&proc_path) {
            continue;
        }

        let comm = std::fs::read_to_string(format!("{proc_path}/comm"))
            .unwrap_or_default()
            .trim()
            .to_string();

        let cmdline = read_cmdline(&proc_path);

        results.push(SystemProcess { pid, cmdline, comm });
    }

    results
}

/// Read `/proc/<pid>/cmdline` and return it as a space-joined string.
fn read_cmdline(proc_path: &str) -> String {
    std::fs::read(format!("{proc_path}/cmdline"))
        .unwrap_or_default()
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Check if a process is owned by root (UID 0) via `/proc/<pid>/status`.
fn is_root_owned(proc_path: &str) -> bool {
    let Ok(status) = std::fs::read_to_string(format!("{proc_path}/status")) else {
        return true; // Can't read → skip
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            // Format: "Uid:\t<real>\t<effective>\t<saved>\t<fs>"
            // Check real UID
            if let Some(uid_str) = rest.split_whitespace().next() {
                return uid_str == "0";
            }
        }
    }
    true // Couldn't parse → skip
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_track_and_list() {
        // Track a fake PID that definitely doesn't exist
        track(999_999_999, "test command", Some("/tmp"));

        // list() should prune it since it's not alive
        let procs = list();
        assert!(
            !procs.iter().any(|p| p.pid == 999_999_999),
            "Dead process should be pruned"
        );
    }

    #[test]
    fn test_kill_nonexistent() {
        let result = kill_by("999999999");
        assert!(result.is_err());
    }

    #[test]
    fn test_kill_no_match() {
        let result = kill_by("zzz_nonexistent_command_zzz");
        assert!(result.is_err());
    }

    #[test]
    fn test_scan_system_finds_self() {
        // Our own test process should be discoverable (it runs as cargo test)
        let results = scan_system("cargo");
        // We might find cargo processes — just verify no panic and results are valid
        for proc in &results {
            assert!(proc.pid > 2);
            assert!(!proc.comm.is_empty());
        }
    }

    #[test]
    fn test_scan_system_no_match() {
        let results = scan_system("zzz_impossible_process_name_zzz");
        assert!(results.is_empty());
    }
}
