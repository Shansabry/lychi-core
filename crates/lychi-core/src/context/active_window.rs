//! Active window detection — reads the currently focused window.
//!
//! X11: reads `_NET_ACTIVE_WINDOW` from root, fetches title/class/pid.
//! KWin Wayland: D-Bus script reading `workspace.activeWindow`.

use super::WindowContext;

/// Known terminal emulator WM classes (real terminals only, not IDEs).
const TERMINALS: &[&str] = &[
    "alacritty",
    "kitty",
    "wezterm",
    "foot",
    "gnome-terminal",
    "gnome-terminal-server",
    "org.gnome.terminal",
    "konsole",
    "xterm",
    "terminator",
    "tilix",
    "st",
    "urxvt",
    "xfce4-terminal",
    "mate-terminal",
    "lxterminal",
    "sakura",
    "guake",
    "yakuake",
    "ghostty",
    "rio",
    "contour",
    "blackbox",
    "ptyxis",
];

/// Detect the currently focused window.
pub fn detect() -> Option<WindowContext> {
    let result = if super::is_wayland() {
        detect_kwin()
    } else {
        detect_x11()
    };
    tracing::debug!(
        "active_window::detect: {:?}",
        result.as_ref().map(|w| format!(
            "{}(pid={},term={},ide={},title={})",
            w.wm_class, w.pid, w.is_terminal, w.is_ide, w.title
        ))
    );
    result
}

/// Check if a wm_class is a terminal emulator.
pub fn is_terminal_class(wm_class: &str) -> bool {
    let lower = wm_class.to_lowercase();
    TERMINALS.iter().any(|t| lower.contains(t))
}

/// Known IDE WM classes.
const IDES: &[&str] = &[
    "code",
    "code - oss",
    "vscodium",
    "cursor",
    "windsurf",
    "jetbrains-idea",
    "jetbrains-pycharm",
    "jetbrains-webstorm",
    "jetbrains-clion",
    "jetbrains-goland",
    "jetbrains-rustrover",
    "jetbrains-rider",
    "jetbrains-phpstorm",
    "jetbrains-datagrip",
    "zed",
];

/// Check if a wm_class is an IDE.
pub fn is_ide_class(wm_class: &str) -> bool {
    let lower = wm_class.to_lowercase();
    IDES.iter().any(|t| lower.contains(t))
}

// ── X11 ─────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn detect_x11() -> Option<WindowContext> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::*;
    use x11rb::rust_connection::RustConnection;

    let (conn, screen_num) = RustConnection::connect(None).ok()?;
    let root = conn.setup().roots[screen_num].root;

    // Intern atoms
    let net_active = conn
        .intern_atom(false, b"_NET_ACTIVE_WINDOW")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let net_wm_name = conn
        .intern_atom(false, b"_NET_WM_NAME")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let utf8_string = conn
        .intern_atom(false, b"UTF8_STRING")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let net_wm_pid = conn
        .intern_atom(false, b"_NET_WM_PID")
        .ok()?
        .reply()
        .ok()?
        .atom;

    // Read active window ID from root
    let active_reply = conn
        .get_property(false, root, net_active, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?;

    let wid = active_reply.value32()?.next()?;
    if wid == 0 {
        return None;
    }

    // Fetch title: _NET_WM_NAME (UTF8) → WM_NAME fallback
    let title = conn
        .get_property(false, wid, net_wm_name, utf8_string, 0, 256)
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| String::from_utf8(r.value).ok())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            conn.get_property::<Atom, Atom>(
                false,
                wid,
                AtomEnum::WM_NAME.into(),
                AtomEnum::STRING.into(),
                0,
                256,
            )
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| String::from_utf8_lossy(&r.value).into_owned())
            .filter(|s| !s.is_empty())
        })
        .unwrap_or_default();

    // Fetch WM_CLASS
    let wm_class = conn
        .get_property::<Atom, Atom>(
            false,
            wid,
            AtomEnum::WM_CLASS.into(),
            AtomEnum::STRING.into(),
            0,
            256,
        )
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| parse_wm_class(&r.value))
        .unwrap_or_default();

    // Fetch PID
    let pid = conn
        .get_property::<u32, Atom>(false, wid, net_wm_pid, AtomEnum::CARDINAL.into(), 0, 1)
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| r.value32().and_then(|mut i| i.next()))
        .unwrap_or(0);

    if pid == 0 || wm_class == "lychi" {
        return None;
    }

    Some(WindowContext {
        is_terminal: is_terminal_class(&wm_class),
        is_ide: is_ide_class(&wm_class),
        title,
        wm_class,
        pid,
    })
}

