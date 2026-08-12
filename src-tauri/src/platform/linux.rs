use std::path::PathBuf;
use tauri::Manager;
use tauri::WebviewWindow;
use tokio::sync::oneshot;

/// Set the GLib application/program name. Called once at startup before Tauri builder.
/// `prgname` sets the Wayland app-id, which KDE uses to match .desktop files for
/// the taskbar icon. Must match the .desktop file's filename or StartupWMClass.
pub fn init_app() {
    glib::set_prgname(Some("lychi-app"));
    glib::set_application_name("Lychi");
}

/// Show a blocking error dialog before the app has a window of its own.
///
/// Exists for the startup failures that happen *before* Tauri is running — a
/// second instance finding the database locked, above all. Those already print a
/// clear line to stderr, but somebody who launched from a desktop icon never
/// sees stderr: the window simply fails to appear, with no explanation.
///
/// Uses GTK directly rather than the Tauri dialog plugin because at this point
/// there is no app handle to hang a dialog off. `gtk::init` is idempotent and
/// safe to call before `tauri::Builder`.
///
/// Best-effort by design: if GTK cannot start (no display, a TTY, a headless CI
/// runner) the caller has already written the same text to stderr, so failing to
/// draw a dialog costs nothing.
pub fn show_startup_error(title: &str, body: &str) {
    if gtk::init().is_err() {
        return;
    }
    use gtk::prelude::{DialogExt, GtkWindowExt, MessageDialogExt, WidgetExtManual};
    let dialog = gtk::MessageDialog::new(
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Warning,
        gtk::ButtonsType::Ok,
        title,
    );
    dialog.set_secondary_text(Some(body));
    dialog.set_title("Lychi");
    dialog.run();
    // SAFETY: the dialog is owned here and never referenced again; `run` has
    // already returned, so no GTK callback can still be holding it.
    unsafe {
        dialog.destroy();
    }
}

/// Detect KDE Plasma on Wayland (where layer-shell focus is unreliable).
///
/// Delegates to the core compositor decider (`is_kde_wayland_session`: D-Bus
/// `NameHasOwner("org.kde.KWin")` with a session fallback). This file used to
/// keep a private env parser that read `XDG_SESSION_DESKTOP` **first** — but
/// that variable holds a session *file name* (`plasma`), not a desktop name,
/// which the session decider's own tests document. On any Plasma install whose
/// display manager exports the file name (GDM, LightDM, greetd), the `kde`
/// fact came back false and auto strategy resolved to LayerShell on KWin — the
/// configuration I-008 says cannot receive keyboard focus. The same trap was
/// already removed from `hotkey_de` and `context::compositor()`; this was the
/// last surviving copy, invisible locally because the dev box happens to
/// export `XDG_SESSION_DESKTOP=KDE`.
pub fn is_kde_wayland() -> bool {
    lychi_core::context::is_kde_wayland_session()
}

/// Is this a Wayland session?
///
/// Delegates to `lychi_core::context::is_wayland` rather than re-reading the
/// environment. This file used to carry its own copy that tested only
/// `XDG_SESSION_TYPE == "wayland"` — but that variable is **frequently absent
/// under autostart**, which the core function documents (it was found and fixed
/// there, once, after it misrouted the hotkey path on boot).
///
/// The copy here still had the bug, and it decides more than the hotkey. With
/// `XDG_SESSION_TYPE` unset on a KDE Wayland session, `resolve_strategy`'s auto
/// branch saw `is_kde_wayland() == false`, then `gtk_layer_shell::is_supported()
/// == true` on KWin, and selected `LayerShell` — the one configuration I-008
/// says cannot receive keyboard focus on KWin. An autostarted launcher would
/// come up unable to type.
///
/// One definition, in the crate that already reasoned about it.
fn is_wayland_session() -> bool {
    lychi_core::context::is_wayland()
}

/// Switch off WebKit subsystems Lychi does not use.
///
/// **Why this exists.** WebKitGTK builds a GStreamer pipeline in every
/// WebProcess, whether or not the page has media in it. On a host without
/// `gst-plugins-base` the pipeline can't be built, the WebProcess dies, and the
/// UI goes blank while the app process stays alive — reported from GNOME
/// Wayland as "crashes on any keystroke, and I have to Ctrl-C it".
///
/// Lychi renders no `<video>`, no `<audio>`, and calls no `getUserMedia`. So
/// this is not a workaround for a missing codec: it removes a dependency the
/// app never had. Bundling GStreamer to satisfy a subsystem we then don't use
/// would add ~15MB and re-open the `_dl_init` crash class that
/// `scripts/fix-appimage-codecs.sh` exists to prevent.
///
/// **Adding media later.** If Lychi ever needs a `<video>`, delete the
/// `set_enable_media(false)` line — and then the AppImage genuinely must ship
/// GStreamer, because the host can no longer be assumed to have it.
#[cfg(target_os = "linux")]
pub fn harden_webview(window: &WebviewWindow) {
    use webkit2gtk::{SettingsExt, WebViewExt};

    // with_webview runs on the main thread; failure here is not fatal — the
    // app still works on hosts that do have the codecs, so log and continue.
    let result = window.with_webview(|webview| {
        let wv = webview.inner();
        let Some(settings) = WebViewExt::settings(&wv) else {
            tracing::warn!("[webview] no WebKitSettings — media stays enabled");
            return;
        };
        // Tell WebKit it is not a browser.
        //
        // The default cache model is `WEB_BROWSER`, which sizes page/memory
        // caches for someone with thirty tabs and a back/forward history to keep
        // warm. Lychi has one document that never navigates, and WebKitGTK's docs
        // say an application without a browsing interface "can reduce memory
        // usage substantially" with `DOCUMENT_VIEWER`. wry does not set it, so
        // every Tauri app on Linux inherits the browser sizing.
        //
        // MEASURED: it changes nothing here. Idle PSS with and without, on
        // WebKitGTK 2.50 / Fedora 44: WebProcess 136 MB -> 135 MB, whole tree
        // 296 MB -> 295 MB. The log line below confirms the call ran, so this is
        // WebKit declining to act on the hint rather than the hint being missed
        // — the caches it governs are evidently not what a Tauri WebProcess is
        // holding at rest.
        //
        // Kept because it is a correct declaration of intent (this genuinely is
        // a document viewer, and a future WebKit may honour it), and because
        // deleting it would invite someone to "discover" the same idea again in
        // six months and re-measure it. The numbers above are the point of the
        // comment.
        //
        // The other documented lever, `WebKitMemoryPressureSettings` (default
        // limit = system RAM capped at 3GB), is a WebContext CONSTRUCTION
        // property. wry builds the context, so it cannot be reached from here
        // without patching wry.
        //
        // The context is shared by the whole process, so this is set once here
        // rather than per-view.
        if let Some(ctx) = WebViewExt::context(&wv) {
            use webkit2gtk::{CacheModel, WebContextExt};
            ctx.set_cache_model(CacheModel::DocumentViewer);
            tracing::info!("[webview] cache model = DOCUMENT_VIEWER (not a browser)");
        } else {
            tracing::warn!("[webview] no WebContext — cache model left at the browser default");
        }

        settings.set_enable_media(false);
        // WebRTC and the media-stream APIs pull in the same GStreamer stack
        // (and a launcher has no business opening a camera or microphone).
        settings.set_enable_media_stream(false);
        settings.set_enable_webrtc(false);
        tracing::info!("[webview] media/webrtc disabled (unused; avoids GStreamer dependency)");

        // When the WebProcess dies the window goes blank, but THIS process
        // keeps running and keeps logging — so the logs look healthy while the
        // app is unusable. That's exactly how I-013 was reported: "the UI
        // crashes but the app doesn't quit, I have to Ctrl-C it", with nothing
        // in the log naming a cause.
        //
        // Two responses, and the reload is the one the user notices. WebKit
        // supports recovering a dead WebProcess by reloading, which turns
        // "blank window, kill it from a terminal" into "it blinked and came
        // back". Reload once per death and let the log carry the diagnosis;
        // if it dies repeatedly the log shows a run of these rather than a
        // silent loop.
        wv.connect_web_process_terminated(|view, reason| {
            tracing::error!(
                ?reason,
                "[webview] WebProcess terminated — the UI went blank; reloading. \
                 If this repeats, please report it with this log."
            );
            view.reload();
        });

        // A failed load is the other way to end up staring at a blank window,
        // with a different cause (missing/broken frontend assets) and the same
        // symptom. Distinguishing the two in the log is the difference between
        // a diagnosable report and a guess.
        wv.connect_load_failed(|_, event, uri, error| {
            tracing::error!(?event, uri, %error, "[webview] load failed");
            // false = let WebKit show its own error page rather than swallow it
            // silently; a visible error beats an unexplained blank window.
            false
        });
    });
    if let Err(e) = result {
        tracing::warn!("[webview] could not apply settings: {e}");
    }
}

