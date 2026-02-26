//! Window stack detection — finds the most recently focused terminal.
//!
//! Reads the stacking order (most-recently-focused last) and finds the
//! nearest terminal window, even when an IDE or browser has focus.
//!
//! KWin Wayland: `workspace.stackingOrder` via D-Bus scripting.
//! X11: `_NET_CLIENT_LIST_STACKING` EWMH property.

use super::WindowContext;
use super::active_window::{is_terminal_class, parse_wm_class};

/// Find the most recently focused terminal from the window stack.
///
/// Returns `None` if no terminal is in the stack, or if the focused
/// window is already a terminal (caller should use the primary context).
pub fn find_recent_terminal(focused: Option<&WindowContext>) -> Option<WindowContext> {
    // If the focused window is already a terminal, no need to search the stack
    if let Some(w) = focused {
        if is_terminal_class(&w.wm_class) {
            tracing::debug!(
                "window_stack: focused is already a terminal ({}), skipping",
                w.wm_class
            );
            return None;
        }
        tracing::debug!(
            "window_stack: focused is '{}', scanning stack for terminal",
            w.wm_class
        );
    } else {
        tracing::debug!("window_stack: no focused window, scanning stack for terminal");
    }

    let wayland = super::is_wayland();
    tracing::debug!(
        "window_stack: session_type={}",
        if wayland { "wayland" } else { "x11" }
    );

    let stack = if wayland {
        detect_stack_kwin()
    } else {
        detect_stack_x11()
    };

    tracing::debug!(
        "window_stack: got {} windows in stack: [{}]",
        stack.len(),
        stack
            .iter()
            .map(|w| format!("{}(pid={},term={})", w.wm_class, w.pid, w.is_terminal))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Stack is ordered most-recent-first (we reverse the raw bottom→top order).
    // Find the first terminal that isn't Lychi.
    let result = stack.into_iter().find(|w| is_terminal_class(&w.wm_class));

    match &result {
        Some(t) => tracing::debug!(
            "window_stack: found terminal '{}' pid={} title='{}'",
            t.wm_class,
            t.pid,
            t.title
        ),
        None => tracing::debug!("window_stack: no terminal found in stack"),
    }

    result
}

// ── KWin Wayland ────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn detect_stack_kwin() -> Vec<WindowContext> {
    use std::sync::mpsc;
    use std::time::Duration;

    use dbus::blocking::SyncConnection;
    use dbus::channel::MatchingReceiver;
    use dbus::message::MatchRule;

    let conn = match SyncConnection::new_session() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let bus_name = conn.unique_name().to_string();

    // Use workspace.stackingOrder — returns windows ordered bottom→top by Z-order.
    // The topmost (last) window is the most recently focused.
    let script = format!(
        r#"
var wins = workspace.stackingOrder;
var result = [];
for (var i = 0; i < wins.length; i++) {{
    var w = wins[i];
    var rc = w.resourceClass ? w.resourceClass.toString() : "";
    var cap = w.caption ? w.caption.toString() : "";
    var p = w.pid ? w.pid : 0;
    if (cap === "" || p === 0) continue;
    if (rc.toLowerCase() === "lychi") continue;
    if (w.minimized) continue;
    result.push(rc + "\t" + p + "\t" + cap);
}}
callDBus("{bus_name}", "/", "", "lychi_stack", result.join("\n"));
"#
    );

    let script_path = std::env::temp_dir().join("lychi_ctx_stack.js");
    if std::fs::write(&script_path, &script).is_err() {
        return Vec::new();
    }

    let plugin_name = format!(
        "lychi_stack_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );

    let scripting = conn.with_proxy("org.kde.KWin", "/Scripting", Duration::from_secs(2));

    let script_id: i32 = match scripting.method_call(
        "org.kde.kwin.Scripting",
        "loadScript",
        (script_path.to_str().unwrap_or_default(), &plugin_name),
    ) {
        Ok((id,)) => id,
        Err(_) => {
            let _ = std::fs::remove_file(&script_path);
            return Vec::new();
        }
    };

    if script_id < 0 {
        let _ = std::fs::remove_file(&script_path);
        return Vec::new();
    }

    let (tx, rx) = mpsc::channel::<String>();
    conn.start_receive(
        MatchRule::new_method_call(),
        Box::new(move |msg, _conn| {
            if let Some(member) = msg.member()
                && &*member == "lychi_stack"
                && let Ok(payload) = msg.read1::<String>()
            {
                let _ = tx.send(payload);
            }
            true
        }),
    );

    let script_path_dbus = format!("/Scripting/Script{script_id}");
    let script_proxy = conn.with_proxy("org.kde.KWin", &script_path_dbus, Duration::from_secs(2));

    let _ = script_proxy.method_call::<(), _, _, _>("org.kde.kwin.Script", "run", ());

    // Wait for callback (2s timeout)
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut payload = None;
    while std::time::Instant::now() < deadline {
        let remaining = deadline - std::time::Instant::now();
        let _ = conn.process(remaining.min(Duration::from_millis(50)));
        if let Ok(data) = rx.try_recv() {
            payload = Some(data);
            break;
        }
    }

    // Cleanup
    let _ = script_proxy.method_call::<(), _, _, _>("org.kde.kwin.Script", "stop", ());
    let _ = scripting.method_call::<(), _, _, _>(
        "org.kde.kwin.Scripting",
        "unloadScript",
        (&plugin_name,),
    );
    let _ = std::fs::remove_file(&script_path);

    let data = match payload {
        Some(d) if !d.is_empty() => d,
        _ => return Vec::new(),
    };

    // Parse tab-separated lines, reverse to get most-recent-first
    let mut windows: Vec<WindowContext> = data
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(3, '\t').collect();
            if parts.len() != 3 {
                return None;
            }
            let wm_class = parts[0].to_lowercase();
            let pid: u32 = parts[1].parse().ok()?;
            let title = parts[2].to_string();
            if pid == 0 || title.is_empty() {
                return None;
            }
            Some(WindowContext {
                wm_class,
                pid,
                title,
                is_terminal: is_terminal_class(parts[0]),
                is_ide: false, // stack scan only cares about terminals
            })
        })
        .collect();

    windows.reverse(); // bottom→top becomes most-recent-first
    windows
}

