//! Screenshot capture — a signature Linux feature. The catch is that Linux
//! screenshot tooling is fragmented across compositors: wlroots (grim+slurp),
//! KDE (spectacle), GNOME (gnome-screenshot), plus the cross-desktop flameshot
//! and the X11 classics (scrot, maim, ImageMagick's import).
//!
//! The **primary** path is the XDG Screenshot portal
//! (`org.freedesktop.portal.Screenshot`) — DE-agnostic and requires *no*
//! screenshot tool installed. When no portal backend is present, this handler
//! falls back to being **adaptive**: it probes the session type and which tools
//! are actually installed, then picks the best available one and maps the
//! requested mode onto that tool's own flags. So the same `screenshot area`
//! works on Sway, Plasma, GNOME, or bare X11 — whatever the user happens to
//! run — with zero configuration.
//!
//! Modes:
//!   - `screenshot`         → full screen (all monitors)
//!   - `screenshot area`    → interactive region select (aliases: region, select)
//!   - `screenshot window`  → active window (falls back to region if unsupported)
//!
//! Every capture is saved to the Pictures directory with a timestamped name and
//! copied to the clipboard when the tool supports it.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;

pub struct ScreenshotHandler;

impl ScreenshotHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ScreenshotHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// What the user wants to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Full,
    Area,
    Window,
}

impl Mode {
    fn parse(args: &str) -> Mode {
        match args.trim().to_ascii_lowercase().as_str() {
            "area" | "region" | "select" | "selection" | "crop" => Mode::Area,
            "window" | "win" | "active" => Mode::Window,
            _ => Mode::Full,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Mode::Full => "full screen",
            Mode::Area => "selected region",
            Mode::Window => "active window",
        }
    }
}

/// Whether the tool copies to clipboard on its own, so we don't double-handle.
struct Plan {
    /// Program to run.
    program: String,
    /// Arguments, already resolved for the chosen mode + output path.
    args: Vec<String>,
    /// If set, pipe the tool's stdout into this program (grim → wl-copy path is
    /// handled separately; this is for the file-then-clipboard case).
    clipboard_after: Option<ClipboardCopy>,
    /// True when the tool wrote to `path` (so we can report the saved file).
    writes_file: bool,
}

/// How to place the saved file on the clipboard when the capture tool can't.
struct ClipboardCopy {
    program: &'static str,
    /// Args template with `{path}` and `{mime}` placeholders.
    args: Vec<String>,
}

fn is_wayland() -> bool {
    std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|s| s.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false)
}

fn have(tool: &str) -> bool {
    which::which(tool).is_ok()
}

/// Build a timestamped destination path in the Pictures dir (falling back to
/// home, then /tmp). Name mirrors what GNOME/KDE produce so it feels native.
fn output_path() -> PathBuf {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = dirs::picture_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join(format!("Screenshot_{secs}.png"))
}

/// Pick the best available tool for the environment and mode, returning a
/// concrete plan. `None` means no supported tool is installed.
///
/// Preference order is chosen so the most capable, most native tool wins first:
/// on Wayland we prefer the compositor-native tool (spectacle on KDE, grim on
/// wlroots, gnome-screenshot on GNOME), then flameshot, then X11 tools as a
/// last resort. On X11 we prefer flameshot/spectacle (rich), then scrot/maim.
fn plan(mode: Mode, path: &str) -> Option<Plan> {
    let wayland = is_wayland();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let is_kde = desktop.to_ascii_uppercase().contains("KDE")
        || desktop.to_ascii_uppercase().contains("PLASMA");
    let is_gnome = desktop.to_ascii_uppercase().contains("GNOME");

    // Ordered list of candidate tool ids to try. The first installed one wins.
    let mut order: Vec<&str> = Vec::new();
    if wayland {
        if is_kde {
            order.push("spectacle");
        }
        if is_gnome {
            order.push("gnome-screenshot");
        }
        order.extend(["grim", "spectacle", "gnome-screenshot", "flameshot"]);
    } else {
        order.extend([
            "flameshot",
            "spectacle",
            "gnome-screenshot",
            "maim",
            "scrot",
            "import",
        ]);
    }
    // De-duplicate while preserving order.
    let mut seen = std::collections::HashSet::new();
    order.retain(|t| seen.insert(*t));

    let tool = order.into_iter().find(|t| have(t))?;
    Some(build_plan(tool, mode, path))
}