/// The window strategy init_window() resolved for this session.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowStrategy {
    /// wlr-layer-shell surface (wlroots compositors)
    LayerShell,
    /// Monitor-covering transparent xdg_toplevel, launcher centered by CSS
    /// (KDE Wayland, GNOME Wayland, any Wayland without usable layer-shell)
    Toplevel,
    /// Fullscreen override window with X11 hints
    X11,
}

impl WindowStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            WindowStrategy::LayerShell => "layer-shell",
            WindowStrategy::Toplevel => "toplevel",
            WindowStrategy::X11 => "x11",
        }
    }
}

static ACTIVE_STRATEGY: std::sync::OnceLock<WindowStrategy> = std::sync::OnceLock::new();

/// True when the user forced `window_strategy = "toplevel-window"` — the
/// escape hatch that skips fullscreen_on_monitor() on non-KDE Wayland.
static TOPLEVEL_PLAIN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn toplevel_plain() -> bool {
    TOPLEVEL_PLAIN.get().copied().unwrap_or(false)
}

/// Whether to ask the compositor for true fullscreen on the toplevel path.
///
/// **Never on Mutter.** GNOME deliberately paints an opaque black backdrop
/// behind fullscreen windows (mutter "Draw black background for fullscreen
/// windows"), which destroys the alpha the whole toplevel design depends on:
/// the window covers the monitor precisely so CSS can float a small launcher
/// bar over a transparent backdrop. Asking for fullscreen there is asking for
/// the one state where our transparency is guaranteed to be thrown away —
/// GNOME users see an opaque full-screen panel instead of a launcher.
///
/// Dropping the request costs nothing: the window is already sized to the full
/// monitor via `set_size_request`, and Mutter centers it. Fullscreen was only
/// ever a positioning trick, because Mutter ignores `move_()` — but a window
/// that already fills the monitor has nowhere to be mispositioned to.
///
/// KDE takes neither branch (it honours `move_()`, handled separately), and
/// `toplevel-window` remains the user-facing escape hatch.
fn wants_fullscreen() -> bool {
    !toplevel_plain() && !is_gnome_like()
}

/// GNOME/Mutter, or anything presenting itself as GNOME (Unity, Pantheon,
/// Budgie all run Mutter or a fork). Matched by desktop name rather than by
/// probing for a Mutter-specific protocol, because the black-fullscreen
/// behaviour lives in the shell's compositing path, not in a protocol we can
/// feature-detect.
fn is_gnome_like() -> bool {
    let s = lychi_core::context::session::session();
    is_mutter_family(s.is_wayland(), &s.desktops)
}

/// The rule, as a pure function of the session facts.
///
/// Split out so it can be tested: the previous version read the environment
/// inline, and once session detection became cached (correctly — env vars do
/// not change) the env-mutating tests around it stopped being able to affect
/// the answer at all. They then passed or failed depending on which test in the
/// binary ran first.
///
/// Wayland-gated because the black-fullscreen behaviour is in Mutter's Wayland
/// compositing path. Note `GnomeClassic`/`GnomeFlashback` are X11-only sessions,
/// so they never reach this — which is correct, and previously obscured by
/// matching them in a list that could not fire.
fn is_mutter_family(wayland: bool, desktops: &[lychi_core::context::session::Desktop]) -> bool {
    use lychi_core::context::session::Desktop;
    wayland
        && desktops.iter().any(|d| {
            d.is_gnome_family() || matches!(d, Desktop::Unity | Desktop::Pantheon | Desktop::Budgie)
        })
}

/// Whether the X11 screen has a compositor. Without one (xfwm4/Marco with
/// compositing off) the transparent fullscreen overlay renders opaque black.
/// Set by init_window(); true on Wayland (always composited).
static SCREEN_COMPOSITED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub fn screen_composited() -> bool {
    SCREEN_COMPOSITED.get().copied().unwrap_or(true)
}

/// Compact opaque window used on non-composited X11 (rofi-style fallback).
/// Width matches the launcher bar; height starts at the input-bar height and
/// is resized to content by the frontend (window.setSize via ResizeObserver).
const COMPACT_W: i32 = 680;
const COMPACT_INITIAL_H: i32 = 64;

fn compact_x(geom: &gdk::Rectangle) -> i32 {
    geom.x() + (geom.width() - COMPACT_W) / 2
}

fn compact_y(geom: &gdk::Rectangle) -> i32 {
    geom.y() + (geom.height() as f64 * 0.18) as i32
}

/// The strategy resolved by init_window(). Every consumer (dismiss handlers,
/// reposition, settings IPC) reads this instead of re-deriving from env vars.
/// Defaults to X11 if queried before init_window() ran.
pub fn active_strategy() -> WindowStrategy {
    ACTIVE_STRATEGY
        .get()
        .copied()
        .unwrap_or(WindowStrategy::X11)
}

/// What the session looks like, as the strategy decision sees it.
///
/// Split out so [`decide_strategy`] is a pure function of these facts. The
/// decision used to read the environment and probe GTK inline, which made the
/// autostart bug (see [`is_wayland_session`]) unreachable by any test — the
/// failing combination could only be produced by actually booting into it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionFacts {
    pub wayland: bool,
    pub kde: bool,
    pub layer_shell: bool,
}

impl SessionFacts {
    fn probe() -> Self {
        Self {
            wayland: is_wayland_session(),
            // KWin-over-Wayland from the core compositor decider — never a
            // private env parse (see `is_kde_wayland`). False on X11 by
            // construction; the auto branch's `wayland && kde` tolerates that.
            kde: is_kde_wayland(),
            layer_shell: gtk_layer_shell::is_supported(),
        }
    }
}

/// The strategy rule, as a pure function of the session facts.
pub(crate) fn decide_strategy(strategy: &str, f: SessionFacts) -> WindowStrategy {
    match strategy {
        "layer-shell" => {
            if f.layer_shell {
                WindowStrategy::LayerShell
            } else if f.wayland {
                WindowStrategy::Toplevel
            } else {
                WindowStrategy::X11
            }
        }
        "toplevel" | "toplevel-window" => WindowStrategy::Toplevel,
        "x11" => WindowStrategy::X11,
        _ => {
            // "auto"
            //
            // KDE is checked on `kde && wayland` rather than a combined helper
            // so the Wayland fact has exactly one source. When that fact was
            // wrong (XDG_SESSION_TYPE unset under autostart) this branch fell
            // through to layer-shell ON KWIN, which I-008 says cannot take
            // keyboard focus.
            if f.wayland && f.kde {
                WindowStrategy::Toplevel
            } else if f.layer_shell {
                WindowStrategy::LayerShell
            } else if f.wayland {
                // GNOME (Mutter has no layer-shell) and unknown compositors
                WindowStrategy::Toplevel
            } else {
                WindowStrategy::X11
            }
        }
    }
}

/// Resolve the configured strategy string to a concrete strategy for this session.
fn resolve_strategy(strategy: &str) -> WindowStrategy {
    let facts = SessionFacts::probe();
    let resolved = decide_strategy(strategy, facts);
    if strategy == "layer-shell" && !facts.layer_shell {
        tracing::warn!(
            "layer-shell strategy requested but not supported — falling back to {resolved:?}"
        );
    }
    tracing::info!(
        "window strategy: {resolved:?} (wayland={} kde={} layer_shell={})",
        facts.wayland,
        facts.kde,
        facts.layer_shell
    );
    resolved
}

/// Strip ANSI escape codes from kscreen-doctor output.
fn strip_ansi(text: &str) -> String {
    text.chars()
        .scan(false, |in_esc, ch| {
            if *in_esc {
                *in_esc = ch != 'm';
                Some(None)
            } else if ch == '\x1b' {
                *in_esc = true;
                Some(None)
            } else {
                Some(Some(ch))
            }
        })
        .flatten()
        .collect()
}