#[cfg(not(target_os = "linux"))]
fn detect_stack_kwin() -> Vec<WindowContext> {
    Vec::new()
}

// ── X11 ─────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn detect_stack_x11() -> Vec<WindowContext> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::*;
    use x11rb::rust_connection::RustConnection;

    let (conn, screen_num) = match RustConnection::connect(None) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let root = conn.setup().roots[screen_num].root;

    // Intern _NET_CLIENT_LIST_STACKING (focus-ordered, bottom→top)
    let stacking_atom = match conn.intern_atom(false, b"_NET_CLIENT_LIST_STACKING") {
        Ok(c) => match c.reply() {
            Ok(r) => r.atom,
            Err(_) => return Vec::new(),
        },
        Err(_) => return Vec::new(),
    };
    let net_wm_name = conn
        .intern_atom(false, b"_NET_WM_NAME")
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.atom);
    let utf8_string = conn
        .intern_atom(false, b"UTF8_STRING")
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.atom);
    let net_wm_pid = conn
        .intern_atom(false, b"_NET_WM_PID")
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.atom);

    let (Some(net_wm_name), Some(utf8_string), Some(net_wm_pid)) =
        (net_wm_name, utf8_string, net_wm_pid)
    else {
        return Vec::new();
    };

    // Read stacking-ordered window list
    let client_list = match conn
        .get_property(false, root, stacking_atom, AtomEnum::WINDOW, 0, 4096)
        .ok()
        .and_then(|c| c.reply().ok())
    {
        Some(reply) if reply.format == 32 => reply,
        _ => return Vec::new(),
    };

    let window_ids: Vec<u32> = client_list
        .value32()
        .map(|iter| iter.collect())
        .unwrap_or_default();

    // Pipeline all property requests
    let mut requests = Vec::with_capacity(window_ids.len());
    for &wid in &window_ids {
        let name = conn.get_property(false, wid, net_wm_name, utf8_string, 0, 256);
        let name_fb = conn.get_property::<Atom, Atom>(
            false,
            wid,
            AtomEnum::WM_NAME.into(),
            AtomEnum::STRING.into(),
            0,
            256,
        );
        let class = conn.get_property::<Atom, Atom>(
            false,
            wid,
            AtomEnum::WM_CLASS.into(),
            AtomEnum::STRING.into(),
            0,
            256,
        );
        let pid =
            conn.get_property::<u32, Atom>(false, wid, net_wm_pid, AtomEnum::CARDINAL.into(), 0, 1);
        requests.push((name, name_fb, class, pid));
    }

    let mut windows: Vec<WindowContext> = requests
        .into_iter()
        .filter_map(|(name_cookie, fb_cookie, class_cookie, pid_cookie)| {
            let title = name_cookie
                .ok()
                .and_then(|c| c.reply().ok())
                .and_then(|r| String::from_utf8(r.value).ok())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    fb_cookie
                        .ok()
                        .and_then(|c| c.reply().ok())
                        .map(|r| String::from_utf8_lossy(&r.value).into_owned())
                        .filter(|s| !s.is_empty())
                })?;

            let wm_class = class_cookie
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| parse_wm_class(&r.value))
                .unwrap_or_default();

            let pid = pid_cookie
                .ok()
                .and_then(|c| c.reply().ok())
                .and_then(|r| r.value32().and_then(|mut i| i.next()))
                .unwrap_or(0);

            if title.is_empty() || pid == 0 || wm_class == "lychi" {
                return None;
            }

            Some(WindowContext {
                is_terminal: is_terminal_class(&wm_class),
                is_ide: false, // stack scan only cares about terminals
                title,
                wm_class,
                pid,
            })
        })
        .collect();

    windows.reverse(); // bottom→top becomes most-recent-first
    windows
}

#[cfg(not(target_os = "linux"))]
fn detect_stack_x11() -> Vec<WindowContext> {
    Vec::new()
}