#[cfg(not(target_os = "linux"))]
fn detect_x11() -> Option<WindowContext> {
    None
}

/// Parse WM_CLASS: "instance\0class\0" → class (lowercase).
pub(super) fn parse_wm_class(data: &[u8]) -> String {
    let parts: Vec<&[u8]> = data.split(|&b| b == 0).filter(|p| !p.is_empty()).collect();
    if parts.len() >= 2 {
        String::from_utf8_lossy(parts[1]).to_lowercase()
    } else if let Some(first) = parts.first() {
        String::from_utf8_lossy(first).to_lowercase()
    } else {
        String::new()
    }
}

// ── KWin Wayland ────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn detect_kwin() -> Option<WindowContext> {
    use std::sync::mpsc;
    use std::time::Duration;

    use dbus::blocking::SyncConnection;
    use dbus::channel::MatchingReceiver;
    use dbus::message::MatchRule;

    let conn = SyncConnection::new_session().ok()?;
    let bus_name = conn.unique_name().to_string();

    // JS script: read workspace.activeWindow properties
    let script = format!(
        r#"
var w = workspace.activeWindow;
if (w && w.caption && w.pid > 0) {{
    var rc = w.resourceClass ? w.resourceClass.toString() : "";
    var cap = w.caption ? w.caption.toString() : "";
    var p = w.pid ? w.pid : 0;
    callDBus("{bus_name}", "/", "", "lychi_active_win", rc + "\t" + p + "\t" + cap);
}} else {{
    callDBus("{bus_name}", "/", "", "lychi_active_win", "");
}}
"#
    );

    let script_path = std::env::temp_dir().join("lychi_ctx_active.js");
    std::fs::write(&script_path, &script).ok()?;

    let plugin_name = format!(
        "lychi_ctx_active_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );

    let scripting = conn.with_proxy("org.kde.KWin", "/Scripting", Duration::from_secs(2));

    let (script_id,): (i32,) = scripting
        .method_call(
            "org.kde.kwin.Scripting",
            "loadScript",
            (script_path.to_str().unwrap_or_default(), &plugin_name),
        )
        .ok()?;

    if script_id < 0 {
        let _ = std::fs::remove_file(&script_path);
        return None;
    }

    let (tx, rx) = mpsc::channel::<String>();
    conn.start_receive(
        MatchRule::new_method_call(),
        Box::new(move |msg, _conn| {
            if let Some(member) = msg.member()
                && &*member == "lychi_active_win"
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

    let data = payload.filter(|s| !s.is_empty())?;
    let parts: Vec<&str> = data.splitn(3, '\t').collect();
    if parts.len() != 3 {
        return None;
    }

    let wm_class = parts[0].to_lowercase();
    let pid: u32 = parts[1].parse().unwrap_or(0);
    let title = parts[2].to_string();

    if pid == 0 || wm_class == "lychi" {
        return None;
    }

    Some(WindowContext {
        is_terminal: is_terminal_class(&wm_class),
        is_ide: is_ide_class(&wm_class),
        title,
        wm_class,
        pid,
    })
}

#[cfg(not(target_os = "linux"))]
fn detect_kwin() -> Option<WindowContext> {
    None
}
