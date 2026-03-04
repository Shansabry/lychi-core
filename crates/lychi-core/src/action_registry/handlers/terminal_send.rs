//! Terminal command routing — send commands to existing terminal windows.
//!
//! Protocol-first: Konsole (D-Bus), Kitty (remote control), WezTerm (CLI).
//! Unsupported terminals return `Err` — caller falls back to opening a new terminal.
//!
//! Includes a busy-terminal guard that checks whether the shell is running
//! a foreground process (via `/proc` stat parsing).

use std::process::{Command, Stdio};
use std::time::Duration;

// ── Send command ────────────────────────────────────────────────────────

/// Send a command string to an existing terminal window via its native protocol.
///
/// Returns `Ok(())` on success, `Err(reason)` if the terminal type is unsupported
/// or the protocol call failed. Caller should fall back to `launch_in_terminal()`.
pub fn send_command(wm_class: &str, pid: u32, command: &str) -> Result<(), String> {
    match wm_class.to_lowercase().as_str() {
        "konsole" => konsole_send(pid, command),
        "kitty" => kitty_send(command),
        "wezterm" | "org.wezfurlong.wezterm" => wezterm_send(command),
        _ => Err(format!("no send protocol for {wm_class}")),
    }
}

// ── Konsole — D-Bus sendText ────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn konsole_send(pid: u32, command: &str) -> Result<(), String> {
    use dbus::blocking::SyncConnection;

    let conn = SyncConnection::new_session().map_err(|e| format!("D-Bus session: {e}"))?;
    let service = format!("org.kde.konsole-{pid}");
    let timeout = Duration::from_millis(500);

    // Find a responding window and its current session
    for win_id in 1..=5 {
        let win_path = format!("/Windows/{win_id}");
        let win_proxy = conn.with_proxy(&service, &win_path, timeout);
        let session_id: i32 =
            match win_proxy.method_call("org.kde.konsole.Window", "currentSession", ()) {
                Ok((id,)) => id,
                Err(_) => continue,
            };

        let session_path = format!("/Sessions/{session_id}");
        let session_proxy = conn.with_proxy(&service, &session_path, timeout);

        // sendText appends text to terminal input; \n executes
        return session_proxy
            .method_call::<(), _, _, _>(
                "org.kde.konsole.Session",
                "sendText",
                (format!("{command}\n"),),
            )
            .map_err(|e| format!("konsole sendText: {e}"));
    }

    Err("no responding Konsole window".into())
}

#[cfg(not(target_os = "linux"))]
fn konsole_send(_pid: u32, _command: &str) -> Result<(), String> {
    Err("Konsole D-Bus not available on this platform".into())
}

// ── Kitty — remote control ──────────────────────────────────────────────

fn kitty_send(command: &str) -> Result<(), String> {
    let text = format!("{command}\n");
    let output = Command::new("kitty")
        .args(["@", "send-text", "--match", "state:focused", &text])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("kitty @: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("kitty @ send-text: {}", stderr.trim()))
    }
}

// ── WezTerm — CLI send-text ─────────────────────────────────────────────

fn wezterm_send(command: &str) -> Result<(), String> {
    let text = format!("{command}\n");
    let output = Command::new("wezterm")
        .args(["cli", "send-text", "--no-paste", &text])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("wezterm cli: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("wezterm cli send-text: {}", stderr.trim()))
    }
}

// ── Busy guard ──────────────────────────────────────────────────────────

/// Check if a terminal's shell has a foreground process running.
///
/// Reads `/proc/<shell_pid>/stat` and compares the shell's process group (pgrp)
/// with the terminal's foreground process group (tpgid). If they differ,
/// another process is in the foreground (e.g. `cargo build`, `vim`).
///
/// Returns `false` on any read error — assume not busy (fail open).
#[cfg(target_os = "linux")]
pub fn is_terminal_busy(terminal_pid: u32) -> bool {
    // Find shell child of terminal
    let children =
        match std::fs::read_to_string(format!("/proc/{terminal_pid}/task/{terminal_pid}/children"))
        {
            Ok(c) => c,
            Err(_) => return false,
        };

    let shell_pid: u32 = match children
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .find(|&pid| crate::context::terminal_probe::is_shell_process(pid))
    {
        Some(pid) => pid,
        None => return false,
    };

    // Read /proc/<shell>/stat
    let stat = match std::fs::read_to_string(format!("/proc/{shell_pid}/stat")) {
        Ok(s) => s,
        Err(_) => return false,
    };

    // Skip comm field (may contain spaces/parens) by finding last ')'
    let after_comm = match stat.rfind(')') {
        Some(pos) if pos + 2 < stat.len() => &stat[pos + 2..],
        _ => return false,
    };

    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    // fields[0]=state, [1]=ppid, [2]=pgrp, [3]=session, [4]=tty_nr, [5]=tpgid
    if fields.len() < 6 {
        return false;
    }

    let pgrp: u32 = fields[2].parse().unwrap_or(0);
    let tpgid: u32 = fields[5].parse().unwrap_or(0);

    // If foreground group differs from shell's group, something is running
    pgrp != 0 && tpgid != 0 && pgrp != tpgid
}

#[cfg(not(target_os = "linux"))]
pub fn is_terminal_busy(_terminal_pid: u32) -> bool {
    false
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_command_unsupported_terminal_returns_err() {
        let result = send_command("alacritty", 12345, "echo hello");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no send protocol"));
    }

    #[test]
    fn send_command_dispatches_known_terminals() {
        // These will fail (no actual terminal running) but should NOT return
        // "no send protocol" — they should attempt the real protocol.
        for wm_class in &["konsole", "kitty", "wezterm", "org.wezfurlong.wezterm"] {
            let result = send_command(wm_class, 9999999, "echo test");
            assert!(result.is_err(), "{wm_class} should fail (no real terminal)");
            assert!(
                !result.as_ref().unwrap_err().contains("no send protocol"),
                "{wm_class} should dispatch to real protocol, got: {}",
                result.unwrap_err()
            );
        }
    }

    #[test]
    fn busy_guard_returns_false_for_invalid_pid() {
        assert!(!is_terminal_busy(9999999));
    }
}
