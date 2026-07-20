//! KWin D-Bus scripting for Wayland window enumeration.
//!
//! Uses the kdotool pattern: load a temporary JS script into KWin via D-Bus,
//! the script calls `workspace.windowList()` (which sees ALL windows including
//! native Wayland), serializes the data as JSON, and sends it back via
//! `callDBus()` to our listening D-Bus connection.
//!
//! This is the only reliable way to enumerate Wayland-native windows on KDE
//! Plasma 6 — the X11 EWMH `_NET_CLIENT_LIST` only sees XWayland windows.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use dbus::blocking::SyncConnection;
use dbus::channel::MatchingReceiver;
use dbus::message::MatchRule;

static CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A running window discovered via KWin scripting.
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub caption: String,
    pub resource_class: String,
    pub pid: u32,
    /// KWin internalId (UUID) for per-window targeting.
    pub internal_id: Option<String>,
    /// Virtual desktop number (1-indexed), None if on all desktops.
    pub desktop: Option<u32>,
}

/// Enumerate all windows via KWin D-Bus scripting.
/// Returns empty vec if KWin is not running or D-Bus fails.
pub fn enumerate_windows() -> Vec<WindowInfo> {
    match enumerate_windows_inner() {
        Ok(windows) => {
            tracing::info!("kwin_windows: enumerated {} windows", windows.len());
            windows
        }
        Err(e) => {
            tracing::warn!("kwin_windows enumerate failed: {e}");
            Vec::new()
        }
    }
}