/// Find the primary monitor, working around KDE Wayland where GDK's
/// `primary_monitor()` always returns None.
fn find_primary_monitor(display: &gdk::Display) -> Option<gdk::Monitor> {
    use gtk::prelude::MonitorExt;

    // Try GDK's built-in primary detection first (works on X11, GNOME Wayland)
    if let Some(m) = display.primary_monitor() {
        return Some(m);
    }

    // KDE Wayland fallback: ask kscreen-doctor for the priority-1 output's geometry,
    // then match against GDK monitors by position (since GDK doesn't expose
    // connector names like "HDMI-A-1" on Wayland).
    if let Ok(output) = std::process::Command::new("kscreen-doctor")
        .arg("-o")
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        let clean = strip_ansi(&text);

        // Parse kscreen-doctor output to find the priority-1 output's geometry.
        // Format:
        //   Output: 2 HDMI-A-1 ...
        //     priority 1
        //     Geometry: 1920,0 1920x1080
        let mut is_primary = false;
        let mut primary_geom: Option<(i32, i32)> = None;

        for line in clean.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("Output:") {
                is_primary = false;
            } else if trimmed.starts_with("priority 1") {
                is_primary = true;
            } else if is_primary && trimmed.starts_with("Geometry:") {
                // "Geometry: 1920,0 1920x1080"
                if let Some(coords) = trimmed.strip_prefix("Geometry:").map(|s| s.trim())
                    && let Some(pos) = coords.split_whitespace().next()
                {
                    let parts: Vec<&str> = pos.split(',').collect();
                    if parts.len() == 2
                        && let (Ok(x), Ok(y)) = (parts[0].parse(), parts[1].parse())
                    {
                        primary_geom = Some((x, y));
                    }
                }
                break;
            }
        }

        if let Some((px, py)) = primary_geom {
            tracing::debug!("KDE primary monitor at {px},{py}");
            let n = display.n_monitors();
            for i in 0..n {
                if let Some(m) = display.monitor(i) {
                    let g = m.geometry();
                    if g.x() == px && g.y() == py {
                        return Some(m);
                    }
                }
            }
            tracing::warn!("KDE primary at {px},{py} not matched in GDK monitors");
        }
    }

    // Ultimate fallback: first monitor
    display.monitor(0)
}

/// Find the monitor currently under the pointer cursor.
/// Falls back to primary monitor if cursor position cannot be determined.
fn find_cursor_monitor(display: &gdk::Display) -> Option<gdk::Monitor> {
    use gdk::prelude::{DeviceExt, MonitorExt, SeatExt};

    // On KDE Wayland, GDK3's device.position() doesn't return real global
    // coordinates — they're clamped to the focused surface's monitor.
    // Use KWin D-Bus to get the active output name, then kscreen-doctor
    // to resolve that output's geometry, and match against GDK monitors.
    if gtk_layer_shell::is_supported()
        && let Some(m) = find_cursor_monitor_kde(display)
    {
        return Some(m);
    }

    // GDK path (works on X11 and GNOME Wayland)
    let seat = display.default_seat()?;
    let pointer = seat.pointer()?;
    let (_screen, x, y) = pointer.position();
    tracing::debug!("Cursor position (GDK): ({x}, {y})");

    let monitor = display
        .monitor_at_point(x, y)
        .or_else(|| find_primary_monitor(display));

    if let Some(ref m) = monitor {
        let geom = m.geometry();
        tracing::debug!(
            "Cursor monitor resolved at {},{} ({}x{})",
            geom.x(),
            geom.y(),
            geom.width(),
            geom.height()
        );
    }

    monitor
}

/// KDE Wayland fallback: ask KWin for the active output name via D-Bus,
/// then resolve it to a GDK monitor via kscreen-doctor geometry matching.
fn find_cursor_monitor_kde(display: &gdk::Display) -> Option<gdk::Monitor> {
    use gtk::prelude::MonitorExt;

    // Step 1: Get the active output name from KWin D-Bus
    // org.kde.KWin /KWin org.kde.KWin.activeOutputName
    let output = std::process::Command::new("dbus-send")
        .args([
            "--session",
            "--dest=org.kde.KWin",
            "--print-reply",
            "/KWin",
            "org.kde.KWin.activeOutputName",
        ])
        .output()
        .ok()?;

    let reply = String::from_utf8_lossy(&output.stdout);
    // Reply format: `method return ...\n   string "DP-1"\n`
    let active_name = reply.lines().find_map(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("string \"")
            .and_then(|s| s.strip_suffix('"'))
    })?;

    tracing::debug!("KWin active output: {active_name}");

    // Step 2: Parse kscreen-doctor to find this output's geometry
    let kscreen = std::process::Command::new("kscreen-doctor")
        .arg("-o")
        .output()
        .ok()?;

    let text = String::from_utf8_lossy(&kscreen.stdout);
    let clean = strip_ansi(&text);

    let mut current_is_target = false;
    let mut target_geom: Option<(i32, i32)> = None;

    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Output:") {
            // "Output: 2 HDMI-A-1 ..." — check if this output matches
            current_is_target = trimmed
                .split_whitespace()
                .nth(2)
                .is_some_and(|name| name == active_name);
        } else if current_is_target && trimmed.starts_with("Geometry:") {
            // "Geometry: 1920,0 1920x1080"
            if let Some(coords) = trimmed.strip_prefix("Geometry:").map(|s| s.trim())
                && let Some(pos) = coords.split_whitespace().next()
            {
                let parts: Vec<&str> = pos.split(',').collect();
                if parts.len() == 2
                    && let (Ok(x), Ok(y)) = (parts[0].parse(), parts[1].parse())
                {
                    target_geom = Some((x, y));
                }
            }
            break;
        }
    }

    // Step 3: Match geometry to a GDK monitor
    let (tx, ty) = target_geom?;
    tracing::debug!("KDE cursor monitor '{active_name}' at {tx},{ty}");

    let n = display.n_monitors();
    for i in 0..n {
        if let Some(m) = display.monitor(i) {
            let g = m.geometry();
            if g.x() == tx && g.y() == ty {
                return Some(m);
            }
        }
    }
    tracing::warn!("KDE active output '{active_name}' at {tx},{ty} not matched in GDK monitors");
    None
}

/// Index of a monitor within the display, matched by geometry origin.
/// Needed for fullscreen_on_monitor(), which takes an index rather than a
/// GdkMonitor. Falls back to 0 if not matched.
fn monitor_index(display: &gdk::Display, monitor: &gdk::Monitor) -> i32 {
    use gtk::prelude::MonitorExt;
    let target = monitor.geometry();
    for i in 0..display.n_monitors() {
        if let Some(m) = display.monitor(i) {
            let g = m.geometry();
            if g.x() == target.x() && g.y() == target.y() {
                return i;
            }
        }
    }
    tracing::warn!(
        "monitor_index: monitor at {},{} not found — defaulting to 0",
        target.x(),
        target.y()
    );
    0
}

/// Resolve the target monitor based on the monitor_mode config value.
/// "cursor" → monitor under the pointer; "primary" → primary monitor.
pub fn get_monitor_for_mode(mode: &str) -> Option<gdk::Monitor> {
    let display = gdk::Display::default()?;
    if mode == "cursor" {
        find_cursor_monitor(&display)
    } else {
        find_primary_monitor(&display)
    }
}

