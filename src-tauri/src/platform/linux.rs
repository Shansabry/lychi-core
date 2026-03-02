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

/// Detect KDE Plasma on Wayland (where layer-shell focus is unreliable).
/// Uses `XDG_SESSION_DESKTOP` first (single-valued, more reliable), falls back
/// to `XDG_CURRENT_DESKTOP`. Only triggers on Wayland sessions.
pub fn is_kde_wayland() -> bool {
    let is_wayland = std::env::var("XDG_SESSION_TYPE")
        .map(|v| v == "wayland")
        .unwrap_or(false);
    let is_kde = std::env::var("XDG_SESSION_DESKTOP")
        .or_else(|_| std::env::var("XDG_CURRENT_DESKTOP"))
        .map(|v| v.to_uppercase().contains("KDE"))
        .unwrap_or(false);
    is_wayland && is_kde
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
    } else if is_kde_wayland() {
        // KDE toplevel: fullscreen container, CSS handles centering
        gtk_win.move_(geom.x(), geom.y());
        gtk_win.set_size_request(geom.width(), geom.height());
        tracing::debug!(
            "Repositioned KDE toplevel fullscreen to {},{} ({}x{})",
            geom.x(),
            geom.y(),
            geom.width(),
            geom.height()
        );
    } else {
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

    use gtk::prelude::{GtkWindowExt, MonitorExt, WidgetExt};

    let use_layer_shell = match strategy {
        "layer-shell" => {
            if gtk_layer_shell::is_supported() {
                true
            } else {
                tracing::warn!(
                    "layer-shell strategy requested but not supported, falling back to x11"
                );
                false
            }
        }
        "x11" | "toplevel" => false,
        _ => {
            // "auto": use layer-shell UNLESS on KDE Wayland (focus bugs)
            if is_kde_wayland() {
                tracing::info!(
                    "KDE Wayland detected — using toplevel window (layer-shell focus unreliable on KWin)"
                );
                false
            } else {
                gtk_layer_shell::is_supported()
            }
        }
    };

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

        if use_layer_shell {
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
        } else if strategy == "toplevel" || (strategy == "auto" && is_kde_wayland()) {
            // KDE toplevel: fullscreen transparent container — CSS handles centering.
            // Wayland xdg_toplevel ignores move_(), so we can't position a sized window.
            // Instead, cover the monitor and let the frontend center the launcher UI.
            // Normal GTK window — KWin's focus model works correctly here.
            // NOTE: set_skip_taskbar_hint doesn't work on Wayland — it only sets
            // X11 atoms. This is a known Tauri limitation (tauri#9829). The window
            // will appear in KDE's taskbar. We set Utility hint so it at least
            // behaves as a tool window (no alt-tab, stays above, etc).
            gtk_win.set_type_hint(gdk::WindowTypeHint::Utility);
            gtk_win.set_skip_taskbar_hint(true);
            gtk_win.set_skip_pager_hint(true);
            gtk_win.set_decorated(false);
            gtk_win.set_keep_above(true);

            gtk_win.move_(geom.x(), geom.y());
            gtk_win.set_size_request(geom.width(), geom.height());
            tracing::debug!(
                "KDE toplevel window: fullscreen {}x{} at {},{} (strategy: {strategy})",
                geom.width(),
                geom.height(),
                geom.x(),
                geom.y()
            );
        } else {
            // X11 path — fullscreen overlay on primary monitor
            gtk_win.move_(geom.x(), geom.y());
            gtk_win.set_size_request(geom.width(), geom.height());
            gtk_win.set_skip_taskbar_hint(true);
            gtk_win.set_skip_pager_hint(true);
            tracing::debug!("X11 window hints applied (strategy: {strategy})");
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
/// Works on both layer-shell (wlroots) and KDE toplevel windows.
/// On layer-shell: also switches keyboard mode to OnDemand on first interaction.
/// On X11 fullscreen: returns early (frontend handles dismiss via click-on-backdrop).
///
///   show_window() → dismiss_armed=false
///   key-press / button-press inside window → dismiss_armed=true
///   focus-out (armed) → emit lychi://dismiss
///   focus-out (not armed) → ignore (compositor churn)
pub fn setup_dismiss_on_blur(
    window: &WebviewWindow,
    dismiss_armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    summon_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
) {
    let gtk_win = match window.gtk_window() {
        Ok(w) => w,
        Err(_) => return,
    };

    use gtk_layer_shell::LayerShell;
    let is_layer = gtk_win.is_layer_window();
    let is_toplevel_kde = !is_layer && is_kde_wayland();

    if !is_layer && !is_toplevel_kde {
        return; // X11 fullscreen — frontend handles dismiss
    }

    use gtk::prelude::WidgetExt;
    use std::sync::atomic::Ordering;

    // key-press: user typed → arm dismiss (Escape handled separately)
    {
        let armed = dismiss_armed.clone();
        let seq = summon_seq.clone();
        gtk_win.connect_key_press_event(move |_, event| {
            use gdk::keys::constants as key;
            let keyval = event.keyval();
            // Don't arm on Escape (handled by setup_escape_handler)
            if keyval != key::Escape && !armed.load(Ordering::SeqCst) {
                armed.store(true, Ordering::SeqCst);
                tracing::info!(
                    "[dismiss] seq={} key-press → armed=true",
                    seq.load(Ordering::SeqCst)
                );
            }
            glib::Propagation::Proceed // let key propagate to WebView
        });
    }

    // button-press: user clicked inside → arm dismiss
    {
        let armed = dismiss_armed.clone();
        let seq = summon_seq.clone();
        gtk_win.connect_button_press_event(move |_, _| {
            if !armed.load(Ordering::SeqCst) {
                armed.store(true, Ordering::SeqCst);
                tracing::info!(
                    "[dismiss] seq={} button-press → armed=true",
                    seq.load(Ordering::SeqCst)
                );
            }
            glib::Propagation::Proceed
        });
    }

    // focus-out: only dismiss if user interacted (armed)
    let handle = window.app_handle().clone();
    gtk_win.connect_focus_out_event(move |_, _| {
        let seq = summon_seq.load(Ordering::SeqCst);
        let armed = dismiss_armed.load(Ordering::SeqCst);
        if armed {
            tracing::info!("[dismiss] seq={seq} focus-out (armed) → DISMISS");
            dismiss_armed.store(false, Ordering::SeqCst);
            use tauri::Emitter;
            let _ = handle.emit("lychi://dismiss", ());
        } else {
            tracing::info!("[dismiss] seq={seq} focus-out (not armed) → ignored");
        }
        glib::Propagation::Proceed
    });
    tracing::info!(
        "[dismiss] interaction-gated dismiss handler installed (layer={is_layer}, kde_toplevel={is_toplevel_kde})"
    );
}

/// GTK-level Escape key handler — catches Escape even when the WebView
/// input doesn't have DOM focus. Emits `lychi://gtk-escape` for the frontend.
/// Active on layer-shell and KDE toplevel (X11 handles Escape via fullscreen overlay click).
pub fn setup_escape_handler(window: &WebviewWindow) {
    let gtk_win = match window.gtk_window() {
        Ok(w) => w,
        Err(_) => return,
    };

    use gtk_layer_shell::LayerShell;
    let is_layer = gtk_win.is_layer_window();
    if !is_layer && !is_kde_wayland() {
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
pub async fn open_uri(uri: &str) -> Result<(), String> {
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
