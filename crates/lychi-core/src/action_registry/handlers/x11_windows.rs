//! Native X11 window operations via EWMH protocol.
//!
//! Uses x11rb (pure Rust) to enumerate, focus, and close windows.
//! No external tools required (no wmctrl, xprop, xdotool).

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

/// A running window discovered via EWMH.
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub window_id: u32,
    pub title: String,
    pub wm_class: String,
    pub pid: u32,
}

/// Interned EWMH atoms needed for window enumeration.
struct Atoms {
    net_client_list: Atom,
    net_wm_name: Atom,
    utf8_string: Atom,
    net_wm_pid: Atom,
}

fn intern_atoms(conn: &RustConnection) -> Option<Atoms> {
    let net_client_list = conn.intern_atom(false, b"_NET_CLIENT_LIST").ok()?;
    let net_wm_name = conn.intern_atom(false, b"_NET_WM_NAME").ok()?;
    let utf8_string = conn.intern_atom(false, b"UTF8_STRING").ok()?;
    let net_wm_pid = conn.intern_atom(false, b"_NET_WM_PID").ok()?;

    Some(Atoms {
        net_client_list: net_client_list.reply().ok()?.atom,
        net_wm_name: net_wm_name.reply().ok()?.atom,
        utf8_string: utf8_string.reply().ok()?.atom,
        net_wm_pid: net_wm_pid.reply().ok()?.atom,
    })
}

/// Enumerate all windows via _NET_CLIENT_LIST EWMH property.
/// Returns empty vec if X11 is unavailable (e.g. pure Wayland).
pub fn enumerate_windows() -> Vec<WindowInfo> {
    let (conn, screen_num) = match RustConnection::connect(None) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let root = conn.setup().roots[screen_num].root;

    let atoms = match intern_atoms(&conn) {
        Some(a) => a,
        None => return Vec::new(),
    };

    // Get window list from root
    let client_list = match conn
        .get_property(
            false,
            root,
            atoms.net_client_list,
            AtomEnum::WINDOW,
            0,
            4096,
        )
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

    // Pipeline: send all property requests at once for maximum throughput
    let mut requests = Vec::with_capacity(window_ids.len());
    for &wid in &window_ids {
        let name = conn.get_property(false, wid, atoms.net_wm_name, atoms.utf8_string, 0, 256);
        let name_fallback = conn.get_property::<Atom, Atom>(
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
        let pid = conn.get_property::<u32, Atom>(
            false,
            wid,
            atoms.net_wm_pid,
            AtomEnum::CARDINAL.into(),
            0,
            1,
        );
        requests.push((wid, name, name_fallback, class, pid));
    }

    // Collect replies
    let mut windows = Vec::new();
    for (wid, name_cookie, fallback_cookie, class_cookie, pid_cookie) in requests {
        let title = name_cookie
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| String::from_utf8(r.value).ok())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                fallback_cookie
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .map(|r| String::from_utf8_lossy(&r.value).into_owned())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_default();

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

        if title.is_empty() || pid == 0 {
            continue;
        }
        if wm_class == "lychi" {
            continue;
        }

        windows.push(WindowInfo {
            window_id: wid,
            title,
            wm_class,
            pid,
        });
    }

    windows
}

/// Raise and activate a window via _NET_ACTIVE_WINDOW client message.
pub fn focus_window(window_id: u32) -> Result<(), String> {
    let (conn, screen_num) = RustConnection::connect(None).map_err(|e| format!("X11: {e}"))?;
    let root = conn.setup().roots[screen_num].root;
    let atom = conn
        .intern_atom(false, b"_NET_ACTIVE_WINDOW")
        .map_err(|e| format!("intern: {e}"))?
        .reply()
        .map_err(|e| format!("intern reply: {e}"))?
        .atom;

    // source=2 (pager/taskbar) — WMs honor this for focus requests
    let event = ClientMessageEvent::new(32, window_id, atom, [2, 0, 0, 0, 0]);

    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    )
    .map_err(|e| format!("send_event: {e}"))?;

    conn.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

/// Gracefully close a window via _NET_CLOSE_WINDOW client message.
pub fn close_window(window_id: u32) -> Result<(), String> {
    let (conn, screen_num) = RustConnection::connect(None).map_err(|e| format!("X11: {e}"))?;
    let root = conn.setup().roots[screen_num].root;
    let atom = conn
        .intern_atom(false, b"_NET_CLOSE_WINDOW")
        .map_err(|e| format!("intern: {e}"))?
        .reply()
        .map_err(|e| format!("intern reply: {e}"))?
        .atom;

    // timestamp=0, source=2
    let event = ClientMessageEvent::new(32, window_id, atom, [0, 2, 0, 0, 0]);

    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    )
    .map_err(|e| format!("send_event: {e}"))?;

    conn.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

/// Parse WM_CLASS property: "instance\0class\0" → class (lowercase).
fn parse_wm_class(data: &[u8]) -> String {
    let parts: Vec<&[u8]> = data.split(|&b| b == 0).filter(|p| !p.is_empty()).collect();
    if parts.len() >= 2 {
        String::from_utf8_lossy(parts[1]).to_lowercase()
    } else if let Some(first) = parts.first() {
        String::from_utf8_lossy(first).to_lowercase()
    } else {
        String::new()
    }
}