/// Reposition the window to the given monitor.
/// On Wayland layer-shell: hides the GTK window, calls set_monitor(), then
/// re-shows it — KWin and some compositors require the surface to be unmapped
/// before a monitor change takes effect.
/// On X11: calls move_() + set_size_request().
pub fn reposition_to_monitor(window: &WebviewWindow, monitor: &gdk::Monitor) {
    use gtk::prelude::{GtkWindowExt, MonitorExt, WidgetExt};

    let gtk_win = match window.gtk_window() {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("reposition_to_monitor: no GTK window: {e}");
            return;
        }
    };

    let geom = monitor.geometry();

    // Check if this window actually has layer-shell initialized (not just
    // whether the compositor supports it). Respects the window_strategy override.
    use gtk_layer_shell::LayerShell;
    if gtk_win.is_layer_window() {
        // Adaptive: wide landscape → 1070px with preview, narrow/portrait → 680px
        const LAUNCHER_W: i32 = 680;
        const PREVIEW_W: i32 = 390;
        const WIDE_SURFACE_W: i32 = LAUNCHER_W + PREVIEW_W;
        let wide_enough = geom.width() >= WIDE_SURFACE_W + 80 && geom.width() > geom.height();

        let top_margin = (geom.height() as f64 * 0.18) as i32;
        let max_height = (geom.height() as f64 * 0.75) as i32;

        // Must unmap → set_monitor → remap for compositor to honour the change
        gtk_win.hide();
        gtk_win.set_monitor(monitor);
        gtk_win.set_layer_shell_margin(gtk_layer_shell::Edge::Top, top_margin);

        if wide_enough {
            gtk_win.set_anchor(gtk_layer_shell::Edge::Left, true);
            gtk_win.set_anchor(gtk_layer_shell::Edge::Right, false);
            let left_margin = (geom.width() - LAUNCHER_W) / 2;
            gtk_win.set_layer_shell_margin(gtk_layer_shell::Edge::Left, left_margin);
            gtk_win.set_size_request(WIDE_SURFACE_W, max_height);
        } else {
            gtk_win.set_anchor(gtk_layer_shell::Edge::Left, false);
            gtk_win.set_anchor(gtk_layer_shell::Edge::Right, false);
            gtk_win.set_layer_shell_margin(gtk_layer_shell::Edge::Left, 0);
            gtk_win.set_size_request(LAUNCHER_W, max_height);
        }
        gtk_win.show();
        tracing::debug!(
            "Repositioned layer-shell to monitor at {},{} ({}x{}, wide: {wide_enough}, top_margin: {top_margin}, max_h: {max_height})",
            geom.x(),
            geom.y(),
            geom.width(),
            geom.height()
        );
    } else if active_strategy() == WindowStrategy::Toplevel {
        // Wayland toplevel: monitor-covering container, CSS handles centering
        gtk_win.set_size_request(geom.width(), geom.height());
        if is_kde_wayland() {
            gtk_win.move_(geom.x(), geom.y());
        } else if wants_fullscreen() {
            // Mutter ignores move_() — retarget via fullscreen-on-output
            // Every other GDK accessor in this file treats "no display/screen"
            // as a condition to handle (`?`, `if let`, `ok_or`); this one
            // panicked. It runs on the GTK main thread during a monitor
            // change, where a screen can genuinely be absent mid-hotplug — and
            // a panic there takes the process down rather than skipping one
            // repositioning. Fall through: the window stays where it is, which
            // is what happens on every other compositor anyway.
            if let (Some(display), Some(screen)) = (
                gdk::Display::default(),
                gtk::prelude::WidgetExt::screen(&gtk_win).or_else(gdk::Screen::default),
            ) {
                gtk_win.fullscreen_on_monitor(&screen, monitor_index(&display, monitor));
            } else {
                tracing::warn!(
                    "[window] no GDK screen while repositioning — leaving the window in place"
                );
            }
        }
        tracing::debug!(
            "Repositioned Wayland toplevel to {},{} ({}x{})",
            geom.x(),
            geom.y(),
            geom.width(),
            geom.height()
        );
    } else if screen_composited() {
        // X11 fullscreen overlay
        gtk_win.move_(geom.x(), geom.y());
        gtk_win.set_size_request(geom.width(), geom.height());
        tracing::debug!(
            "Repositioned X11 window to monitor at {},{} ({}x{})",
            geom.x(),
            geom.y(),
            geom.width(),
            geom.height()
        );
    } else {
        // X11 compact opaque window (no compositor) — keep width, retarget
        // position; frontend owns the height.
        gtk_win.move_(compact_x(&geom), compact_y(&geom));
        tracing::debug!(
            "Repositioned X11 compact window to monitor at {},{}",
            geom.x(),
            geom.y()
        );
    }
}

/// Configure the GTK window for Wayland layer-shell, KDE toplevel, or X11 skip-taskbar.
/// `strategy`: "auto" (detect), "layer-shell" (force), "toplevel" (force KDE), or "x11" (force).
pub fn init_window(window: &WebviewWindow, strategy: &str) {
    let gtk_win = match window.gtk_window() {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("No GTK window: {e}");
            return;
        }
    };

    use gtk::prelude::{GtkWindowExt, MonitorExt, WidgetExt, WidgetExtManual};

    let resolved = resolve_strategy(strategy);
    let _ = ACTIVE_STRATEGY.set(resolved);
    let _ = TOPLEVEL_PLAIN.set(strategy == "toplevel-window");
    tracing::info!(
        "Window strategy resolved: {} (configured: {strategy})",
        resolved.as_str()
    );

    // Gracefully handle missing screen/monitor instead of panicking
    let setup_ok = (|| -> Option<()> {
        let screen = WidgetExt::screen(&gtk_win)?;
        let display = screen.display();
        let monitor = find_primary_monitor(&display)?;
        let geom = monitor.geometry();

        // Ensure the window is transparent
        if let Some(visual) = screen.rgba_visual() {
            gtk_win.set_visual(Some(&visual));
        }
        gtk_win.set_app_paintable(true);

        if resolved == WindowStrategy::LayerShell {
            use gtk_layer_shell::LayerShell;
            gtk_win.hide(); // Must unmap before init_layer_shell
            gtk_win.init_layer_shell();
            gtk_win.set_layer(gtk_layer_shell::Layer::Overlay);
            gtk_win.set_keyboard_mode(gtk_layer_shell::KeyboardMode::OnDemand);
            // Pin to the primary monitor so it doesn't drift to other displays
            gtk_win.set_monitor(&monitor);
            // Adaptive surface sizing: wide monitors get side-by-side preview,
            // narrow/portrait monitors get launcher-only (preview hidden by CSS).
            // NOT fullscreen: avoids compositor blur/dim (I-001).
            const LAUNCHER_W: i32 = 680;
            const PREVIEW_W: i32 = 390; // 10 gap + 380 panel
            const WIDE_SURFACE_W: i32 = LAUNCHER_W + PREVIEW_W; // 1070
            // Need enough room for the surface + some breathing space (40px each side)
            let wide_enough = geom.width() >= WIDE_SURFACE_W + 80 && geom.width() > geom.height(); // landscape only

            gtk_win.set_anchor(gtk_layer_shell::Edge::Top, true);
            gtk_win.set_anchor(gtk_layer_shell::Edge::Bottom, false);
            let top_margin = (geom.height() as f64 * 0.18) as i32;
            gtk_win.set_layer_shell_margin(gtk_layer_shell::Edge::Top, top_margin);
            let max_height = (geom.height() as f64 * 0.75) as i32;

            if wide_enough {
                // Wide landscape: Left anchor + manual centering of 680px bar
                gtk_win.set_anchor(gtk_layer_shell::Edge::Left, true);
                gtk_win.set_anchor(gtk_layer_shell::Edge::Right, false);
                let left_margin = (geom.width() - LAUNCHER_W) / 2;
                gtk_win.set_layer_shell_margin(gtk_layer_shell::Edge::Left, left_margin);
                gtk_win.set_size_request(WIDE_SURFACE_W, max_height);
            } else {
                // Narrow or portrait: Top-only anchor, compositor centers 680px
                gtk_win.set_anchor(gtk_layer_shell::Edge::Left, false);
                gtk_win.set_anchor(gtk_layer_shell::Edge::Right, false);
                gtk_win.set_size_request(LAUNCHER_W, max_height);
            }
            gtk_win.set_namespace("lychi");
            tracing::debug!(
                "Layer shell initialized (strategy: {strategy}, top_margin: {top_margin}, max_h: {max_height})"
            );
        } else if resolved == WindowStrategy::Toplevel {
            // Wayland toplevel: monitor-covering transparent container — CSS
            // handles centering. Wayland xdg_toplevel ignores move_(), so we
            // can't position a sized window; cover the monitor instead.
            // NOTE: set_skip_taskbar_hint doesn't work on Wayland — it only sets
            // X11 atoms. This is a known Tauri limitation (tauri#9829). The window
            // will appear in the taskbar. We set Utility hint so it at least
            // behaves as a tool window (no alt-tab, stays above, etc).
            gtk_win.set_type_hint(gdk::WindowTypeHint::Utility);
            gtk_win.set_skip_taskbar_hint(true);
            gtk_win.set_skip_pager_hint(true);
            gtk_win.set_decorated(false);
            gtk_win.set_keep_above(true);
            gtk_win.set_size_request(geom.width(), geom.height());

            if is_kde_wayland() {
                // KWin honours move_() here, and true fullscreen would
                // re-trigger compositor blur/dim effects (I-001).
                gtk_win.move_(geom.x(), geom.y());
                // Hide from taskbar/Alt-Tab via KWin's plasma-shell protocol
                // (I-009: the X11 skip hints above are no-ops on Wayland).
                // Re-applied on every map — GTK recreates the wl_surface each
                // time the window is re-shown.
                gtk_win.connect_map_event(|win, _| {
                    if let Err(e) = crate::platform::kde_taskbar::hide_from_taskbar(win) {
                        tracing::warn!("[plasma-shell] skip-taskbar failed: {e}");
                    } else {
                        tracing::debug!("[plasma-shell] skip-taskbar applied");
                    }
                    glib::Propagation::Proceed
                });
                gtk_win.connect_unmap_event(|_, _| {
                    crate::platform::kde_taskbar::mark_unmapped();
                    glib::Propagation::Proceed
                });
            } else if wants_fullscreen() {
                // Unknown Wayland compositors: move_() is a no-op on Mutter and
                // friends, so fullscreen-on-output is the only sanctioned way to
                // pin the surface to a specific monitor. Deliberately NOT taken
                // on GNOME — see wants_fullscreen().
                gtk_win.fullscreen_on_monitor(&screen, monitor_index(&display, &monitor));
            }
            tracing::debug!(
                "Toplevel window: {}x{} at {},{} (strategy: {strategy}, kde: {})",
                geom.width(),
                geom.height(),
                geom.x(),
                geom.y(),
                is_kde_wayland()
            );
        } else {
            // X11 path
            let composited = screen.is_composited();
            let _ = SCREEN_COMPOSITED.set(composited);
            if composited {
                // Fullscreen transparent overlay on primary monitor
                gtk_win.move_(geom.x(), geom.y());
                gtk_win.set_size_request(geom.width(), geom.height());
            } else {
                // No compositor (xfwm4/Marco with compositing off): ARGB
                // transparency renders black, so a fullscreen overlay would
                // blank the desktop. Fall back to a rofi-style compact
                // opaque window; the frontend resizes it to content height.
                tracing::warn!(
                    "X11: no compositor detected — using compact opaque window (rofi-style fallback)"
                );
                gtk_win.set_type_hint(gdk::WindowTypeHint::Utility);
                gtk_win.set_keep_above(true);
                gtk_win.resize(COMPACT_W, COMPACT_INITIAL_H);
                gtk_win.move_(compact_x(&geom), compact_y(&geom));
            }
            gtk_win.set_skip_taskbar_hint(true);
            gtk_win.set_skip_pager_hint(true);
            tracing::debug!(
                "X11 window hints applied (strategy: {strategy}, composited: {composited})"
            );
        }
        Some(())
    })();

    if setup_ok.is_none() {
        tracing::error!("No GDK screen or monitor available — skipping window hints");
    }
}

