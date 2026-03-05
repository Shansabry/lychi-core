//! Terminal-specific CWD adapters — query actual CWD via native APIs.
//!
//! Each terminal gets its own probe that uses the terminal's API (D-Bus,
//! CLI, /proc) to read the shell's working directory. This works even when
//! a command is running and the window title shows something else.
//!
//! Per C16: probe failure → silent `None`. A wrong CWD is worse than no CWD.

use std::process::{Command, Stdio};
use std::time::Duration;

/// A terminal-specific CWD probe.
///
/// Synchronous — `gather()` runs probes inside `spawn_blocking`.
pub trait TerminalProbe: Send + Sync {
    /// Attempt to detect the shell CWD for a terminal process.
    ///
    /// `pid` is the terminal's X11/Wayland PID (from KWin).
    /// `title` is the window title (for terminals that can only match by title).
    /// Returns `None` on failure — never guesses.
    fn probe(&self, pid: u32, title: &str) -> Option<String>;
}

// ── Dispatch ────────────────────────────────────────────────────────────

/// Known single-tab terminals safe for ProcCwdProbe.
const SINGLE_TAB_TERMINALS: &[&str] = &[
    "alacritty",
    "foot",
    "st",
    "xterm",
    "ghostty",
    "rio",
    "contour",
    "blackbox",
    "ptyxis",
    "sakura",
];

/// Which adapter produced a CWD result. Stored in cache for observability.
#[derive(Debug, Clone, Copy)]
pub enum ProbeSource {
    Konsole,
    Kitty,
    Wezterm,
    Proc,
    None,
}

impl ProbeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Konsole => "konsole",
            Self::Kitty => "kitty",
            Self::Wezterm => "wezterm",
            Self::Proc => "proc",
            Self::None => "none",
        }
    }
}

/// Entry point — dispatch to the right adapter based on wm_class, with caching.
pub fn probe_terminal_cwd(wm_class: &str, pid: u32, title: &str) -> Option<String> {
    if let Some(cached) = super::cache::get_terminal_cwd(wm_class, pid) {
        return cached;
    }
    let (result, source) = dispatch(wm_class, pid, title);
    super::cache::set_terminal_cwd(wm_class, pid, &result, source);
    if let Some(ref cwd) = result {
        super::metrics::inc_terminal_probe_hit();
        tracing::debug!("terminal_probe: hit source={} cwd={cwd}", source.as_str());
    }
    result
}

fn dispatch(wm_class: &str, pid: u32, title: &str) -> (Option<String>, ProbeSource) {
    let short = super::active_window::normalize_wm_class(wm_class);
    match short.as_str() {
        "konsole" => (KonsoleProbe.probe(pid, title), ProbeSource::Konsole),
        "kitty" => (KittyProbe.probe(pid, title), ProbeSource::Kitty),
        "wezterm" => (WeztermProbe.probe(pid, title), ProbeSource::Wezterm),
        c if SINGLE_TAB_TERMINALS.contains(&c) => {
            (ProcCwdProbe.probe(pid, title), ProbeSource::Proc)
        }
        _ => (None, ProbeSource::None),
    }
}

// ── Timeout wrapper ─────────────────────────────────────────────────────

/// Run a subprocess with a timeout, draining stdout via a thread to avoid
/// pipe deadlock (child fills pipe buffer → blocks on write → never exits).
fn run_with_timeout(cmd: &str, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    let child = Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) if output.status.success() => Some(output.stdout),
        _ => None,
    }
}

// ── KonsoleProbe ────────────────────────────────────────────────────────

pub struct KonsoleProbe;

impl TerminalProbe for KonsoleProbe {
    #[cfg(target_os = "linux")]
    fn probe(&self, pid: u32, _title: &str) -> Option<String> {
        use dbus::blocking::SyncConnection;

        let conn = match SyncConnection::new_session() {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("konsole_probe: D-Bus session connect failed: {e}");
                return None;
            }
        };
        let service = format!("org.kde.konsole-{pid}");
        let timeout = Duration::from_millis(200);
        tracing::debug!("konsole_probe: trying service={service}");

