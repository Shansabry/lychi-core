//! Window stack detection — finds the most recently focused terminal.
//!
//! Reads the stacking order (most-recently-focused last) and finds the
//! nearest terminal window, even when an IDE or browser has focus.
//!
//! KWin Wayland: `workspace.stackingOrder` via D-Bus scripting.
//! X11: `_NET_CLIENT_LIST_STACKING` EWMH property.
//!
//! The focus ring (`FOCUS_RING`) is maintained by the KWin watcher task and
//! reflects true last-focus order. `find_recent_terminal()` checks it first
//! before falling back to the Z-order stack scan.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::active_window::{is_terminal_class, parse_wm_class};
use super::{TerminalSource, WindowContext};

// ── Focus ring ───────────────────────────────────────────────────────────

#[derive(Clone)]
struct FocusEntry {
    window: WindowContext,
    source: TerminalSource,
    focused_at: Instant,
    /// CWD resolved via terminal probes at push time. `None` if probe failed.
    cwd: Option<String>,
}

static FOCUS_RING: Mutex<VecDeque<FocusEntry>> = Mutex::new(VecDeque::new());
const RING_CAPACITY: usize = 10;

fn push_focus_entry_inner(window: WindowContext, source: TerminalSource) {
    // Resolve CWD before locking — avoids holding ring mutex during I/O
    let cwd = super::cwd::detect(window.pid, &window.wm_class, &window.title);

    let Ok(mut ring) = FOCUS_RING.lock() else {
        return;
    };
    ring.retain(|e| !same_window(&e.window, &window));
    ring.push_front(FocusEntry {
        window,
        source,
        focused_at: Instant::now(),
        cwd,
    });
    ring.truncate(RING_CAPACITY);
}

/// Push a terminal focus event from the background watcher into the ring.
///
/// Deduplicates by `window_id` when available, otherwise by `pid + wm_class`.
/// Most-recently-focused is always at the front.
pub fn push_focus_entry(window: WindowContext) {
    push_focus_entry_inner(window, TerminalSource::FocusRingWatcher);
}

/// Push a terminal focus event from a pre-summon window snapshot.
///
/// Used to seed the ring on the first summon when the user was already in a
/// terminal — avoids the "ring empty until post-start terminal focus" cold start.
pub fn push_focus_entry_pre_summon(window: WindowContext) {
    push_focus_entry_inner(window, TerminalSource::FocusRingPreSummon);
}

/// Two windows are the same if they share a `window_id`, or (fallback) the
/// same `pid + wm_class` when no window ID is available.
fn same_window(a: &WindowContext, b: &WindowContext) -> bool {
    match (&a.window_id, &b.window_id) {
        (Some(ia), Some(ib)) => ia == ib,
        _ => a.pid == b.pid && a.wm_class == b.wm_class,
    }
}

/// Pre-populate the focus ring by scanning the window stack once at startup.
///
/// This avoids the D-Bus stack scan on the very first summon. Call from
/// `spawn_blocking` during app setup. Safe to call on X11 too.
pub fn warmup() {
    let t0 = Instant::now();
    let stack = match super::compositor() {
        super::Compositor::KdeWayland => detect_stack_kwin(),
        super::Compositor::X11 => detect_stack_x11(),
        super::Compositor::OtherWayland => detect_stack_wlr(),
        // No stack backend on GNOME Wayland (Mutter offers no protocol)
        _ => Vec::new(),
    };

    let mut seeded = 0u32;
    for win in stack {
        if is_terminal_class(&win.wm_class) {
            push_focus_entry_inner(win, TerminalSource::FocusRingPreSummon);
            seeded += 1;
        }
    }
    tracing::info!(
        "[window_stack] warmup done: {}ms (seeded {} terminals)",
        t0.elapsed().as_millis(),
        seeded
    );
}

// ── Public API ───────────────────────────────────────────────────────────