/// Focus and present the window using GTK APIs.
pub fn focus_window(window: &WebviewWindow) {
    if let Ok(gtk_win) = window.gtk_window() {
        use gtk::prelude::GtkWindowExt;
        gtk_win.set_keep_above(true);
        gtk_win.present();
        tracing::debug!(
            "focus_window: is_active={}, has_toplevel_focus={}",
            gtk_win.is_active(),
            gtk_win.has_toplevel_focus()
        );
    }
}

/// Set up interaction-gated dismiss-on-blur.
///
/// Works on both layer-shell (wlroots) and Wayland toplevel windows (KDE/GNOME).
/// On layer-shell: also switches keyboard mode to OnDemand on first interaction.
/// On X11 fullscreen: returns early (frontend handles dismiss via click-on-backdrop).
///
///   show_window() → dismiss_armed=false
///   key-press / button-press inside window → dismiss_armed=true
///   focus-out → reported to the launcher state machine as
///     `FocusOut { focus_lost, interacted }`; it dismisses only when BOTH hold.
///     `focus_lost` (the protocol's FOCUSED bit) filters GTK noise; `interacted`
///     (armed in THIS summon cycle) filters focus theft — on GNOME something
///     genuinely takes focus at keys=0 shortly after show (unidentified; see
///     `launcher_state::Event::FocusOut`), and without the arming gate every
///     summon flashed and vanished.
pub fn setup_dismiss_on_blur(
    window: &WebviewWindow,
    dismiss_armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    summon_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    armed_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    agent_busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let gtk_win = match window.gtk_window() {
        Ok(w) => w,
        Err(_) => return,
    };

    // Installed on every strategy. Layer-shell/toplevel need it because the
    // frontend can't see compositor focus; X11 needs it so Alt-Tab away
    // dismisses (backdrop clicks land inside our fullscreen overlay, but
    // focus loss to another window is only visible at the GTK level).
    use gtk_layer_shell::LayerShell;
    let is_layer = gtk_win.is_layer_window();
    let is_toplevel = !is_layer && active_strategy() == WindowStrategy::Toplevel;

    use gtk::prelude::WidgetExt;
    use std::sync::atomic::Ordering;

    // How long a would-be-dismissing focus-out waits for focus to return
    // before it commits, on Wayland-toplevel. Long enough to absorb Mutter's
    // spurious focus flicker and a portal/IM dialog handing focus back (both
    // observed to bounce within a frame or two, well under this); short enough
    // that a genuine click-away still feels instant. Only pays on GNOME-style
    // toplevel — layer-shell and X11 dismiss immediately.
    const GRACE_MS: u64 = 150;

    // Session counters, shared by the handlers below purely for diagnostics —
    // nothing branches on them. They exist so the log answers "how many focus
    // events did this window get, and how many keys did the user actually
    // type?" directly, instead of leaving it to be inferred from timestamps.
    let keypress_count = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let focusout_count = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let focusin_count = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));

    // The "did focus come BACK?" signal. A spurious Mutter focus-out is
    // followed within a frame by a focus-in; a genuine switch-away is not.
    // The dismiss handler snapshots this counter and, after a short grace
    // window, dismisses only if it has not advanced — so a focus flicker the
    // compositor invented (or the portal/IM dialog briefly stealing focus, as
    // in Berin's launch log) can no longer close the launcher. GTK #1395 /
    // #1871: focus-out on Wayland/Mutter is documented-unreliable, so a single
    // one can never be trusted as "the user left" without this confirmation.
    let focusin_generation = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));

    // Set once the window has actually held focus this cycle. A key-press that
    // arrives BEFORE focus-in is not the user typing into the launcher — it is
    // stray input from the compositor/IM layer (Berin's log: `key-press #1`
    // with no typing, while an input-method context and a portal dialog were
    // active). Arming on it let the next spurious focus-out dismiss. Arming is
    // now gated on this, so a pre-focus phantom key cannot arm the gate.
    let has_focused = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // focus-IN: promotes Showing -> Visible, bumps the return-generation, and
    // records that focus has been held this cycle.
    {
        let ins = focusin_count.clone();
        let generation = focusin_generation.clone();
        let focused = has_focused.clone();
        let seq = summon_seq.clone();
        let handle = window.app_handle().clone();
        gtk_win.connect_focus_in_event(move |_, _| {
            let n = ins.fetch_add(1, Ordering::SeqCst) + 1;
            generation.fetch_add(1, Ordering::SeqCst);
            focused.store(true, Ordering::SeqCst);
            let s = seq.load(Ordering::SeqCst);
            // Focus-in is what promotes Showing -> Visible. Until it arrives,
            // no focus-out can be a dismiss, because focus was never held.
            handle.state::<crate::state::AppState>().launcher.apply(
                crate::launcher_state::Event::FocusIn,
                &format!("focus-IN #{n} seq={s}"),
            );
            glib::Propagation::Proceed
        });
    }

    // key-press: user typed → arm dismiss, stamped with the current summon
    // cycle (Escape handled separately)
    {
        let armed = dismiss_armed.clone();
        let seq = summon_seq.clone();
        let stamp = armed_seq.clone();
        let keys = keypress_count.clone();
        let has_focused_key = has_focused.clone();
        gtk_win.connect_key_press_event(move |_, event| {
            use gdk::keys::constants as key;
            let keyval = event.keyval();
            // EVERY keystroke is counted, and the count is logged. The old
            // handler logged only the FIRST key (the `!armed` guard swallowed
            // the rest), which is why a report of "it closes after about three
            // characters" could not be checked against the log at all: there
            // was one key-press line and no way to tell if a second key ever
            // arrived. The count is the difference between reading a user's
            // recollection and reading what happened.
            let n = keys.fetch_add(1, Ordering::SeqCst) + 1;
            // Only a key that lands AFTER the window has held focus counts as
            // the user typing into the launcher. A key-press before focus-in
            // is stray compositor/IM input (see `has_focused`), and arming on
            // it is what let Berin's launcher self-dismiss.
            let focused = has_focused_key.load(Ordering::SeqCst);
            if keyval != key::Escape && focused && !armed.load(Ordering::SeqCst) {
                let current = seq.load(Ordering::SeqCst);
                stamp.store(current, Ordering::SeqCst);
                armed.store(true, Ordering::SeqCst);
                tracing::info!("[dismiss] seq={current} key-press #{n} → armed=true");
            } else if !focused {
                tracing::info!("[dismiss] key-press #{n} IGNORED (before focus-in — stray input)");
            } else {
                tracing::debug!("[dismiss] key-press #{n} (already armed)");
            }
            glib::Propagation::Proceed // let key propagate to WebView
        });
    }

    // button-press: user clicked inside → arm dismiss, stamped with the cycle
    {
        let armed = dismiss_armed.clone();
        let seq = summon_seq.clone();
        let stamp = armed_seq.clone();
        gtk_win.connect_button_press_event(move |_, _| {
            if !armed.load(Ordering::SeqCst) {
                let current = seq.load(Ordering::SeqCst);
                stamp.store(current, Ordering::SeqCst);
                armed.store(true, Ordering::SeqCst);
                tracing::info!("[dismiss] seq={current} button-press → armed=true");
            }
            glib::Propagation::Proceed
        });
    }

    // focus-out: only dismiss if the user interacted (armed) IN THIS summon
    // cycle — a stale focus-out arriving after a re-summon must not close
    // the fresh window.
    let handle = window.app_handle().clone();
    let blurs = focusout_count.clone();
    let keys_seen = keypress_count.clone();
    let focusin_gen_out = focusin_generation.clone();
    let summon_seq_out = summon_seq.clone();
    let has_focused_out = has_focused.clone();
    let agent_busy = agent_busy.clone();
    let started = std::time::Instant::now();
    gtk_win.connect_focus_out_event(move |w, _| {
        let seq = summon_seq.load(Ordering::SeqCst);

        // Diagnostics attached to EVERY focus-out, dismissing or not.
        //
        // A tester on GNOME/Wayland reported the launcher "crashing" when he
        // typed. It had not crashed — it dismissed. His log showed the decision
        // and nothing else, so the interesting facts had to be reconstructed by
        // hand from timestamps: that his window emitted THREE focus-outs before
        // he ever pressed a key, i.e. that on Mutter this window bleeds focus
        // roughly twice a second unprompted, and the dismissing focus-out was
        // simply the first one to arrive after arming.
        //
        // That is the fact that reframes the bug, and it should be legible
        // without arithmetic. So: how many focus-outs this window has seen, how
        // many keys the user actually typed, and how long the window had been up.
        let n = blurs.fetch_add(1, Ordering::SeqCst) + 1;
        let keys = keys_seen.load(Ordering::SeqCst);
        let up_ms = started.elapsed().as_millis();
        // Focus is no longer held: the next arming key-press must wait for a
        // fresh focus-in. Cheap and correct across hide→show cycles — arming
        // is a one-way latch, so this only matters for the not-yet-armed case
        // that Berin hit (stray key before this cycle's focus-in).
        has_focused_out.store(false, Ordering::SeqCst);
        use gtk::prelude::{GtkWindowExt, WidgetExt};
        let is_active = w.is_active();
        let has_focus = w.has_toplevel_focus();

        // THE AUTHORITATIVE SIGNAL: GdkWindowState::FOCUSED.
        //
        // GDK sets this bit straight from the Wayland `wl_keyboard` enter/leave
        // events, so it is the protocol's own notion of keyboard focus rather
        // than a GTK-level approximation of it. Measured on KDE Wayland, it is
        // exactly inverted from the two properties tried before it:
        //
        //   FOCUS-IN   is_active=false toplevel_focus=false  FOCUSED=true
        //   FOCUS-OUT  is_active=true  toplevel_focus=true   FOCUSED=false
        //
        // That inversion is why `is_active()`/`has_toplevel_focus()` failed:
        // on Wayland they lag by one event, so at focus-out time they still
        // describe the PREVIOUS state. A predicate built on them reads `true`
        // on every focus-out — including a genuine click-away — which silently
        // disables dismiss-on-blur rather than fixing anything.
        //
        // This also replaces a 1200ms "wait and see if focus comes back" timer.
        // That worked, but it inferred from behaviour what the protocol states
        // outright, and it made every genuine dismiss lag by over a second.
        let focus_lost = w
            .window()
            .map(|gw| !gw.state().contains(gdk::WindowState::FOCUSED))
            .unwrap_or(false);

        // Armed AND stamped with THIS summon cycle — interaction from a
        // previous cycle must not license a dismiss of the fresh window.
        let interacted =
            dismiss_armed.load(Ordering::SeqCst) && armed_seq.load(Ordering::SeqCst) == seq;

        // `is_active`/`toplevel_focus` stay in the log — not as inputs, but so a
        // future report can be checked against the inversion documented above
        // rather than re-derived from scratch.
        let ctx = format!(
            "blur#{n} keys={keys} up={up_ms}ms focus_lost={focus_lost} interacted={interacted} \
             (is_active={is_active} toplevel_focus={has_focus}) visible={}",
            w.is_visible()
        );

        // A focus-out that WOULD dismiss is not acted on immediately on
        // Wayland-toplevel (GNOME/Mutter): the compositor emits spurious
        // focus-outs, and a real one caused by the portal/IM dialog is
        // followed within a frame by focus returning. So the machine is only
        // told to dismiss once a short grace window confirms focus did NOT
        // come back. On layer-shell and X11, where focus-out is trustworthy,
        // the decision is immediate — nothing changes there.
        //
        // Crucial ordering: the state machine is NOT stepped here for the
        // dismissing case, because stepping it flips Visible -> Hiding and a
        // deferred cancel would strand it. Instead we peek at what it WOULD do
        // with the current state, and only commit the transition after the
        // grace window.
        let would_dismiss = matches!(
            handle.state::<crate::state::AppState>().launcher.peek(
                crate::launcher_state::Event::FocusOut {
                    focus_lost,
                    interacted,
                }
            ),
            crate::launcher_state::Action::EmitDismiss
        );

        if !would_dismiss {
            // Non-dismissing focus-out: report to the machine as before so it
            // keeps its books (Showing/Hiding bookkeeping, logging).
            handle.state::<crate::state::AppState>().launcher.apply(
                crate::launcher_state::Event::FocusOut {
                    focus_lost,
                    interacted,
                },
                &format!("{ctx} seq={seq}"),
            );
            return glib::Propagation::Proceed;
        }

        // While an AI agent run is in flight, the launcher does not self-dismiss
        // on focus loss. A running agent can trigger any focus-stealing external
        // window — a spawned terminal, a `pkexec`/polkit password dialog for a
        // package install, a file picker — each a genuine `focus_lost &&
        // interacted` that would otherwise dismiss the chat out from under the
        // user (and take a pending approval prompt with it). The guard is raised
        // when the run starts driving and lowered when it resolves. We do NOT
        // step the machine: the launcher legitimately stays `Visible`, so no
        // deferred cancel can strand it. This does NOT touch the ordinary "launch
        // an app, launcher hides" path — it applies only while the agent is
        // working, and Escape still dismisses deliberately.
        //
        // Deliberately LEVEL-triggered, not one-shot: Mutter bleeds spurious
        // focus-outs several times a second (see the diagnostics note above), so
        // consuming the guard on the first focus-out would risk spending it on a
        // spurious one and letting the real focus theft dismiss the launcher.
        // The cost is that while the agent is working a deliberate click-away
        // also won't dismiss; the flag clears the instant the run resolves.
        if agent_busy.load(Ordering::SeqCst) {
            tracing::info!("[dismiss] seq={seq} focus-out suppressed — agent run in flight  {ctx}");
            return glib::Propagation::Proceed;
        }

        let emit_dismiss = {
            let handle = handle.clone();
            let dismiss_armed = dismiss_armed.clone();
            move |ctx: String, seq: u64| {
                let action = handle.state::<crate::state::AppState>().launcher.apply(
                    crate::launcher_state::Event::FocusOut {
                        focus_lost: true,
                        interacted: true,
                    },
                    &format!("{ctx} seq={seq}"),
                );
                if matches!(action, crate::launcher_state::Action::EmitDismiss) {
                    tracing::info!("[dismiss] seq={seq} → DISMISS  {ctx}");
                    dismiss_armed.store(false, Ordering::SeqCst);
                    use tauri::Emitter;
                    let _ = handle.emit("lychi://dismiss", ());
                }
            }
        };

        if !is_toplevel {
            // Layer-shell / X11: focus-out is trustworthy, dismiss now.
            emit_dismiss(ctx, seq);
            return glib::Propagation::Proceed;
        }

        // Wayland-toplevel: defer and confirm focus did not return.
        let gen_at_blur = focusin_gen_out.load(Ordering::SeqCst);
        let gen_check = focusin_gen_out.clone();
        let seq_at_blur = seq;
        let seq_now = summon_seq_out.clone();
        tracing::info!(
            "[dismiss] seq={seq} focus-out would dismiss — {GRACE_MS}ms re-focus grace  {ctx}"
        );
        glib::timeout_add_local_once(std::time::Duration::from_millis(GRACE_MS), move || {
            // Focus came back (spurious flicker or reclaimed dialog focus)?
            // The generation advanced → cancel.
            if gen_check.load(Ordering::SeqCst) != gen_at_blur {
                tracing::info!(
                    "[dismiss] seq={seq_at_blur} focus returned within grace — NOT dismissing"
                );
                return;
            }
            // A newer summon started meanwhile? This blur is stale.
            if seq_now.load(Ordering::SeqCst) != seq_at_blur {
                tracing::info!(
                    "[dismiss] seq={seq_at_blur} superseded during grace — NOT dismissing"
                );
                return;
            }
            emit_dismiss(format!("(after {GRACE_MS}ms grace) {ctx}"), seq_at_blur);
        });
        glib::Propagation::Proceed
    });
    tracing::info!(
        "[dismiss] interaction-gated dismiss handler installed (layer={is_layer}, toplevel={is_toplevel})"
    );
}