/// Map (tool, mode) → concrete command. Kept separate from `plan` so it's unit
/// testable without a real environment.
fn build_plan(tool: &str, mode: Mode, path: &str) -> Plan {
    let s = |x: &str| x.to_string();
    match tool {
        // KDE — spectacle background mode (-b): captures and exits with NO
        // Spectacle app window, just the native selector where relevant. -n no
        // notification, -c copies to clipboard, -o writes the file.
        //   -f full screen
        //   -r region  → Spectacle's own crosshair drag (the good region UX)
        //   -u window-under-cursor → click to pick a window. NOT -a (active
        //      window), because Lychi itself is the active/focused window when
        //      the command runs, so -a would screenshot Lychi.
        "spectacle" => {
            let mut args = vec![s("-b"), s("-n"), s("-c"), s("-o"), s(path)];
            match mode {
                Mode::Full => args.insert(0, s("-f")),
                Mode::Area => args.insert(0, s("-r")),
                Mode::Window => args.insert(0, s("-u")),
            }
            Plan {
                program: s("spectacle"),
                args,
                clipboard_after: None, // -c handles it
                writes_file: true,
            }
        }
        // wlroots — grim (+ slurp for region). grim writes the file to a
        // positional path; no window mode, so window degrades to region.
        "grim" => {
            let args = match mode {
                Mode::Full => vec![s(path)],
                // grim -g "<geometry from slurp>" <path>
                Mode::Area | Mode::Window => vec![s("-g"), s("__SLURP__"), s(path)],
            };
            Plan {
                program: s("grim"),
                args,
                clipboard_after: Some(ClipboardCopy {
                    program: "wl-copy",
                    args: vec![s("--type"), s("image/png")],
                }),
                writes_file: true,
            }
        }
        // GNOME — gnome-screenshot. -a area, -w window, default full; -f file.
        "gnome-screenshot" => {
            let mut args = vec![s("-f"), s(path)];
            match mode {
                Mode::Full => {}
                Mode::Area => args.insert(0, s("-a")),
                Mode::Window => args.insert(0, s("-w")),
            }
            Plan {
                program: s("gnome-screenshot"),
                args,
                clipboard_after: Some(ClipboardCopy {
                    program: "wl-copy",
                    args: vec![s("--type"), s("image/png")],
                }),
                writes_file: true,
            }
        }
        // Cross-desktop — flameshot. `full`/`gui` (region). -p path, -c clip.
        // No dedicated active-window mode, so window degrades to interactive.
        "flameshot" => {
            let sub = match mode {
                Mode::Full => "full",
                Mode::Area | Mode::Window => "gui",
            };
            Plan {
                program: s("flameshot"),
                args: vec![s(sub), s("-c"), s("-p"), s(path)],
                clipboard_after: None, // -c handles it
                writes_file: true,
            }
        }
        // X11 — maim. -s region, -i window id (we can't easily get one, so
        // window degrades to region). Positional file.
        "maim" => {
            let mut args = vec![s(path)];
            if matches!(mode, Mode::Area | Mode::Window) {
                args.insert(0, s("-s"));
            }
            Plan {
                program: s("maim"),
                args,
                clipboard_after: Some(ClipboardCopy {
                    program: "xclip",
                    args: vec![s("-selection"), s("clipboard"), s("-t"), s("image/png")],
                }),
                writes_file: true,
            }
        }
        // X11 — scrot. -s region, -u active window. Positional file.
        "scrot" => {
            let mut args = vec![s(path)];
            match mode {
                Mode::Full => {}
                Mode::Area => args.insert(0, s("-s")),
                Mode::Window => args.insert(0, s("-u")),
            }
            Plan {
                program: s("scrot"),
                args,
                clipboard_after: Some(ClipboardCopy {
                    program: "xclip",
                    args: vec![s("-selection"), s("clipboard"), s("-t"), s("image/png")],
                }),
                writes_file: true,
            }
        }
        // X11 — ImageMagick import. -window root full; otherwise interactive.
        "import" => {
            let args = match mode {
                Mode::Full => vec![s("-window"), s("root"), s(path)],
                Mode::Area | Mode::Window => vec![s(path)],
            };
            Plan {
                program: s("import"),
                args,
                clipboard_after: Some(ClipboardCopy {
                    program: "xclip",
                    args: vec![s("-selection"), s("clipboard"), s("-t"), s("image/png")],
                }),
                writes_file: true,
            }
        }
        // Unreachable: `plan` only picks from the tools above.
        other => Plan {
            program: s(other),
            args: vec![s(path)],
            clipboard_after: None,
            writes_file: true,
        },
    }
}

