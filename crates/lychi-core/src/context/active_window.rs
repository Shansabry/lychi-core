//! Active window detection — reads the currently focused window.
//!
//! X11: reads `_NET_ACTIVE_WINDOW` from root, fetches title/class/pid.
//! KWin Wayland: D-Bus script reading `workspace.activeWindow`.
//!
//! ## Caching (KWin Wayland only)
//!
//! `detect_kwin()` checks `KWIN_CACHE` before running a live D-Bus probe:
//! - Age < 1s   → return cached immediately (0ms, "cache_hot").
//! - Age 1–15s  → return cached, spawn one background refresh (single-flight).
//! - Age > 15s  → run live probe, update cache.
//!
//! The cache is populated by `run_kwin_watcher()`, a background async task
//! that polls `detect_kwin_live()` at adaptive intervals and probes on change.
//! KWin does not expose `windowActivated` as an observable D-Bus signal.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use super::WindowContext;

/// Known terminal emulator WM classes (real terminals only, not IDEs).
///
/// Matched with `==` after lowercasing — no substring matching, to prevent
/// false positives like "st" matching "netsoft-com.netsoft.hubstaff".
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
    "dev.warp.warp",
    "warp",
];

/// User-supplied extra terminal WM classes, registered once at startup.
static EXTRA_TERMINALS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// Register additional terminal WM classes from config.
/// Call once at startup with `config.commands.extra_terminals`.
/// No-op if called more than once.
pub fn register_extra_terminals(extra: &[String]) {
    let _ = EXTRA_TERMINALS.set(extra.iter().map(|s| s.to_lowercase()).collect());
}

/// Check if a wm_class is a terminal emulator.
/// Normalize a Wayland reverse-DNS `resourceClass` to its short form.
/// `"org.kde.konsole"` → `"konsole"`, `"kitty"` → `"kitty"`.
pub fn normalize_wm_class(wm_class: &str) -> String {
    let lower = wm_class.to_lowercase();
    let short = lower.rsplit('.').next().unwrap_or(&lower);
    if short == lower {
        lower
    } else {
        short.to_string()
    }
}

pub fn is_terminal_class(wm_class: &str) -> bool {
    let lower = wm_class.to_lowercase();
    let short = normalize_wm_class(wm_class);
    if TERMINALS.iter().any(|t| lower == *t || short == *t) {
        return true;
    }
    EXTRA_TERMINALS
        .get()
        .is_some_and(|extra| extra.contains(&lower) || extra.contains(&short))
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

// ── KWin cache ───────────────────────────────────────────────────────────

struct CachedWindow {
    window: WindowContext,
    cached_at: Instant,
}

static KWIN_CACHE: Mutex<Option<CachedWindow>> = Mutex::new(None);

/// A background refresh is already in-flight; don't spawn another.
static REFRESH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Monotonic process start time. Used to produce a monotonic millisecond
/// counter without OS epoch calls — immune to wall-clock jumps.
static PROCESS_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn mono_ms() -> u64 {
    PROCESS_START
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis() as u64
}

/// Monotonic millisecond timestamp of the last recorded summon.
/// 0 = never summoned.
static LAST_SUMMON_MONO_MS: AtomicU64 = AtomicU64::new(0);

/// Record a summon event. Call this on every summon to keep the watcher's
/// idle-backoff gate accurate.
pub fn record_summon() {
    LAST_SUMMON_MONO_MS.store(mono_ms(), Ordering::Relaxed);
}

/// Milliseconds since the last recorded summon. Returns `u64::MAX` if never summoned.
fn ms_since_last_summon() -> u64 {
    let last = LAST_SUMMON_MONO_MS.load(Ordering::Relaxed);
    if last == 0 {
        return u64::MAX;
    }
    mono_ms().saturating_sub(last)
}

/// Cache is fresh enough to return immediately (no probe needed).
const CACHE_HOT_SECS: f32 = 1.0;
/// Cache is stale enough to force a live probe.
const CACHE_HARD_EXPIRY_SECS: f32 = 15.0;

/// Update the KWin active-window cache. Called by the background watcher
/// and by `detect_kwin()` after a live probe.
pub(crate) fn set_kwin_cache(window: WindowContext) {
    if let Ok(mut guard) = KWIN_CACHE.lock() {
        *guard = Some(CachedWindow {
            window,
            cached_at: Instant::now(),
        });
    }
}

// ── Public API ───────────────────────────────────────────────────────────

/// Detect the currently focused window, using cache when fresh.
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

/// Detect the currently focused window, **bypassing cache**.
///
/// Use this for pre-summon snapshots where ground truth matters — the cache
/// may be stale if the watcher hasn't polled since the user switched windows.
/// Updates the cache as a side effect so subsequent `detect()` calls are fast.
pub(crate) fn detect_live() -> Option<WindowContext> {
    let result = if super::is_wayland() {
        let w = detect_kwin_live()?;
        set_kwin_cache(w.clone());
        Some(w)
    } else {
        detect_x11()
    };
    tracing::debug!(
        "active_window::detect_live: {:?}",
        result.as_ref().map(|w| format!(
            "{}(pid={},term={},ide={},title={})",
            w.wm_class, w.pid, w.is_terminal, w.is_ide, w.title
        ))
    );
    result
}

// ── KWin Wayland ────────────────────────────────────────────────────────

/// Detect active window on KWin Wayland, using cache when fresh.
///
/// Cache tiers:
/// - Hot (< 1s)     → return immediately, no probe.
/// - Warm (1–15s)   → return cached, kick off one background refresh.
/// - Cold (> 15s)   → live probe, update cache, return result.
///
/// If the cache lock is poisoned, falls back to a live probe.
fn detect_kwin() -> Option<WindowContext> {
    if let Ok(guard) = KWIN_CACHE.lock()
        && let Some(ref c) = *guard
    {
        let age = c.cached_at.elapsed().as_secs_f32();
        if age < CACHE_HOT_SECS {
            tracing::debug!("active_window: cache_hot (age={age:.2}s)");
            return Some(c.window.clone());
        }
        if age < CACHE_HARD_EXPIRY_SECS {
            let cached = c.window.clone();
            drop(guard);
            tracing::debug!("active_window: cache_warm (age={age:.2}s), spawning refresh");
            // Single-flight: only one background refresh at a time
            if REFRESH_IN_FLIGHT
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                std::thread::spawn(|| {
                    if let Some(w) = detect_kwin_live() {
                        set_kwin_cache(w);
                    }
                    REFRESH_IN_FLIGHT.store(false, Ordering::Release);
                });
            }
            return Some(cached);
        }
    }
    // Cold cache — live probe
    tracing::debug!("active_window: cache_cold, running live probe");
    let result = detect_kwin_live()?;
    set_kwin_cache(result.clone());
    Some(result)
}