/// GTK-level Escape key handler — catches Escape even when the WebView
/// input doesn't have DOM focus. Emits `lychi://gtk-escape` for the frontend.
/// Active on layer-shell and Wayland toplevel (X11 handles Escape via fullscreen overlay click).
pub fn setup_escape_handler(window: &WebviewWindow) {
    let gtk_win = match window.gtk_window() {
        Ok(w) => w,
        Err(_) => return,
    };

    use gtk_layer_shell::LayerShell;
    let is_layer = gtk_win.is_layer_window();
    if !is_layer && active_strategy() != WindowStrategy::Toplevel {
        return; // X11 fullscreen — frontend DOM handles Escape
    }

    let handle = window.app_handle().clone();
    use gtk::prelude::WidgetExt;
    gtk_win.connect_key_press_event(move |_, event| {
        use gdk::keys::constants as key;
        let keyval = event.keyval();
        let state = event.state();
        // Bare Escape (no modifiers) → dismiss
        let no_mods = !state.contains(gdk::ModifierType::CONTROL_MASK)
            && !state.contains(gdk::ModifierType::MOD1_MASK)
            && !state.contains(gdk::ModifierType::SHIFT_MASK)
            && !state.contains(gdk::ModifierType::SUPER_MASK);
        if keyval == key::Escape && no_mods {
            tracing::info!("GTK Escape handler: emitting lychi://gtk-escape");
            use tauri::Emitter;
            let _ = handle.emit("lychi://gtk-escape", ());
        }
        glib::Propagation::Proceed // Let it propagate to WebView too
    });
    tracing::debug!("GTK Escape key handler installed");
}