/// Execute the plan. Handles the grim+slurp two-step and the file→clipboard
/// copy where the tool can't do it itself.
fn capture(plan: Plan, path: &str) -> Result<(), String> {
    let mut args = plan.args.clone();

    // grim's region path needs a live geometry from slurp first.
    if let Some(idx) = args.iter().position(|a| a == "__SLURP__") {
        if !have("slurp") {
            return Err(
                "Region capture on this compositor needs `slurp` — install it (it pairs with grim)"
                    .to_string(),
            );
        }
        let geo = Command::new("slurp")
            .output()
            .map_err(|e| format!("Failed to run slurp: {e}"))?;
        if !geo.status.success() {
            // Non-zero usually means the user cancelled the selection.
            return Err("Selection cancelled".to_string());
        }
        let geometry = String::from_utf8_lossy(&geo.stdout).trim().to_string();
        if geometry.is_empty() {
            return Err("Selection cancelled".to_string());
        }
        args[idx] = geometry;
    }

    let status = Command::new(&plan.program)
        .args(&args)
        .status()
        .map_err(|e| format!("Failed to run {}: {e}", plan.program))?;
    if !status.success() {
        // Interactive tools exit non-zero when the user cancels — treat that as
        // a soft cancel rather than a hard error.
        return Err("Screenshot cancelled".to_string());
    }

    // Copy the saved file to the clipboard if the tool didn't already.
    if let Some(copy) = plan.clipboard_after
        && plan.writes_file
        && have(copy.program)
    {
        copy_file_to_clipboard(&copy, path);
    }

    Ok(())
}

/// Best-effort: read the file and pipe it into a clipboard program. Failure is
/// non-fatal — the file is already saved, which is the primary outcome.
fn copy_file_to_clipboard(copy: &ClipboardCopy, path: &str) {
    use std::io::Write;
    use std::process::Stdio;

    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Ok(mut child) = Command::new(copy.program)
        .args(&copy.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&bytes);
    }
    let _ = child.wait();
}

/// Fire a desktop notification confirming the capture, using the saved image
/// as the notification icon (a thumbnail). Best-effort — failure is non-fatal.
fn notify_saved(path: &str, mode: Mode) {
    let _ = notify_rust::Notification::new()
        .summary("Screenshot saved")
        .body(&format!("{} — copied to clipboard", mode.label()))
        // Absolute path → notification servers render it as the icon/thumbnail.
        .icon(path)
        .appname("Lychi")
        .timeout(notify_rust::Timeout::Milliseconds(4000))
        .show();
}

/// Outcome of the portal attempt, so the caller knows whether to fall back.
enum PortalOutcome {
    /// Portal captured to `dest` — done, no tool fallback needed.
    Captured,
    /// Portal is unavailable on this system — fall back to CLI tools.
    Unavailable,
    /// Portal ran but the user cancelled / it errored — surface this, don't
    /// silently retry with a tool (which would re-prompt the user).
    Failed(String),
}