/// Run a live KWin D-Bus probe for the active window.
/// This is the original `detect_kwin()` body, extracted to allow cache + watcher to call it.
#[cfg(target_os = "linux")]
fn detect_kwin_live() -> Option<WindowContext> {
    use std::sync::mpsc;
    use std::time::Duration;

    use dbus::blocking::SyncConnection;
    use dbus::channel::MatchingReceiver;
    use dbus::message::MatchRule;

    let conn = SyncConnection::new_session().ok()?;
    let bus_name = conn.unique_name().to_string();

    // JS script: read workspace.activeWindow properties.
    // w.internalId provides a stable per-window UUID; empty string on older Plasma.
    // Delimiter is \x1F (ASCII Unit Separator) — cannot appear in window titles.
    let script = format!(
        r#"
var SEP = "\x1F";
var w = workspace.activeWindow;
if (w && w.caption && w.pid > 0) {{
    var rc = w.resourceClass ? w.resourceClass.toString() : "";
    var cap = w.caption ? w.caption.toString() : "";
    var p = w.pid ? w.pid : 0;
    var id = w.internalId ? w.internalId.toString() : "";
    callDBus("{bus_name}", "/", "", "lychi_active_win", rc + SEP + p + SEP + cap + SEP + id);
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
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut payload = None;
    while Instant::now() < deadline {
        let remaining = deadline - Instant::now();
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

    // Parse: rc \x1F pid \x1F cap \x1F id (id may be absent on older Plasma)
    // \x1F (Unit Separator) cannot appear in window titles, unlike \t.
    let data = payload.filter(|s| !s.is_empty())?;
    let parts: Vec<&str> = data.splitn(4, '\x1F').collect();
    if parts.len() < 3 {
        return None;
    }

    let wm_class = parts[0].to_lowercase();
    let pid: u32 = parts[1].parse().unwrap_or(0);
    let title = parts[2].to_string();
    let window_id = parts
        .get(3)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    if pid == 0 || wm_class == "lychi" {
        return None;
    }

    Some(WindowContext {
        is_terminal: is_terminal_class(&wm_class),
        is_ide: is_ide_class(&wm_class),
        title,
        wm_class,
        pid,
        window_id,
    })
}

#[cfg(not(target_os = "linux"))]
fn detect_kwin_live() -> Option<WindowContext> {
    None
}

#[cfg(not(target_os = "linux"))]
fn detect_kwin() -> Option<WindowContext> {
    None
}

// ── KWin watcher ────────────────────────────────────────────────────────

/// Stable identity key for change detection.
///
/// Prefer `window_id` (KWin UUID / X11 hex wid) — distinguishes two windows
/// of the same app (two terminals, two VSCode windows). Falls back to
/// `pid + wm_class` when `window_id` is absent (older Plasma / rare case).
fn window_key(w: &WindowContext) -> (Option<&str>, u32, &str) {
    (w.window_id.as_deref(), w.pid, &w.wm_class)
}

/// Background task: poll KWin active window and keep the cache warm.
///
/// KWin does not expose a `windowActivated` D-Bus *signal* observable from
/// outside the KWin script engine. Polling is the only reliable alternative.
///
/// Adaptive intervals keep baseline cost low while ensuring snappy tracking
/// right after a focus switch:
/// - Fast (200ms) for 2s after a detected change — user is switching windows.
/// - Slow (2s) in steady state — Lychi is idle in the background.
///
/// Cache is updated only on actual window change or when cold/expired; the
/// mutex is not touched on quiet polls.
///
/// Gate: Linux only.
#[cfg(target_os = "linux")]
pub async fn run_kwin_watcher(push_focus: impl Fn(WindowContext) + Send + 'static) {
    tracing::info!(
        "[ctx] KWin watcher armed (adaptive polling: fast=200ms, slow=2s±jitter, idle=8s±jitter)"
    );

    /// Fast-mode interval right after a detected focus change. No jitter — we
    /// want quick detection right after a switch.
    const FAST_MS: u64 = 200;
    /// Steady-state base interval.
    const SLOW_MS: u64 = 2_000;
    /// Idle base interval (no recent summon).
    const IDLE_MS: u64 = 8_000;
    /// ±Jitter applied to slow/idle intervals to avoid phase-locking with
    /// other periodic tasks. Uses a lightweight LCG — no external dependency.
    const JITTER_MS: u64 = 50;
    /// How long to stay in fast mode after the last detected change (Instant deadline).
    const FAST_WINDOW_MS: u64 = 2_000;
    /// Back off to idle polling after this many ms without a summon (10 minutes).
    const IDLE_AFTER_MS: u64 = 600_000;

    let mut last_key: Option<(Option<String>, u32, String)> = None;
    // Initialise to "already past fast window" so first poll uses slow interval.
    let mut fast_until = tokio::time::Instant::now();
    // Lightweight LCG for jitter — seeded from process start mono time.
    let mut rng: u64 = mono_ms().wrapping_add(0xDEAD_BEEF_CAFE_1337);

    loop {
        // Three-tier base interval: fast (post-change) → slow (steady) → idle.
        let base = if tokio::time::Instant::now() < fast_until {
            FAST_MS
        } else if ms_since_last_summon() > IDLE_AFTER_MS {
            IDLE_MS
        } else {
            SLOW_MS
        };
        // Add ±JITTER_MS to non-fast intervals via LCG.
        let interval = if base == FAST_MS {
            base
        } else {
            rng = rng
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let jitter = (rng >> 58) % (JITTER_MS * 2 + 1); // 0..=2*JITTER_MS
            base - JITTER_MS + jitter
        };
        tokio::time::sleep(tokio::time::Duration::from_millis(interval)).await;

        let Some(ctx) = detect_kwin_live() else {
            continue;
        };

        let key = (ctx.window_id.clone(), ctx.pid, ctx.wm_class.clone());
        let changed = last_key
            .as_ref()
            .map(|(id, pid, cls)| window_key(&ctx) != (id.as_deref(), *pid, cls.as_str()))
            .unwrap_or(true); // first probe always counts as "changed"

        if changed {
            last_key = Some(key);
            fast_until =
                tokio::time::Instant::now() + tokio::time::Duration::from_millis(FAST_WINDOW_MS);

            set_kwin_cache(ctx.clone());

            tracing::debug!(
                "kwin watcher: focus → '{}' pid={} id={:?}",
                ctx.wm_class,
                ctx.pid,
                ctx.window_id
            );
            push_focus(ctx);
        }
        // Cache not touched on quiet polls — no mutex traffic.
    }
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
        // X11 window ID as hex string — stable per-window identifier
        window_id: Some(format!("{:#010x}", wid)),
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