/// IPC socket path using XDG_RUNTIME_DIR.
pub fn ipc_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/tmp/lychi-{}", unsafe { libc::getuid() }));
    PathBuf::from(runtime_dir).join("lychi.sock")
}

/// Open a URI with Wayland activation tokens via GDK AppLaunchContext.
///
/// Every URI-opening path funnels here, so this is where the central URI-scheme
/// decider is enforced: a `javascript:`/`data:`/unknown-scheme URI (e.g. from a
/// browser bookmark) is refused before it reaches the desktop's default handler.
pub async fn open_uri(uri: &str) -> Result<(), String> {
    lychi_core::rules::uri::check_uri(uri)?;
    let uri = uri.to_string();
    let (tx, rx) = oneshot::channel::<Result<(), String>>();

    glib::MainContext::default().invoke(move || {
        let result = (|| {
            let display = gdk::Display::default().ok_or("No GDK display")?;
            let context = display.app_launch_context().ok_or("No AppLaunchContext")?;
            gio::AppInfo::launch_default_for_uri(&uri, Some(&context))
                .map_err(|e| format!("Failed to open URI: {e}"))
        })();
        let _ = tx.send(result);
    });

    rx.await.map_err(|_| "Channel closed".to_string())?
}

/// Record a hotkey combo by capturing the next modifier+key press via GTK.
/// Returns the combo string (e.g. "Super+Space") or an error.
pub async fn record_hotkey(app: &tauri::AppHandle) -> Result<String, String> {
    let (tx, rx) = oneshot::channel::<Result<String, String>>();
    let app_for_glib = app.clone();

    glib::MainContext::default().invoke(move || {
        use gdk::keys::constants as key;
        use gtk::prelude::*;
        use std::sync::{Arc, Mutex};

        let win = match app_for_glib.get_webview_window("main") {
            Some(w) => w,
            None => {
                let _ = tx.send(Err("No main window".into()));
                return;
            }
        };
        let gtk_win = match win.gtk_window() {
            Ok(w) => w,
            Err(e) => {
                let _ = tx.send(Err(e.to_string()));
                return;
            }
        };

        let tx = Arc::new(Mutex::new(Some(tx)));
        let handler_id: Arc<Mutex<Option<glib::SignalHandlerId>>> = Arc::new(Mutex::new(None));

        let tx_clone = tx.clone();
        let handler_id_clone = handler_id.clone();
        let gtk_win_ref = gtk_win.clone();

        let id = gtk_win.connect_key_press_event(move |_, ev| {
            let keyval = ev.keyval();

            // Ignore pure modifier presses
            if matches!(
                keyval,
                key::Control_L
                    | key::Control_R
                    | key::Alt_L
                    | key::Alt_R
                    | key::Shift_L
                    | key::Shift_R
                    | key::Super_L
                    | key::Super_R
                    | key::Meta_L
                    | key::Meta_R
                    | key::Hyper_L
                    | key::Hyper_R
                    | key::ISO_Level3_Shift
            ) {
                return glib::Propagation::Stop;
            }

            let state = ev.state();
            let mut parts = Vec::new();
            if state.contains(gdk::ModifierType::CONTROL_MASK) {
                parts.push("Ctrl");
            }
            if state.contains(gdk::ModifierType::MOD1_MASK) {
                parts.push("Alt");
            }
            if state.contains(gdk::ModifierType::SHIFT_MASK) {
                parts.push("Shift");
            }
            if state.contains(gdk::ModifierType::SUPER_MASK)
                || state.contains(gdk::ModifierType::MOD4_MASK)
            {
                parts.push("Super");
            }

            // Escape without modifiers = cancel
            if keyval == key::Escape && parts.is_empty() {
                if let Some(id) = handler_id_clone.lock().ok().and_then(|mut g| g.take()) {
                    gtk_win_ref.disconnect(id);
                }
                if let Some(tx) = tx_clone.lock().ok().and_then(|mut g| g.take()) {
                    let _ = tx.send(Err("Cancelled".into()));
                }
                return glib::Propagation::Stop;
            }

            // Require at least one modifier
            if parts.is_empty() {
                return glib::Propagation::Stop;
            }

            // Map the key name
            let key_name = gdk_keyval_to_tauri_name(keyval);
            if key_name.is_empty() {
                return glib::Propagation::Stop;
            }

            parts.push(&key_name);
            let combo = parts.join("+");

            // Disconnect this handler — one-shot capture complete
            if let Some(id) = handler_id_clone.lock().ok().and_then(|mut g| g.take()) {
                gtk_win_ref.disconnect(id);
            }

            if let Some(tx) = tx_clone.lock().ok().and_then(|mut g| g.take()) {
                let _ = tx.send(Ok(combo));
            }

            glib::Propagation::Stop
        });

        // Store the handler ID so the closure can disconnect itself
        if let Ok(mut guard) = handler_id.lock() {
            *guard = Some(id);
        }
    });

    rx.await.map_err(|_| "Recording cancelled".to_string())?
}