/// Try the XDG Screenshot portal (`org.freedesktop.portal.Screenshot`).
///
/// This is the industry-standard, DE-agnostic path: it needs **no** screenshot
/// tool installed and works on KDE, GNOME, wlroots, Hyprland, Sway, COSMIC —
/// anywhere a portal backend is running. The portal writes to its own location
/// and hands back a `file://` URI; we copy that into our timestamped Pictures
/// path so the result matches the tool path (saved file + clipboard).
///
/// We speak the D-Bus interface directly (via the `dbus` crate the core already
/// links) rather than pull in a portal wrapper crate, to avoid a heavy new
/// dependency and its version churn.
///
/// The Screenshot portal only distinguishes non-interactive vs interactive; it
/// has no "active window" mode. So: Full → non-interactive; Area/Window →
/// interactive (the portal presents its own region/window picker). A dedicated
/// active-window capture, when the portal can't, is left to the tool fallback.
async fn try_portal_capture(mode: Mode, dest: &str) -> PortalOutcome {
    let interactive = !matches!(mode, Mode::Full);

    // The `dbus` crate's blocking connection spins up its own internal runtime,
    // which PANICS with "Cannot start a runtime from within a runtime" if run on
    // a tokio worker thread — and `spawn_blocking` threads are tokio-managed, so
    // they trip it too. Run the handshake on a *plain* std thread (no ambient
    // tokio runtime) and await the result over a oneshot channel. This keeps the
    // async fn non-blocking while giving the D-Bus call a clean thread.
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(portal_screenshot_blocking(interactive));
    });

    let uri = match rx.await {
        Ok(Ok(Some(uri))) => uri,
        // Portal returned but produced no URI, or the user cancelled the dialog
        // (response code != 0). A deliberate cancel shouldn't fall back to a
        // CLI tool and re-prompt.
        Ok(Ok(None)) => return PortalOutcome::Failed("Screenshot cancelled".to_string()),
        // No portal backend / D-Bus unavailable, or the worker thread died → let
        // the tool path handle it.
        Ok(Err(_)) | Err(_) => return PortalOutcome::Unavailable,
    };

    // The portal hands back a file:// URI. Resolve it to a path and copy the
    // bytes into our timestamped Pictures destination.
    let src = match uri_to_path(&uri) {
        Some(p) => p,
        None => {
            return PortalOutcome::Failed(
                "Screenshot portal returned an unreadable location".to_string(),
            );
        }
    };
    if let Err(e) = std::fs::copy(&src, dest) {
        return PortalOutcome::Failed(format!("Failed to save screenshot: {e}"));
    }
    // Best-effort: the portal often keeps the file in a temp/cache dir; remove
    // the source copy so we don't leave duplicates behind. Non-fatal.
    let _ = std::fs::remove_file(&src);

    // Copy to clipboard using whatever tool fits the session (best-effort).
    copy_dest_to_clipboard(dest);

    PortalOutcome::Captured
}

/// Decode a `file://` URI to a filesystem path (handles percent-encoding).
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Strip an optional authority component ("//host/path" → "/path"); portals
    // emit a bare local path, so anything before the first '/' is a host we drop.
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(i) => &rest[i..],
        None => return None,
    };
    let decoded = urlencoding::decode(path).ok()?;
    Some(PathBuf::from(decoded.into_owned()))
}