/// Find the most recently focused terminal from the focus ring or window stack.
///
/// Returns `(terminal, source)`:
/// - `terminal`: the terminal window, or `None` if the focused window is already
///   a terminal or no background terminal is found.
/// - `source`: how the terminal was found (`FocusRing`, `Stacking`, or `None`).
pub fn find_recent_terminal(
    focused: Option<&WindowContext>,
) -> (Option<WindowContext>, TerminalSource) {
    // If the focused window is already a terminal, no need to search
    if let Some(w) = focused {
        if is_terminal_class(&w.wm_class) {
            tracing::debug!(
                "window_stack: focused is already a terminal ({}), skipping",
                w.wm_class
            );
            return (None, TerminalSource::None);
        }
        tracing::debug!(
            "window_stack: focused is '{}', scanning for background terminal",
            w.wm_class
        );
    } else {
        tracing::debug!("window_stack: no focused window, scanning for terminal");
    }

    // Check focus ring first (true last-focus order)
    if let Ok(ring) = FOCUS_RING.lock()
        && !ring.is_empty()
    {
        for entry in ring.iter() {
            // Exclude the currently focused window by stable ID
            if let Some(f) = focused
                && same_window(&entry.window, f)
            {
                continue;
            }
            tracing::debug!(
                "window_stack: focus ring hit ({}) — '{}' pid={} cwd={:?}",
                entry.source,
                entry.window.wm_class,
                entry.window.pid,
                entry.cwd.as_deref(),
            );
            return (Some(entry.window.clone()), entry.source);
        }
    }

    // Fall back to Z-order stack scan
    let compositor = super::compositor();
    tracing::debug!(
        "window_stack: focus ring empty/exhausted, falling back to stack scan ({compositor:?})"
    );

    let stack = match compositor {
        super::Compositor::KdeWayland => detect_stack_kwin(),
        super::Compositor::X11 => detect_stack_x11(),
        super::Compositor::OtherWayland => detect_stack_wlr(),
        // No stack backend on GNOME Wayland (Mutter offers no protocol)
        _ => Vec::new(),
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

    // Stack is ordered most-recent-first. Find the first terminal.
    let result = stack.into_iter().find(|w| is_terminal_class(&w.wm_class));

    match &result {
        Some(t) => {
            tracing::debug!(
                "window_stack: stacking fallback found terminal '{}' pid={}",
                t.wm_class,
                t.pid
            );
            (Some(t.clone()), TerminalSource::Stacking)
        }
        None => {
            tracing::debug!("window_stack: no terminal found in stack");
            (None, TerminalSource::None)
        }
    }
}

/// Find the most recently focused terminal whose CWD is within `project_root`.
///
/// Returns `None` if no matching terminal is found — does NOT fall back to
/// any-terminal (caller should use `find_recent_terminal()` for that).
///
/// Not wired into `gather()` yet — exists as API for future Phase 3 consumers.
pub fn find_recent_terminal_for_project(
    project_root: &str,
    focused: Option<&WindowContext>,
) -> Option<(WindowContext, TerminalSource)> {
    let ring = FOCUS_RING.lock().ok()?;
    let pr = project_root.trim_end_matches('/');
    for entry in ring.iter() {
        // Skip focused window
        if let Some(f) = focused
            && same_window(&entry.window, f)
        {
            continue;
        }
        // Skip stale entries (>15min)
        if entry.focused_at.elapsed() > RING_STALE_TTL {
            continue;
        }
        // Match: CWD equals or is under project_root (normalized)
        if let Some(ref cwd) = entry.cwd {
            let c = cwd.trim_end_matches('/');
            if c == pr || c.starts_with(&format!("{pr}/")) {
                return Some((entry.window.clone(), entry.source));
            }
        }
    }
    None
}

const RING_STALE_TTL: Duration = Duration::from_secs(900); // 15 minutes

/// Return ring entries for debug display (most recent first).
pub fn ring_debug_entries() -> Vec<(String, u32, Option<String>, u64)> {
    let Ok(ring) = FOCUS_RING.lock() else {
        return Vec::new();
    };
    ring.iter()
        .map(|e| {
            (
                e.window.wm_class.clone(),
                e.window.pid,
                e.cwd.clone(),
                e.focused_at.elapsed().as_secs(),
            )
        })
        .collect()
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
    // w.internalId provides a stable per-window UUID for deduplication.
    // Delimiter is \x1F (ASCII Unit Separator) — cannot appear in window titles.
    let script = format!(
        r#"
var SEP = "\x1F";
var wins = workspace.stackingOrder;
var result = [];
for (var i = 0; i < wins.length; i++) {{
    var w = wins[i];
    var rc = w.resourceClass ? w.resourceClass.toString() : "";
    var cap = w.caption ? w.caption.toString() : "";
    var p = w.pid ? w.pid : 0;
    var id = w.internalId ? w.internalId.toString() : "";
    if (cap === "" || p === 0) continue;
    if (rc.toLowerCase() === "lychi") continue;
    if (w.minimized) continue;
    result.push(rc + SEP + p + SEP + cap + SEP + id);
}}
callDBus("{bus_name}", "/", "", "lychi_stack", result.join("\n"));
"#
    );

    // Unique per-call path in XDG_RUNTIME_DIR (PLAT-4) — no fixed-name race.
    let Some(script_path) = super::kwin_script::write_temp_script(&script) else {
        return Vec::new();
    };

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

    // Parse lines: rc \x1F pid \x1F cap \x1F id (id may be absent on older Plasma)
    // \x1F (Unit Separator) cannot appear in window titles, unlike \t.
    let mut windows: Vec<WindowContext> = data
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(4, '\x1F').collect();
            if parts.len() < 3 {
                return None;
            }
            let wm_class = parts[0].to_lowercase();
            let pid: u32 = parts[1].parse().ok()?;
            let title = parts[2].to_string();
            let window_id = parts
                .get(3)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            if pid == 0 || title.is_empty() {
                return None;
            }
            Some(WindowContext {
                wm_class,
                pid,
                title,
                is_terminal: is_terminal_class(parts[0]),
                is_ide: false, // stack scan only cares about terminals
                window_id,
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

// ── wlroots (Sway / Hyprland / niri / wlroots family) ────────────────────

/// Build the window stack from the wlr-foreign-toplevel protocol.
///
/// The protocol exposes **no Z-order**, so we can't reproduce a true stacking
/// order. What it does give is the `activated` (focused) flag — so we surface
/// the activated window first and the rest after. It also exposes **no pid**;
/// unlike the X11/KWin scans (which drop pid==0 as junk) we keep pid==0 here
/// because that's every wlr window, and the nearest-terminal logic keys on
/// wm_class, not pid.
fn detect_stack_wlr() -> Vec<WindowContext> {
    let mut toplevels: Vec<_> = super::wlr_toplevel::list_toplevels()
        .into_iter()
        .filter(|w| {
            // Drop our own window and anything nameless.
            let app = w.app_id.to_lowercase();
            !app.is_empty() && app != "lychi"
        })
        .collect();

    // Best-effort recency: the `activated` (focused) window first, the rest
    // after. Stable so relative order of the non-activated tail is preserved.
    toplevels.sort_by_key(|w| std::cmp::Reverse(w.activated));

    toplevels
        .into_iter()
        .map(|w| {
            let wm_class = w.app_id.to_lowercase();
            WindowContext {
                is_terminal: is_terminal_class(&wm_class),
                is_ide: false, // stack scan only cares about terminals
                title: w.title,
                wm_class,
                pid: 0,          // foreign-toplevel exposes no pid
                window_id: None, // no stable cross-connection id
            }
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn detect_stack_wlr() -> Vec<WindowContext> {
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
        requests.push((wid, name, name_fb, class, pid));
    }

    let mut windows: Vec<WindowContext> = requests
        .into_iter()
        .filter_map(|(wid, name_cookie, fb_cookie, class_cookie, pid_cookie)| {
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
                window_id: Some(format!("{:#010x}", wid)),
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