/// Convert a GDK keyval to the Tauri shortcut key name.
fn gdk_keyval_to_tauri_name(keyval: gdk::keys::Key) -> String {
    use gdk::keys::constants as key;
    match keyval {
        key::space => "Space".into(),
        key::Return | key::KP_Enter => "Enter".into(),
        key::Tab | key::ISO_Left_Tab => "Tab".into(),
        key::BackSpace => "Backspace".into(),
        key::Delete | key::KP_Delete => "Delete".into(),
        key::Up | key::KP_Up => "Up".into(),
        key::Down | key::KP_Down => "Down".into(),
        key::Left | key::KP_Left => "Left".into(),
        key::Right | key::KP_Right => "Right".into(),
        key::Home | key::KP_Home => "Home".into(),
        key::End | key::KP_End => "End".into(),
        key::Page_Up | key::KP_Page_Up => "PageUp".into(),
        key::Page_Down | key::KP_Page_Down => "PageDown".into(),
        key::Insert | key::KP_Insert => "Insert".into(),
        key::F1 => "F1".into(),
        key::F2 => "F2".into(),
        key::F3 => "F3".into(),
        key::F4 => "F4".into(),
        key::F5 => "F5".into(),
        key::F6 => "F6".into(),
        key::F7 => "F7".into(),
        key::F8 => "F8".into(),
        key::F9 => "F9".into(),
        key::F10 => "F10".into(),
        key::F11 => "F11".into(),
        key::F12 => "F12".into(),
        _ => {
            // Try to get a printable name (letters, digits, symbols)
            if let Some(ch) = keyval.to_unicode()
                && (ch.is_alphanumeric() || ch.is_ascii_punctuation())
            {
                return ch.to_uppercase().to_string();
            }
            // Fall back to GDK key name
            keyval.name().map(|n| n.to_string()).unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod strategy_tests {
    use super::*;

    fn facts(wayland: bool, kde: bool, layer_shell: bool) -> SessionFacts {
        SessionFacts {
            wayland,
            kde,
            layer_shell,
        }
    }

    /// The desktop facts must come from the core session/compositor deciders,
    /// never from a private env parse in this file.
    ///
    /// Two copies of that parse have now been buried here. The first read only
    /// `XDG_SESSION_TYPE` (absent under autostart — the `wayland` fact broke).
    /// The second, `desktop_contains`, read `XDG_SESSION_DESKTOP` first — a
    /// session *file name* (`plasma`), not a desktop name — so the `kde` fact
    /// broke on every display manager that exports the file name, and KDE
    /// Wayland resolved to LayerShell: unable to type (I-008). Both were
    /// invisible on a dev box whose environment happens to spell the answer
    /// correctly, which is exactly why a source scan and not a behaviour test.
    #[test]
    fn desktop_env_parsing_stays_in_the_session_decider() {
        let src = include_str!("linux.rs");
        for var in [
            "XDG_SESSION_DESKTOP",
            "XDG_CURRENT_DESKTOP",
            "XDG_SESSION_TYPE",
        ] {
            assert!(
                !src.contains(&format!("var(\"{var}\")")),
                "platform/linux.rs reads {var} directly — derive the fact from \
                 lychi_core::context::session / compositor() instead (the env \
                 semantics are subtle and already solved there, once)"
            );
        }
    }

    /// The autostart bug, as a test.
    ///
    /// KDE Wayland with a working layer-shell. If the Wayland fact is WRONG
    /// (which it was, whenever `XDG_SESSION_TYPE` was absent — routine under
    /// autostart), auto-mode selects LayerShell on KWin: the configuration
    /// I-008 says cannot receive keyboard focus. The launcher comes up unable
    /// to type, only when started at login, which is the hardest way to notice.
    #[test]
    fn kde_wayland_never_gets_layer_shell() {
        assert_eq!(
            decide_strategy("auto", facts(true, true, true)),
            WindowStrategy::Toplevel,
            "KDE Wayland must never resolve to layer-shell (I-008)"
        );
        // The same session with the Wayland fact broken — what the bug produced.
        assert_eq!(
            decide_strategy("auto", facts(false, true, true)),
            WindowStrategy::LayerShell,
            "documents the failure mode: a wrong `wayland` fact routes KDE to \
             the broken strategy, which is why is_wayland_session() must use \
             the WAYLAND_DISPLAY fallback"
        );
    }

    #[test]
    fn wlroots_gets_layer_shell() {
        // Not KDE, layer-shell present → the strategy it was built for.
        assert_eq!(
            decide_strategy("auto", facts(true, false, true)),
            WindowStrategy::LayerShell
        );
    }

    #[test]
    fn gnome_wayland_gets_toplevel() {
        // Mutter offers no layer-shell.
        assert_eq!(
            decide_strategy("auto", facts(true, false, false)),
            WindowStrategy::Toplevel
        );
    }

    #[test]
    fn x11_session_gets_x11() {
        assert_eq!(
            decide_strategy("auto", facts(false, false, false)),
            WindowStrategy::X11
        );
        assert_eq!(
            decide_strategy("auto", facts(false, true, false)),
            WindowStrategy::X11,
            "KDE on X11 is still X11"
        );
    }

    /// An explicit choice is honored, except where the compositor cannot.
    #[test]
    fn explicit_strategies_are_honored() {
        for f in [
            facts(true, true, true),
            facts(false, false, false),
            facts(true, false, true),
        ] {
            assert_eq!(decide_strategy("toplevel", f), WindowStrategy::Toplevel);
            assert_eq!(decide_strategy("x11", f), WindowStrategy::X11);
        }
        // Requested but unsupported degrades by session type, never to itself.
        assert_eq!(
            decide_strategy("layer-shell", facts(true, false, false)),
            WindowStrategy::Toplevel
        );
        assert_eq!(
            decide_strategy("layer-shell", facts(false, false, false)),
            WindowStrategy::X11
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression this guards: asking Mutter for fullscreen makes it paint
    /// an opaque black backdrop, so the transparent monitor-covering window
    /// that the whole toplevel design depends on renders as a solid panel.
    #[test]
    fn gnome_wayland_never_requests_fullscreen() {
        use lychi_core::context::session::Desktop;
        for chain in [
            vec![Desktop::Gnome],
            vec![Desktop::Other, Desktop::Gnome], // "ubuntu:GNOME"
            vec![Desktop::Unity],
            vec![Desktop::Pantheon],
            vec![Desktop::Budgie, Desktop::Gnome],
        ] {
            assert!(
                is_mutter_family(true, &chain),
                "{chain:?} should be detected as Mutter-based"
            );
        }
    }

    /// Other Wayland compositors still need fullscreen-on-output: it's the only
    /// sanctioned way to pin a surface to a monitor when move_() is ignored.
    #[test]
    fn unknown_wayland_still_requests_fullscreen() {
        use lychi_core::context::session::Desktop;
        assert!(!is_mutter_family(true, &[Desktop::Sway]));
        assert!(!is_mutter_family(true, &[Desktop::Hyprland]));
    }

    /// Mutter's black-fullscreen behaviour is in its WAYLAND compositing path,
    /// so an X11 GNOME session must not take the workaround.
    ///
    /// Note this is why `GNOME-Classic`/`GNOME-Flashback` never reach here:
    /// both are X11-only sessions. Matching them by name in a Wayland-gated
    /// check (as the previous version did) was unreachable code that read like
    /// deliberate coverage.
    #[test]
    fn gnome_on_x11_is_not_treated_as_mutter_wayland() {
        use lychi_core::context::session::Desktop;
        assert!(!is_mutter_family(false, &[Desktop::Gnome]));
        assert!(!is_mutter_family(
            false,
            &[Desktop::GnomeFlashback, Desktop::Gnome]
        ));
    }

    /// The compound-name case, now handled by the spec'd chain rather than by
    /// substring matching: "GNOME-Flashback:GNOME" parses to two components and
    /// either one identifies the family.
    #[test]
    fn detection_is_case_insensitive_and_matches_compound_names() {
        use lychi_core::context::session::Desktop;
        assert!(is_mutter_family(
            true,
            &[Desktop::GnomeFlashback, Desktop::Gnome]
        ));
        assert!(!is_mutter_family(true, &[Desktop::Kde]));
        assert!(!is_mutter_family(true, &[]));
    }

    /// KDE goes down the move_() branch, so it must not be misdetected as
    /// GNOME — and it keeps its own non-fullscreen path for I-001 (blur/dim).
    ///
    /// Asserted against the pure rule rather than by setting env vars: session
    /// detection is cached on first use, so an env-mutating version of this
    /// passed on a KDE machine and proved nothing anywhere else.
    #[test]
    fn kde_wayland_is_distinct_from_gnome() {
        use lychi_core::context::session::Desktop;
        assert!(!is_mutter_family(true, &[Desktop::Kde]));
        assert!(is_mutter_family(true, &[Desktop::Gnome]));
    }
}