/// Blocking D-Bus handshake with `org.freedesktop.portal.Screenshot`.
///
/// Returns `Ok(Some(uri))` on a successful capture, `Ok(None)` on user cancel
/// (or a success response carrying no uri), and `Err` if the portal is
/// unavailable (so the caller can fall back to CLI tools).
fn portal_screenshot_blocking(interactive: bool) -> Result<Option<String>, String> {
    use dbus::arg::{RefArg, Variant};
    use dbus::blocking::SyncConnection;
    use dbus::message::MatchRule;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    let conn = SyncConnection::new_session().map_err(|e| format!("D-Bus session: {e}"))?;

    // The portal replies asynchronously via a Response signal on a Request
    // object path. To avoid a race we predict that path from a handle_token and
    // subscribe to its Response signal *before* issuing the call. The path is
    //   /org/freedesktop/portal/desktop/request/<SENDER>/<TOKEN>
    // where SENDER is our unique bus name with dots→underscores and the leading
    // ':' stripped. (Portal spec, org.freedesktop.portal.Request.)
    let token = format!("lychi_{}", std::process::id());
    let sender = conn
        .unique_name()
        .trim_start_matches(':')
        .replace('.', "_");
    let request_path = format!(
        "/org/freedesktop/portal/desktop/request/{sender}/{token}"
    );

    let (tx, rx) = mpsc::channel::<(u32, Option<String>)>();
    let tx = Arc::new(Mutex::new(tx));

    let mut rule = MatchRule::new_signal("org.freedesktop.portal.Request", "Response");
    rule.path = Some(request_path.clone().into());

    let tx_cb = tx.clone();
    let _token = conn
        .add_match(rule, move |_: (), _conn, msg| {
            // Response(u response, a{sv} results). response==0 means success.
            let mut it = msg.iter_init();
            let code: u32 = it.read().unwrap_or(1);
            let mut uri: Option<String> = None;
            // results is a{sv}; find the "uri" key.
            if let Ok(dict) = it.read::<dbus::arg::PropMap>()
                && let Some(v) = dict.get("uri")
            {
                uri = v.0.as_str().map(|s| s.to_string());
            }
            if let Ok(tx) = tx_cb.lock() {
                let _ = tx.send((code, uri));
            }
            true
        })
        .map_err(|e| format!("add_match: {e}"))?;

    // Issue the Screenshot call. Signature: Screenshot(s parent_window,
    // a{sv} options) -> (o handle). Options carry our handle_token and the
    // interactive flag.
    let proxy = conn.with_proxy(
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        Duration::from_secs(2),
    );
    let mut options: dbus::arg::PropMap = std::collections::HashMap::new();
    options.insert(
        "handle_token".to_string(),
        Variant(Box::new(token.clone()) as Box<dyn RefArg>),
    );
    options.insert(
        "interactive".to_string(),
        Variant(Box::new(interactive) as Box<dyn RefArg>),
    );

    let call: Result<(dbus::Path,), _> = proxy.method_call(
        "org.freedesktop.portal.Screenshot",
        "Screenshot",
        ("", options),
    );
    // A failed call here (no such service/method) means no portal backend.
    call.map_err(|e| format!("Screenshot call: {e}"))?;

    // Pump the connection until the Response signal arrives or we time out.
    // Interactive captures wait on the user, so allow a generous window.
    let deadline = if interactive { 120 } else { 15 };
    let end = std::time::Instant::now() + Duration::from_secs(deadline);
    loop {
        if let Ok((code, uri)) = rx.try_recv() {
            return Ok(if code == 0 { uri } else { None });
        }
        if std::time::Instant::now() >= end {
            return Ok(None);
        }
        // Process incoming messages (fires the add_match callback).
        conn.process(Duration::from_millis(200))
            .map_err(|e| format!("D-Bus process: {e}"))?;
    }
}

/// Best-effort clipboard copy of the saved file, session-appropriate:
/// `wl-copy` on Wayland, `xclip` on X11. Non-fatal — the file is already saved.
fn copy_dest_to_clipboard(path: &str) {
    let s = |x: &str| x.to_string();
    let copy = if is_wayland() && have("wl-copy") {
        Some(ClipboardCopy {
            program: "wl-copy",
            args: vec![s("--type"), s("image/png")],
        })
    } else if have("xclip") {
        Some(ClipboardCopy {
            program: "xclip",
            args: vec![s("-selection"), s("clipboard"), s("-t"), s("image/png")],
        })
    } else {
        None
    };
    if let Some(copy) = copy {
        copy_file_to_clipboard(&copy, path);
    }
}