/// Focus a window by resource class via KWin scripting.
pub fn focus_window(resource_class: &str) -> Result<(), String> {
    let script = format!(
        r#"
var wins = workspace.windowList();
for (var i = 0; i < wins.length; i++) {{
    if (wins[i].resourceClass.toString().toLowerCase() === "{}") {{
        workspace.activeWindow = wins[i];
        break;
    }}
}}
"#,
        resource_class.to_lowercase().replace('"', r#"\""#)
    );
    run_kwin_script(&script)
}

/// Focus a specific window by its KWin internalId (UUID).
///
/// Unlike `focus_window()` which matches the first window by resource class,
/// this targets a specific window — important when multiple instances exist
/// (e.g. two Konsole windows).
pub fn focus_window_by_id(window_id: &str) -> Result<(), String> {
    let script = format!(
        r#"
var wins = workspace.windowList();
for (var i = 0; i < wins.length; i++) {{
    if (wins[i].internalId && wins[i].internalId.toString() === "{}") {{
        workspace.activeWindow = wins[i];
        break;
    }}
}}
"#,
        window_id.replace('"', r#"\""#)
    );
    run_kwin_script(&script)
}

/// Close a specific window by its KWin internalId (UUID).
pub fn close_window_by_id(window_id: &str) -> Result<(), String> {
    let script = format!(
        r#"
var wins = workspace.windowList();
for (var i = 0; i < wins.length; i++) {{
    if (wins[i].internalId && wins[i].internalId.toString() === "{}") {{
        wins[i].closeWindow();
        break;
    }}
}}
"#,
        window_id.replace('"', r#"\""#)
    );
    run_kwin_script(&script)
}

/// Close a window by resource class via KWin scripting.
pub fn close_window(resource_class: &str) -> Result<(), String> {
    let script = format!(
        r#"
var wins = workspace.windowList();
for (var i = 0; i < wins.length; i++) {{
    if (wins[i].resourceClass.toString().toLowerCase() === "{}") {{
        wins[i].closeWindow();
        break;
    }}
}}
"#,
        resource_class.to_lowercase().replace('"', r#"\""#)
    );
    run_kwin_script(&script)
}

fn enumerate_windows_inner() -> Result<Vec<WindowInfo>, String> {
    let conn = SyncConnection::new_session().map_err(|e| format!("D-Bus session: {e}"))?;
    let bus_name = conn.unique_name().to_string();

    // JS script that enumerates windows and sends data back via callDBus
    // Fields: resourceClass \t pid \t internalId \t desktop \t caption
    let script = format!(
        r#"
var wins = workspace.windowList();
var result = [];
for (var i = 0; i < wins.length; i++) {{
    var w = wins[i];
    var rc = w.resourceClass ? w.resourceClass.toString() : "";
    var cap = w.caption ? w.caption.toString() : "";
    var p = w.pid ? w.pid : 0;
    if (cap === "" || p === 0) continue;
    if (rc.toLowerCase() === "lychi") continue;
    var iid = w.internalId ? w.internalId.toString() : "";
    var desktops = w.desktops;
    var desk = (desktops && desktops.length === 1) ? desktops[0].x11DesktopNumber : 0;
    result.push(rc + "\t" + p + "\t" + iid + "\t" + desk + "\t" + cap);
}}
callDBus("{bus_name}", "/", "", "kwin_result", result.join("\n"));
"#
    );

    // Write script to temp file
    let script_path = std::env::temp_dir().join("lychi_kwin_enum.js");
    std::fs::write(&script_path, &script).map_err(|e| format!("write script: {e}"))?;

    let call_id = CALL_COUNTER.fetch_add(1, Ordering::Relaxed);
    let plugin_name = format!("lychi_enum_{}_{}", std::process::id(), call_id);

    // Load script via KWin Scripting D-Bus
    let scripting = conn.with_proxy("org.kde.KWin", "/Scripting", Duration::from_secs(2));

    let (script_id,): (i32,) = scripting
        .method_call(
            "org.kde.kwin.Scripting",
            "loadScript",
            (script_path.to_str().unwrap_or_default(), &plugin_name),
        )
        .map_err(|e| format!("loadScript: {e}"))?;

    if script_id < 0 {
        return Err(format!("loadScript returned {script_id}"));
    }

    // Set up receiver for the callDBus callback
    let (tx, rx) = mpsc::channel::<String>();

    conn.start_receive(
        MatchRule::new_method_call(),
        Box::new(move |msg, _conn| {
            if let Some(member) = msg.member()
                && &*member == "kwin_result"
                && let Ok(payload) = msg.read1::<String>()
            {
                let _ = tx.send(payload);
            }
            true
        }),
    );

    // Run the script
    let script_path_dbus = format!("/Scripting/Script{script_id}");
    let script_proxy = conn.with_proxy("org.kde.KWin", &script_path_dbus, Duration::from_secs(2));

    script_proxy
        .method_call::<(), _, _, _>("org.kde.kwin.Script", "run", ())
        .map_err(|e| format!("run: {e}"))?;

    // Process D-Bus messages until we get our result (timeout 3s)
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut payload = None;
    while std::time::Instant::now() < deadline {
        let remaining = deadline - std::time::Instant::now();
        let _ = conn.process(remaining.min(Duration::from_millis(100)));
        if let Ok(data) = rx.try_recv() {
            payload = Some(data);
            break;
        }
    }

    // Cleanup: stop + unload
    let _ = script_proxy.method_call::<(), _, _, _>("org.kde.kwin.Script", "stop", ());
    let _ = scripting.method_call::<(), _, _, _>(
        "org.kde.kwin.Scripting",
        "unloadScript",
        (&plugin_name,),
    );
    let _ = std::fs::remove_file(&script_path);

    // Parse the tab-separated result: resourceClass \t pid \t internalId \t desktop \t caption
    let data = payload.ok_or("Timeout waiting for KWin script response")?;

    let mut windows = Vec::new();
    for line in data.lines() {
        let parts: Vec<&str> = line.splitn(5, '\t').collect();
        if parts.len() == 5 {
            let resource_class = parts[0].to_lowercase();
            let pid: u32 = parts[1].parse().unwrap_or(0);
            let internal_id = if parts[2].is_empty() {
                None
            } else {
                Some(parts[2].to_string())
            };
            let desktop: Option<u32> = parts[3].parse().ok().filter(|&d| d > 0);
            let caption = parts[4].to_string();
            if pid > 0 && !caption.is_empty() {
                windows.push(WindowInfo {
                    caption,
                    resource_class,
                    pid,
                    internal_id,
                    desktop,
                });
            }
        }
    }

    Ok(windows)
}

/// Run a KWin script (fire-and-forget, no data callback needed).
fn run_kwin_script(script_body: &str) -> Result<(), String> {
    let conn = SyncConnection::new_session().map_err(|e| format!("D-Bus session: {e}"))?;

    let script_path = std::env::temp_dir().join("lychi_kwin_action.js");
    std::fs::write(&script_path, script_body).map_err(|e| format!("write script: {e}"))?;

    let call_id = CALL_COUNTER.fetch_add(1, Ordering::Relaxed);
    let plugin_name = format!("lychi_action_{}_{}", std::process::id(), call_id);

    let scripting = conn.with_proxy("org.kde.KWin", "/Scripting", Duration::from_secs(2));

    let (script_id,): (i32,) = scripting
        .method_call(
            "org.kde.kwin.Scripting",
            "loadScript",
            (script_path.to_str().unwrap_or_default(), &plugin_name),
        )
        .map_err(|e| format!("loadScript: {e}"))?;

    if script_id < 0 {
        return Err(format!("loadScript returned {script_id}"));
    }

    let script_path_dbus = format!("/Scripting/Script{script_id}");
    let script_proxy = conn.with_proxy("org.kde.KWin", &script_path_dbus, Duration::from_secs(2));

    script_proxy
        .method_call::<(), _, _, _>("org.kde.kwin.Script", "run", ())
        .map_err(|e| format!("run: {e}"))?;

    // Give KWin a moment to execute
    let _ = conn.process(Duration::from_millis(200));

    // Cleanup
    let _ = script_proxy.method_call::<(), _, _, _>("org.kde.kwin.Script", "stop", ());
    let _ = scripting.method_call::<(), _, _, _>(
        "org.kde.kwin.Scripting",
        "unloadScript",
        (&plugin_name,),
    );
    let _ = std::fs::remove_file(&script_path);

    Ok(())
}