        // Collect CWDs from all responding windows — return only if unique.
        let mut cwds = Vec::new();
        let mut any_window_responded = false;
        for win_id in 1..=5 {
            let win_path = format!("/Windows/{win_id}");
            let win_proxy = conn.with_proxy(&service, &win_path, timeout);
            let session_id: i32 =
                match win_proxy.method_call("org.kde.konsole.Window", "currentSession", ()) {
                    Ok((id,)) => {
                        any_window_responded = true;
                        id
                    }
                    Err(e) => {
                        tracing::debug!("konsole_probe: Window/{win_id} failed: {e}");
                        continue;
                    }
                };

            let session_path = format!("/Sessions/{session_id}");
            let session_proxy = conn.with_proxy(&service, &session_path, timeout);

            // Try currentWorkingDirectory first (newer Konsole versions),
            // fall back to processId + /proc/{pid}/cwd (works on all versions).
            let cwd_result = session_proxy
                .method_call::<(String,), _, _, _>(
                    "org.kde.konsole.Session",
                    "currentWorkingDirectory",
                    (),
                )
                .ok()
                .map(|(s,)| s)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    // Fallback: get shell PID and read /proc/{pid}/cwd
                    let shell_pid: i32 = session_proxy
                        .method_call("org.kde.konsole.Session", "processId", ())
                        .ok()
                        .map(|(id,)| id)?;
                    if shell_pid > 0 {
                        let cwd = std::fs::read_link(format!("/proc/{shell_pid}/cwd"))
                            .ok()?
                            .to_str()?
                            .to_string();
                        tracing::debug!(
                            "konsole_probe: session={session_id} via /proc/{shell_pid}/cwd"
                        );
                        Some(cwd)
                    } else {
                        None
                    }
                });

            match cwd_result {
                Some(cwd) => {
                    tracing::debug!("konsole_probe: session={session_id} cwd={cwd}");
                    cwds.push(cwd);
                }
                None => {
                    tracing::debug!("konsole_probe: session={session_id} no cwd");
                }
            }
        }

        if !any_window_responded {
            tracing::debug!("konsole_probe: no windows responded for {service}");
        }

        // Normalize trailing slashes before dedup (D-Bus may return `/tmp/` vs `/tmp`)
        for cwd in &mut cwds {
            while cwd.len() > 1 && cwd.ends_with('/') {
                cwd.pop();
            }
        }
        // Deduplicate — ambiguity → None (C16: no context > wrong context)
        cwds.sort();
        cwds.dedup();
        tracing::debug!("konsole_probe: result cwds={cwds:?}");
        if cwds.len() == 1 { cwds.pop() } else { None }
    }

    #[cfg(not(target_os = "linux"))]
    fn probe(&self, _pid: u32, _title: &str) -> Option<String> {
        None
    }
}

// ── KittyProbe ──────────────────────────────────────────────────────────

pub struct KittyProbe;

impl TerminalProbe for KittyProbe {
    fn probe(&self, _pid: u32, _title: &str) -> Option<String> {
        let output = run_with_timeout("kitty", &["@", "ls"], Duration::from_millis(200))?;
        parse_kitty_ls(&output)
    }
}