#[async_trait]
impl ActionHandler for ScreenshotHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&[
            "screenshot",
            "screencap",
            "screengrab",
        ])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "screenshot"
    }

    fn description(&self) -> &str {
        "Capture a screenshot (full screen, region, or window)"
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let p = partial.trim().to_ascii_lowercase();
        let hints = [
            ("", "screenshot", "Capture the full screen", "screenshot"),
            (
                "area",
                "screenshot area",
                "Select a region to capture",
                "screenshot area",
            ),
            (
                "window",
                "screenshot window",
                "Capture the active window",
                "screenshot window",
            ),
        ];
        hints
            .iter()
            .filter(|(key, _, _, _)| {
                p.is_empty() || key.starts_with(p.as_str()) || p == "screenshot"
            })
            .enumerate()
            .map(|(i, (_, label, desc, run))| {
                CompletionItem::new(
                    (*label).to_string(),
                    Some("__none__".into()),
                    900 - i as u16,
                )
                .with_run((*run).to_string())
                .with_description((*desc).to_string())
            })
            .collect()
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let mode = Mode::parse(args);
        let path = output_path();
        let path_str = path.to_string_lossy().to_string();

        // Capture-method priority is mode-dependent:
        //
        // - Region/Window: prefer the NATIVE tool (spectacle -r/-u, grim+slurp,
        //   flameshot gui, …). Each brings its own selector — Spectacle's region
        //   crosshair, window-under-cursor pick — which is the good, direct UX.
        //   The portal's interactive picker is clunkier and is only a fallback
        //   here. This is the fix for the poor area/window experience.
        // - Full screen: prefer the portal (no selector needed; DE-agnostic,
        //   needs nothing installed), tool as fallback.
        let native_first = !matches!(mode, Mode::Full);

        if native_first
            && let Some(plan) = plan(mode, &path_str)
        {
            match capture(plan, &path_str) {
                Ok(()) if std::path::Path::new(&path_str).exists() => {
                    notify_saved(&path_str, mode);
                    return Ok(ActionResult::ok(
                        format!(
                            "Screenshot saved ({})\n{}\n\n📋 Copied to clipboard",
                            mode.label(),
                            path_str
                        ),
                        OutputType::Text,
                    ));
                }
                // A soft cancel (user aborted the region drag) is a deliberate
                // action — report it, don't fall through to a second selector.
                Ok(()) => return Ok(ActionResult::err("Screenshot cancelled")),
                Err(e) if e.contains("cancelled") || e.contains("Selection cancelled") => {
                    return Ok(ActionResult::err(e));
                }
                // A real tool failure (missing slurp, etc.) → try the portal.
                Err(_) => {}
            }
        }

        // Portal path: primary for full screen, fallback for region/window.
        match try_portal_capture(mode, &path_str).await {
            PortalOutcome::Captured => {
                return Ok(if std::path::Path::new(&path_str).exists() {
                    notify_saved(&path_str, mode);
                    ActionResult::ok(
                        format!(
                            "Screenshot saved ({})\n{}\n\n📋 Copied to clipboard",
                            mode.label(),
                            path_str
                        ),
                        OutputType::Text,
                    )
                } else {
                    ActionResult::err("Screenshot cancelled")
                });
            }
            PortalOutcome::Failed(e) => return Ok(ActionResult::err(e)),
            // Portal not available → fall through to the tool-detection path.
            PortalOutcome::Unavailable => {}
        }

        let Some(plan) = plan(mode, &path_str) else {
            return Ok(ActionResult::err(
                "No screenshot method available. Enable an XDG screenshot portal, \
                 or install one of: grim (+slurp), spectacle, gnome-screenshot, \
                 flameshot, scrot, or maim.",
            ));
        };

        match capture(plan, &path_str) {
            Ok(()) => {
                // Confirm the file actually landed (interactive cancels can
                // succeed-exit without writing).
                if std::path::Path::new(&path_str).exists() {
                    // Close the loop with a desktop notification — the capture
                    // often happens after Lychi has hidden (quick trigger / CLI),
                    // so this is the only visible confirmation. The saved image
                    // is used as the notification icon (a thumbnail preview).
                    notify_saved(&path_str, mode);
                    Ok(ActionResult::ok(
                        format!(
                            "Screenshot saved ({})\n{}\n\n📋 Copied to clipboard",
                            mode.label(),
                            path_str
                        ),
                        OutputType::Text,
                    ))
                } else {
                    Ok(ActionResult::err("Screenshot cancelled"))
                }
            }
            Err(e) => Ok(ActionResult::err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parsing_is_forgiving() {
        assert_eq!(Mode::parse(""), Mode::Full);
        assert_eq!(Mode::parse("  "), Mode::Full);
        assert_eq!(Mode::parse("area"), Mode::Area);
        assert_eq!(Mode::parse("REGION"), Mode::Area);
        assert_eq!(Mode::parse("select"), Mode::Area);
        assert_eq!(Mode::parse("window"), Mode::Window);
        assert_eq!(Mode::parse("win"), Mode::Window);
        assert_eq!(Mode::parse("everything else"), Mode::Full);
    }

    #[test]
    fn spectacle_flags_per_mode() {
        let p = build_plan("spectacle", Mode::Full, "/tmp/x.png");
        assert_eq!(p.program, "spectacle");
        assert!(p.args.contains(&"-f".to_string()));
        assert!(p.args.contains(&"-o".to_string()));
        assert!(p.args.contains(&"-c".to_string())); // spectacle copies itself
        assert!(p.clipboard_after.is_none());

        let p = build_plan("spectacle", Mode::Area, "/tmp/x.png");
        assert!(p.args.contains(&"-r".to_string()));
        // Window mode uses -u (window-under-cursor / click-to-pick), NOT -a
        // (active window) — Lychi is the active window when the command runs.
        let p = build_plan("spectacle", Mode::Window, "/tmp/x.png");
        assert!(p.args.contains(&"-u".to_string()));
        assert!(!p.args.contains(&"-a".to_string()));
    }

    #[test]
    fn grim_region_uses_slurp_placeholder() {
        let p = build_plan("grim", Mode::Area, "/tmp/x.png");
        assert_eq!(p.program, "grim");
        assert!(p.args.contains(&"-g".to_string()));
        assert!(p.args.contains(&"__SLURP__".to_string()));
        // grim can't copy itself → needs a follow-up wl-copy.
        assert!(p.clipboard_after.is_some());

        // Full screen has no slurp step.
        let p = build_plan("grim", Mode::Full, "/tmp/x.png");
        assert!(!p.args.contains(&"__SLURP__".to_string()));
        assert_eq!(p.args, vec!["/tmp/x.png".to_string()]);
    }

    #[test]
    fn gnome_flags_per_mode() {
        assert!(
            build_plan("gnome-screenshot", Mode::Area, "/tmp/x.png")
                .args
                .contains(&"-a".to_string())
        );
        assert!(
            build_plan("gnome-screenshot", Mode::Window, "/tmp/x.png")
                .args
                .contains(&"-w".to_string())
        );
        // Full = no mode flag, just -f path.
        let p = build_plan("gnome-screenshot", Mode::Full, "/tmp/x.png");
        assert_eq!(p.args, vec!["-f".to_string(), "/tmp/x.png".to_string()]);
    }

    #[test]
    fn flameshot_subcommands() {
        assert!(
            build_plan("flameshot", Mode::Full, "/tmp/x.png")
                .args
                .contains(&"full".to_string())
        );
        assert!(
            build_plan("flameshot", Mode::Area, "/tmp/x.png")
                .args
                .contains(&"gui".to_string())
        );
    }

    #[test]
    fn scrot_active_window_flag() {
        let p = build_plan("scrot", Mode::Window, "/tmp/x.png");
        assert!(p.args.contains(&"-u".to_string()));
        let p = build_plan("scrot", Mode::Area, "/tmp/x.png");
        assert!(p.args.contains(&"-s".to_string()));
    }

    #[test]
    fn uri_to_path_decodes_file_uris() {
        assert_eq!(
            uri_to_path("file:///home/sab/Pictures/a.png"),
            Some(PathBuf::from("/home/sab/Pictures/a.png"))
        );
        // Percent-encoded space.
        assert_eq!(
            uri_to_path("file:///tmp/my%20shot.png"),
            Some(PathBuf::from("/tmp/my shot.png"))
        );
        // Authority form: host is dropped, absolute path kept.
        assert_eq!(
            uri_to_path("file://localhost/tmp/x.png"),
            Some(PathBuf::from("/tmp/x.png"))
        );
        // Not a file URI.
        assert_eq!(uri_to_path("https://example.com/x.png"), None);
    }

    #[test]
    fn output_path_is_timestamped_png() {
        let p = output_path();
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("Screenshot_"), "name: {name}");
        assert!(name.ends_with(".png"), "name: {name}");
    }
}