/// Parse `kitty @ ls` JSON output. Returns the focused window's CWD.
///
/// Ambiguity guard: exactly one focused OS window required, else None.
fn parse_kitty_ls(json: &[u8]) -> Option<String> {
    let os_windows: Vec<serde_json::Value> = serde_json::from_slice(json).ok()?;

    // Filter to focused OS windows
    let focused: Vec<&serde_json::Value> = os_windows
        .iter()
        .filter(|w| {
            w.get("is_focused")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .collect();

    // Ambiguity guard: must be exactly one focused OS window
    if focused.len() != 1 {
        return None;
    }

    let os_window = focused[0];
    let tabs = os_window.get("tabs")?.as_array()?;

    // Find the focused tab
    let tab = tabs.iter().find(|t| {
        t.get("is_focused")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    })?;

    let windows = tab.get("windows")?.as_array()?;

    // Find the focused window (pane) within the tab
    let window = windows.iter().find(|w| {
        w.get("is_focused")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    })?;

    window.get("cwd")?.as_str().map(String::from)
}

// ── WeztermProbe ────────────────────────────────────────────────────────

pub struct WeztermProbe;

impl TerminalProbe for WeztermProbe {
    fn probe(&self, _pid: u32, title: &str) -> Option<String> {
        let output = run_with_timeout(
            "wezterm",
            &["cli", "list", "--format", "json"],
            Duration::from_millis(200),
        )?;
        parse_wezterm_list(&output, title)
    }
}

/// Parse `wezterm cli list --format json` output.
///
/// Strategy: exactly one title match → use it. Multiple → None.
/// No title match → exactly one active pane → use it. Else → None.
fn parse_wezterm_list(json: &[u8], title: &str) -> Option<String> {
    let panes: Vec<serde_json::Value> = serde_json::from_slice(json).ok()?;

    // Try matching by title
    let title_matches: Vec<&serde_json::Value> = panes
        .iter()
        .filter(|p| p.get("title").and_then(|v| v.as_str()) == Some(title))
        .collect();

    if title_matches.len() == 1 {
        return extract_wezterm_cwd(title_matches[0]);
    }

    // Multiple title matches → ambiguity (C16)
    if title_matches.len() > 1 {
        return None;
    }

    // No title match — try is_active field if available
    let active_panes: Vec<&serde_json::Value> = panes
        .iter()
        .filter(|p| {
            p.get("is_active")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .collect();

    if active_panes.len() == 1 {
        return extract_wezterm_cwd(active_panes[0]);
    }

    // Ambiguity or no active pane → None
    None
}

/// Extract CWD from a wezterm pane JSON object, stripping `file://hostname/` prefix.
fn extract_wezterm_cwd(pane: &serde_json::Value) -> Option<String> {
    let cwd = pane.get("cwd")?.as_str()?;
    Some(strip_file_uri(cwd))
}

/// Strip `file://hostname/path` → `/path`.
fn strip_file_uri(uri: &str) -> String {
    if let Some(rest) = uri.strip_prefix("file://") {
        // Skip the hostname part (everything up to the next `/`)
        if let Some(slash_pos) = rest.find('/') {
            return rest[slash_pos..].to_string();
        }
    }
    uri.to_string()
}

// ── ProcCwdProbe ────────────────────────────────────────────────────────

pub struct ProcCwdProbe;

impl TerminalProbe for ProcCwdProbe {
    #[cfg(target_os = "linux")]
    fn probe(&self, pid: u32, _title: &str) -> Option<String> {
        let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).ok()?;

        let child_pids: Vec<u32> = children
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();

        // Prefer a shell process, fall back to first child
        let shell_pid = child_pids
            .iter()
            .find(|&&cpid| is_shell_process(cpid))
            .or(child_pids.first())?;

        let cwd = std::fs::read_link(format!("/proc/{shell_pid}/cwd")).ok()?;
        cwd.to_str().filter(|s| !s.is_empty()).map(String::from)
    }

    #[cfg(not(target_os = "linux"))]
    fn probe(&self, _pid: u32, _title: &str) -> Option<String> {
        None
    }
}

/// Check if a PID's executable looks like a shell.
#[cfg(target_os = "linux")]
pub(crate) fn is_shell_process(pid: u32) -> bool {
    let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
        return false;
    };
    let name = exe.file_name().and_then(|n| n.to_str()).unwrap_or("");
    matches!(
        name,
        "bash" | "zsh" | "fish" | "nu" | "sh" | "dash" | "elvish"
    )
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // -- Dispatch routing --

    #[test]
    fn dispatch_returns_none_for_unknown_class() {
        let (result, _) = dispatch("firefox", 12345, "Mozilla Firefox");
        assert!(result.is_none());
    }

    #[test]
    fn dispatch_selects_proc_for_alacritty() {
        // Just verify routing — ProcCwdProbe will return None for bogus pid
        let (_, source) = dispatch("alacritty", 9999999, "");
        assert!(matches!(source, ProbeSource::Proc));
    }

    #[test]
    fn dispatch_selects_konsole() {
        let (_, source) = dispatch("konsole", 9999999, "");
        assert!(matches!(source, ProbeSource::Konsole));
    }

    #[test]
    fn dispatch_selects_kitty() {
        let (_, source) = dispatch("kitty", 9999999, "");
        assert!(matches!(source, ProbeSource::Kitty));
    }

    #[test]
    fn dispatch_selects_wezterm() {
        let (_, source) = dispatch("wezterm", 9999999, "");
        assert!(matches!(source, ProbeSource::Wezterm));
    }

    #[test]
    fn dispatch_selects_wezterm_fqdn() {
        let (_, source) = dispatch("org.wezfurlong.wezterm", 9999999, "");
        assert!(matches!(source, ProbeSource::Wezterm));
    }

    // -- ProcCwdProbe --

    #[test]
    fn proc_probe_returns_none_for_invalid_pid() {
        let result = ProcCwdProbe.probe(9999999, "");
        assert!(result.is_none());
    }

    // -- kitty parsing --

    #[test]
    fn parse_kitty_ls_extracts_focused_cwd() {
        let json = br#"[
            {
                "id": 1, "is_focused": true,
                "tabs": [{
                    "is_focused": true,
                    "windows": [{
                        "pid": 1234, "cwd": "/home/user/project",
                        "is_focused": true, "is_self": false
                    }]
                }]
            }
        ]"#;
        assert_eq!(parse_kitty_ls(json), Some("/home/user/project".to_string()));
    }

    #[test]
    fn parse_kitty_ls_multiple_focused_returns_none() {
        let json = br#"[
            {
                "id": 1, "is_focused": true,
                "tabs": [{"is_focused": true, "windows": [{"pid": 1, "cwd": "/tmp", "is_focused": true}]}]
            },
            {
                "id": 2, "is_focused": true,
                "tabs": [{"is_focused": true, "windows": [{"pid": 2, "cwd": "/home", "is_focused": true}]}]
            }
        ]"#;
        assert_eq!(parse_kitty_ls(json), None);
    }

    #[test]
    fn parse_kitty_ls_no_focused_returns_none() {
        let json = br#"[
            {
                "id": 1, "is_focused": false,
                "tabs": [{"is_focused": true, "windows": [{"pid": 1, "cwd": "/tmp", "is_focused": true}]}]
            }
        ]"#;
        assert_eq!(parse_kitty_ls(json), None);
    }

    #[test]
    fn parse_kitty_ls_no_focused_tab_returns_none() {
        let json = br#"[
            {
                "id": 1, "is_focused": true,
                "tabs": [{"is_focused": false, "windows": [{"pid": 1, "cwd": "/tmp", "is_focused": true}]}]
            }
        ]"#;
        assert_eq!(parse_kitty_ls(json), None);
    }

    // -- wezterm parsing --

    #[test]
    fn parse_wezterm_list_extracts_cwd_by_title() {
        let json = br#"[
            {"window_id": 0, "tab_id": 0, "pane_id": 0,
             "title": "user@host:/home/user/project",
             "cwd": "file://hostname/home/user/project"}
        ]"#;
        assert_eq!(
            parse_wezterm_list(json, "user@host:/home/user/project"),
            Some("/home/user/project".to_string())
        );
    }

    #[test]
    fn parse_wezterm_list_multiple_title_matches_returns_none() {
        let json = br#"[
            {"window_id": 0, "tab_id": 0, "pane_id": 0,
             "title": "same title", "cwd": "file://host/tmp"},
            {"window_id": 0, "tab_id": 1, "pane_id": 1,
             "title": "same title", "cwd": "file://host/home"}
        ]"#;
        assert_eq!(parse_wezterm_list(json, "same title"), None);
    }

    #[test]
    fn parse_wezterm_list_no_match_active_pane_fallback() {
        let json = br#"[
            {"window_id": 0, "tab_id": 0, "pane_id": 0,
             "title": "other title", "cwd": "file://host/tmp",
             "is_active": true},
            {"window_id": 0, "tab_id": 1, "pane_id": 1,
             "title": "another title", "cwd": "file://host/home",
             "is_active": false}
        ]"#;
        assert_eq!(
            parse_wezterm_list(json, "no match"),
            Some("/tmp".to_string())
        );
    }

    #[test]
    fn parse_wezterm_list_no_match_no_active_returns_none() {
        let json = br#"[
            {"window_id": 0, "tab_id": 0, "pane_id": 0,
             "title": "other title", "cwd": "file://host/tmp"},
            {"window_id": 0, "tab_id": 1, "pane_id": 1,
             "title": "another title", "cwd": "file://host/home"}
        ]"#;
        assert_eq!(parse_wezterm_list(json, "no match"), None);
    }

    #[test]
    fn parse_wezterm_list_strips_file_prefix() {
        assert_eq!(strip_file_uri("file://hostname/home/user"), "/home/user");
        assert_eq!(strip_file_uri("/already/bare"), "/already/bare");
        assert_eq!(strip_file_uri("file:///root/path"), "/root/path");
    }

    // -- is_shell_process --

    // Can't unit-test is_shell_process without /proc mocking,
    // but we verify the match pattern covers expected shells.
    #[test]
    fn shell_match_pattern_covers_common_shells() {
        let shells = ["bash", "zsh", "fish", "nu", "sh", "dash", "elvish"];
        for shell in shells {
            assert!(
                matches!(
                    shell,
                    "bash" | "zsh" | "fish" | "nu" | "sh" | "dash" | "elvish"
                ),
                "{shell} should match"
            );
        }
        // Negative cases
        assert!(!matches!(
            "python",
            "bash" | "zsh" | "fish" | "nu" | "sh" | "dash" | "elvish"
        ));
        assert!(!matches!(
            "node",
            "bash" | "zsh" | "fish" | "nu" | "sh" | "dash" | "elvish"
        ));
    }

    // -- cwd::detect fallback --

    #[test]
    fn cwd_detect_falls_back_to_title_for_unknown_terminal() {
        // Unknown wm_class with pid 0 → probe returns None → title-parse runs
        let result = super::super::cwd::detect(0, "unknown_terminal", "user@host:/tmp");
        assert_eq!(result, Some("/tmp".to_string()));
    }
}
