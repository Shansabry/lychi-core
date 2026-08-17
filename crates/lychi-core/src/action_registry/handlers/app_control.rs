use std::sync::Mutex;
use std::time::Instant;

use async_trait::async_trait;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
    RiskLevel,
};
use crate::error::LychiError;

#[cfg(target_os = "linux")]
use super::kwin_windows;
#[cfg(target_os = "linux")]
use super::x11_windows;
use crate::text::truncate_display;

pub struct AppControlHandler;

impl Default for AppControlHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl AppControlHandler {
    pub fn new() -> Self {
        Self
    }
}

/// A running window discovered from the window manager.
#[derive(Debug, Clone)]
pub(crate) struct RunningWindow {
    /// X11 window ID (only set on X11 sessions)
    pub(crate) window_id: Option<u32>,
    /// KWin internalId UUID (only set on Wayland/KDE)
    pub(crate) kwin_id: Option<String>,
    /// Window title
    pub(crate) title: String,
    /// WM class or app name (lowercase)
    pub(crate) wm_class: String,
    /// Process ID
    pub(crate) pid: u32,
    /// Virtual desktop number (KWin: 1-indexed, X11: 0-indexed), None if on all desktops
    pub(crate) desktop: Option<u32>,
}

/// Cached window list with TTL.
struct WindowCache {
    windows: Vec<RunningWindow>,
    fetched_at: Instant,
}

static WINDOW_CACHE: Mutex<Option<WindowCache>> = Mutex::new(None);
const CACHE_TTL_SECS: u64 = 2;

/// Get the list of running windows, using cache if fresh.
pub(crate) fn get_windows() -> Vec<RunningWindow> {
    if let Ok(cache) = WINDOW_CACHE.lock()
        && let Some(ref c) = *cache
        && c.fetched_at.elapsed().as_secs() < CACHE_TTL_SECS
    {
        return c.windows.clone();
    }

    let windows = enumerate_windows();

    if let Ok(mut cache) = WINDOW_CACHE.lock() {
        *cache = Some(WindowCache {
            windows: windows.clone(),
            fetched_at: Instant::now(),
        });
    }

    windows
}

/// Enumerate windows using the appropriate backend for the session type.
/// KDE Wayland → KWin D-Bus scripting (sees all windows).
/// X11 → native EWMH protocol via x11rb.
/// Other Wayland (Sway/Hyprland/niri/wlroots) → wlr-foreign-toplevel protocol.
/// GNOME Wayland → no backend (Mutter implements neither protocol), empty list.
#[cfg(target_os = "linux")]
fn enumerate_windows() -> Vec<RunningWindow> {
    let compositor = crate::context::compositor();
    tracing::info!("appctl: enumerate_windows ({compositor:?})");
    if compositor == crate::context::Compositor::KdeWayland {
        kwin_windows::enumerate_windows()
            .into_iter()
            .map(|w| RunningWindow {
                window_id: None,
                kwin_id: w.internal_id,
                title: w.caption,
                wm_class: w.resource_class,
                pid: w.pid,
                desktop: w.desktop,
            })
            .collect()
    } else if compositor == crate::context::Compositor::X11 {
        x11_windows::enumerate_windows()
            .into_iter()
            .map(|w| RunningWindow {
                window_id: Some(w.window_id),
                kwin_id: None,
                title: w.title,
                wm_class: w.wm_class,
                pid: w.pid,
                desktop: w.desktop,
            })
            .collect()
    } else if compositor == crate::context::Compositor::OtherWayland {
        // wlroots family (Sway/Hyprland/niri/Wayfire/COSMIC). The protocol gives
        // app_id (used as wm_class, lowercased for match parity) + title, but no
        // pid and no virtual-desktop info — hence pid: 0, desktop: None. Focus
        // and close later re-match on (wm_class, title) since the protocol has
        // no stable cross-connection window id.
        crate::context::wlr_toplevel::list_toplevels()
            .into_iter()
            .map(|w| RunningWindow {
                window_id: None,
                kwin_id: None,
                title: w.title,
                wm_class: w.app_id.to_lowercase(),
                pid: 0,
                desktop: None,
            })
            .collect()
    } else {
        Vec::new()
    }
}

#[cfg(not(target_os = "linux"))]
fn enumerate_windows() -> Vec<RunningWindow> {
    Vec::new()
}

/// How well a window answers a query. Higher is better; `None` is no match.
///
/// Bare `contains` on `wm_class` was the whole of this before, and it is the
/// false-positive class `classify_window_class` was rewritten to eliminate:
/// "code" matches "vscode", "qtcreator" and anything else with those letters
/// mid-word, and the FIRST such window in enumeration order won. Enumeration
/// order is arbitrary, so `quit code` could close the wrong application.
///
/// Substring is still allowed — this is a user searching, not a classifier —
/// but it is now the WEAKEST signal rather than the only one, and every
/// candidate is scored so the best match wins instead of the first seen.
fn match_score(window: &RunningWindow, query_lower: &str) -> Option<u32> {
    let class = window.wm_class.to_lowercase();
    let short = class.rsplit('.').next().unwrap_or(&class);
    let title = window.title.to_lowercase();

    if class == query_lower || short == query_lower {
        return Some(100); // exact class ("firefox")
    }
    if class.starts_with(query_lower) || short.starts_with(query_lower) {
        return Some(80); // prefix ("fire" → "firefox")
    }
    // Whole-word hit in the title ("slack" in "Slack | general"), which is a
    // real match rather than an accident of letters.
    if title
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == query_lower)
    {
        return Some(60);
    }
    if title.starts_with(query_lower) {
        return Some(50);
    }
    // Weakest: letters appearing anywhere. Kept so a partial recall still
    // finds something, but it can no longer outrank a real match.
    if class.contains(query_lower) {
        return Some(30);
    }
    if title.contains(query_lower) {
        return Some(20);
    }
    None
}

/// Does this window answer the query at all?
fn matches_window(window: &RunningWindow, query: &str) -> bool {
    match_score(window, &query.to_lowercase()).is_some()
}

/// Find the best matching window for a query.
///
/// Scores every candidate and takes the highest, rather than returning the
/// first window that happens to contain the letters. Ties keep enumeration
/// order (`max_by_key` is last-wins, so the fold below keeps the first).
pub(crate) fn find_window<'a>(
    windows: &'a [RunningWindow],
    query: &str,
) -> Option<&'a RunningWindow> {
    let query_lower = query.to_lowercase();
    windows
        .iter()
        .filter_map(|w| match_score(w, &query_lower).map(|s| (s, w)))
        .fold(
            None::<(u32, &'a RunningWindow)>,
            |best, (score, w)| match best {
                Some((bs, _)) if bs >= score => best,
                _ => Some((score, w)),
            },
        )
        .map(|(_, w)| w)
}

/// Shared by `focus` and `quit`: both act on a running window matched the same
/// way. Named `window` (not `app`) because the match is against window class
/// and title, and the field merges with the `win` handler's identically-shaped
/// operand in the group schema.
const WINDOW_OPERAND: Operand = Operand {
    name: "window",
    desc: "The running application/window to act on, fuzzy-matched against \
           window class and title (e.g. \"firefox\", \"slack\"). An exact class \
           match always beats a substring hit.",
    required: true,
    kind: ArgKind::Text,
    prefix: None,
};

/// `appctl`'s grammar: verb-first flat form, exactly what
/// [`parse_verb_and_target`] splits. `close` stays an accepted input alias of
/// `quit` but is not a separate model-facing action.
const APPCTL_GRAMMAR: Grammar = Grammar {
    verbs: &[
        Verb {
            name: "focus",
            desc: "Bring an already-running application's window to the front. \
                   Read-only in effect: nothing is closed or changed beyond which \
                   window has focus.",
            mutates: false,
            operands: &[WINDOW_OPERAND],
        },
        Verb {
            name: "quit",
            desc: "Gracefully close an application's window (like clicking its \
                   close button) — the app can prompt to save. Prefer this over \
                   kill.",
            mutates: true,
            operands: &[WINDOW_OPERAND],
        },
        Verb {
            name: "kill",
            desc: "Force-terminate a process (SIGTERM, then SIGKILL). Accepts an \
                   app/process name or a PID; a name matching several processes \
                   of the same program kills them all. Last resort — unsaved \
                   state is lost.",
            mutates: true,
            operands: &[Operand {
                name: "target",
                desc: "What to kill: an application/process name (e.g. \
                       \"spotify\") or a numeric PID for one exact process.",
                required: true,
                kind: ArgKind::Text,
                prefix: None,
            }],
        },
    ],
};

/// Normalize the tool's `args` to the flat `"<verb> <target>"` string
/// [`parse_verb_and_target`] splits, via the ONE structured→flat decider
/// ([`Grammar::flatten_json`]). A human or legacy/flat caller passes through
/// unchanged.
fn appctl_args_to_flat(args: &str) -> String {
    APPCTL_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
}

/// Parse the verb and target from args.
/// "focus firefox" → ("focus", "firefox")
/// "quit code" → ("quit", "code")
fn parse_verb_and_target(args: &str) -> (&str, &str) {
    let args = args.trim();
    if let Some(space) = args.find(' ') {
        let verb = &args[..space];
        let target = args[space + 1..].trim();
        (verb, target)
    } else {
        (args, "")
    }
}

/// Focus a window natively. Prefers per-window ID targeting when available.
#[cfg(target_os = "linux")]
pub(crate) fn do_focus(window: &RunningWindow) -> Result<(), String> {
    match crate::context::compositor() {
        crate::context::Compositor::KdeWayland => {
            if let Some(ref id) = window.kwin_id {
                kwin_windows::focus_window_by_id(id)
            } else {
                kwin_windows::focus_window(&window.wm_class)
            }
        }
        crate::context::Compositor::X11 => {
            if let Some(wid) = window.window_id {
                x11_windows::focus_window(wid)
            } else {
                Err("No window ID available".to_string())
            }
        }
        crate::context::Compositor::OtherWayland => {
            crate::context::wlr_toplevel::activate(&window.wm_class, &window.title)
        }
        _ => Err("Window control not available on this compositor".to_string()),
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn do_focus(_window: &RunningWindow) -> Result<(), String> {
    Err("Window focus not supported on this platform".to_string())
}

/// Focus a running window by WM class. Used by smart-open in the Tauri bridge.
/// Returns Ok(()) if a matching window was found and focused, Err otherwise.
pub fn focus_by_class(wm_class: &str) -> Result<(), String> {
    let windows = get_windows();
    let window = find_window(&windows, wm_class)
        .ok_or_else(|| format!("No running window with class '{wm_class}'"))?;
    do_focus(window)
}

/// Gracefully close a window natively. Prefers per-window ID targeting when available.
#[cfg(target_os = "linux")]
pub(crate) fn do_close(window: &RunningWindow) -> Result<(), String> {
    match crate::context::compositor() {
        crate::context::Compositor::KdeWayland => {
            if let Some(ref id) = window.kwin_id {
                kwin_windows::close_window_by_id(id)
            } else {
                kwin_windows::close_window(&window.wm_class)
            }
        }
        crate::context::Compositor::X11 => {
            if let Some(wid) = window.window_id {
                x11_windows::close_window(wid)
            } else {
                Err("No window ID available".to_string())
            }
        }
        crate::context::Compositor::OtherWayland => {
            crate::context::wlr_toplevel::close(&window.wm_class, &window.title)
        }
        _ => Err("Window control not available on this compositor".to_string()),
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn do_close(_window: &RunningWindow) -> Result<(), String> {
    Err("Window close not supported on this platform".to_string())
}

#[async_trait]
impl ActionHandler for AppControlHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::{ArgTransform, Trigger};
        static TRIGGERS: &[Trigger] = &[
            // `appctl <verb> <target>` — internal run command, verbatim args.
            Trigger::keywords(&["appctl"]),
            // Bare verbs — prepend the verb so the handler sees "kill spotify".
            Trigger::new(
                &["focus", "quit", "close", "kill"],
                ArgTransform::PrependKeyword,
            ),
        ];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "appctl"
    }

    fn mutates_state(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Focus, quit, or kill running applications"
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(APPCTL_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::System
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::System
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // A structured caller sends `{"action":..,"window"/"target":..}`;
        // flatten it (a plain-string caller passes through) to the verb-first
        // form the split below reads.
        let flat = appctl_args_to_flat(args);
        let (verb, target) = parse_verb_and_target(&flat);

        if target.is_empty() {
            return Ok(ActionResult::err(format!(
                "Usage: {verb} <app name>. Try 'focus firefox' or 'quit code'."
            )));
        }

        // For kill: try process-level matching first (tracked + system /proc scan)
        // before falling through to window kill, which would nuke an entire terminal.
        if verb == "kill" {
            // Tier 1: Lychi-tracked processes (also handles PID input for tracked procs)
            if let Ok(msg) = crate::process_tracker::kill_by(target) {
                return Ok(ActionResult::ok(msg, OutputType::Status).with_risk(RiskLevel::Medium));
            }

            // Tier 1.5: direct PID kill (from completion selection or user typing a PID)
            if let Ok(pid) = target.parse::<u32>() {
                return Ok(match crate::process_tracker::kill_system_pid(pid) {
                    Ok(msg) => {
                        ActionResult::ok(msg, OutputType::Status).with_risk(RiskLevel::Medium)
                    }
                    Err(e) => ActionResult::err(e),
                });
            }

            // Tier 2: system processes via /proc scan
            let system_matches = crate::process_tracker::scan_system(target);
            match system_matches.len() {
                1 => {
                    return Ok(
                        match crate::process_tracker::kill_system_pid(system_matches[0].pid) {
                            Ok(msg) => ActionResult::ok(msg, OutputType::Status)
                                .with_risk(RiskLevel::Medium),
                            Err(e) => ActionResult::err(e),
                        },
                    );
                }
                n if n > 1 => {
                    // Many matches usually means one app with several processes
                    // (Spotify/Chrome spawn renderers/helpers). "kill spotify"
                    // means kill the WHOLE app, so if all exact-name matches are
                    // the SAME program, kill them all — never ask the user to
                    // hand-pick a PID from a wall of identical names.
                    let target_lower = target.to_lowercase();
                    let exact: Vec<_> = system_matches
                        .iter()
                        .filter(|p| p.comm.to_lowercase() == target_lower)
                        .collect();

                    // Decide the kill set: the exact-name group if any, else all
                    // matches only when they're all the same program name.
                    let distinct_comms: std::collections::HashSet<String> = system_matches
                        .iter()
                        .map(|p| p.comm.to_lowercase())
                        .collect();
                    let kill_set: Vec<u32> = if !exact.is_empty() {
                        exact.iter().map(|p| p.pid).collect()
                    } else if distinct_comms.len() == 1 {
                        system_matches.iter().map(|p| p.pid).collect()
                    } else {
                        // Genuinely different programs share the query → ambiguous;
                        // only here do we ask the user to disambiguate.
                        let names: Vec<String> = system_matches
                            .iter()
                            .map(|p| format!("  {} (pid={})", p.comm, p.pid))
                            .collect();
                        return Ok(ActionResult::err(format!(
                            "Multiple different programs match '{target}', specify PID:\n{}",
                            names.join("\n")
                        )));
                    };

                    let mut killed = 0usize;
                    let mut last_err = None;
                    for pid in &kill_set {
                        match crate::process_tracker::kill_system_pid(*pid) {
                            Ok(_) => killed += 1,
                            // A child dying when its parent is killed is expected —
                            // "already exited" isn't a real failure.
                            Err(e) if e.contains("already exited") => {}
                            Err(e) => last_err = Some(e),
                        }
                    }
                    let name = exact
                        .first()
                        .map(|p| p.comm.clone())
                        .unwrap_or_else(|| target.to_string());
                    return Ok(if killed > 0 {
                        let msg = if killed == 1 {
                            format!("Killed {name}")
                        } else {
                            format!("Killed {name} ({killed} processes)")
                        };
                        ActionResult::ok(msg, OutputType::Status).with_risk(RiskLevel::Medium)
                    } else {
                        ActionResult::err(
                            last_err.unwrap_or_else(|| format!("Couldn't kill {name}")),
                        )
                    });
                }
                _ => {} // 0 matches — fall through to window kill
            }
        }

        let windows = get_windows();
        let window = match find_window(&windows, target) {
            Some(w) => w,
            None => {
                let msg = if verb == "kill" {
                    format!("No running process matching '{target}'")
                } else {
                    format!("No running window matching '{target}'")
                };
                return Ok(ActionResult::err(msg));
            }
        };

        match verb {
            "focus" => match do_focus(window) {
                Ok(()) => Ok(ActionResult::ok(
                    format!("Focused: {}", window.title),
                    OutputType::Status,
                )),
                Err(e) => Ok(ActionResult::err(format!(
                    "Failed to focus '{}': {e}",
                    window.title
                ))),
            },
            "quit" | "close" => match do_close(window) {
                Ok(()) => Ok(ActionResult::ok(
                    format!("Closed: {}", window.title),
                    OutputType::Status,
                )),
                Err(e) => Ok(ActionResult::err(format!(
                    "Failed to close '{}': {e}",
                    window.title
                ))),
            },
            "kill" => {
                // Graceful kill via SIGTERM → SIGKILL (reuse process_tracker logic)
                match crate::process_tracker::kill_system_pid(window.pid) {
                    Ok(_) => Ok(ActionResult::ok(
                        format!("Killed: {} (PID {})", window.title, window.pid),
                        OutputType::Status,
                    )
                    .with_risk(RiskLevel::Medium)),
                    Err(e) => Ok(ActionResult::err(format!(
                        "Failed to kill '{}' (PID {}): {e}",
                        window.title, window.pid
                    ))),
                }
            }
            _ => Ok(ActionResult::err(format!(
                "Unknown verb '{verb}'. Use focus, quit, or kill."
            ))),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let (verb, target) = parse_verb_and_target(partial);

        // If no verb yet, show available verbs
        if partial.is_empty() || (!["focus", "quit", "close", "kill"].contains(&verb)) {
            return vec![
                CompletionItem {
                    label: "focus <app>".to_string(),
                    icon_path: None,
                    score: 900,
                    description: Some("Bring window to front".to_string()),
                    reason: None,
                    thumb_b64: None,
                    // Needs an app name — fill the verb so the user types it.
                    fill: Some("focus ".to_string()),
                    ..Default::default()
                },
                CompletionItem {
                    label: "quit <app>".to_string(),
                    icon_path: None,
                    score: 800,
                    description: Some("Gracefully close".to_string()),
                    reason: None,
                    thumb_b64: None,
                    fill: Some("quit ".to_string()),
                    ..Default::default()
                },
                CompletionItem {
                    label: "kill <app>".to_string(),
                    icon_path: None,
                    score: 700,
                    description: Some("Force-kill process".to_string()),
                    reason: None,
                    thumb_b64: None,
                    fill: Some("kill ".to_string()),
                    ..Default::default()
                },
            ];
        }

        // Show matching windows
        let windows = get_windows();

        if target.is_empty() {
            // Show all windows
            return windows
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    let display_name = if w.wm_class.is_empty() {
                        w.title.clone()
                    } else {
                        w.wm_class.clone()
                    };
                    CompletionItem {
                        label: display_name.clone(),
                        icon_path: None,
                        score: (1000 - i as u16).max(1),
                        description: Some(truncate_display(&w.title, 50)),
                        reason: None,
                        thumb_b64: None,
                        run: Some(format!("appctl {verb} {display_name}")),
                        ..Default::default()
                    }
                })
                .collect();
        }

        // Fuzzy filter
        let target_lower = target.to_lowercase();
        let matched_windows: Vec<&RunningWindow> = windows
            .iter()
            .filter(|w| matches_window(w, &target_lower))
            .collect();
        let mut items: Vec<CompletionItem> = matched_windows
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let display_name = if w.wm_class.is_empty() {
                    w.title.clone()
                } else {
                    w.wm_class.clone()
                };
                CompletionItem {
                    label: display_name.clone(),
                    icon_path: None,
                    score: (1000 - i as u16).max(1),
                    description: Some(truncate_display(&w.title, 50)),
                    reason: None,
                    thumb_b64: None,
                    run: Some(format!("appctl {verb} {display_name}")),
                    ..Default::default()
                }
            })
            .collect();

        // For kill verb, also show tracked processes and system processes
        if verb == "kill" {
            // Seed seen_pids with window PIDs to avoid duplicate completions
            let mut seen_pids: std::collections::HashSet<u32> =
                matched_windows.iter().map(|w| w.pid).collect();

            // Tier 2: Lychi-spawned processes (highest priority)
            let tracked = crate::process_tracker::list();
            for proc in tracked {
                let matches =
                    target.is_empty() || proc.command.to_lowercase().contains(&target_lower);
                if matches {
                    seen_pids.insert(proc.pid);
                    let elapsed = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .saturating_sub(proc.started_at);
                    let duration = if elapsed < 60 {
                        format!("{elapsed}s")
                    } else {
                        format!("{}m", elapsed / 60)
                    };
                    items.push(CompletionItem {
                        label: proc.command.clone(),
                        icon_path: Some("__terminal__".to_string()),
                        score: 950,
                        description: Some(format!("pid={} running {duration}", proc.pid)),
                        reason: None,
                        thumb_b64: None,
                        // Kill by PID — unambiguous even if the command string repeats.
                        run: Some(format!("appctl kill {}", proc.pid)),
                        ..Default::default()
                    });
                }
            }

            // Tier 3: system processes from /proc scan (cached)
            // Use PID as label so each completion maps to a unique process.
            // When selected, `kill <pid>` is sent which parses directly as a PID.
            if !target.is_empty() {
                let system = crate::process_tracker::scan_system(&target_lower);
                for proc in system {
                    if seen_pids.contains(&proc.pid) {
                        continue;
                    }
                    items.push(CompletionItem {
                        label: proc.pid.to_string(),
                        icon_path: Some("__terminal__".to_string()),
                        score: 900,
                        description: Some(format!(
                            "{} — {}",
                            proc.comm,
                            truncate_display(&proc.cmdline, 60)
                        )),
                        reason: None,
                        thumb_b64: None,
                        run: Some(format!("appctl kill {}", proc.pid)),
                        ..Default::default()
                    });
                }
            }
        }

        items
    }
}

#[cfg(test)]
mod grammar_tests {
    use super::*;

    #[test]
    fn appctl_args_flatten_from_structured_json() {
        // A structured caller sends the typed object; it flattens to the
        // verb-first string parse_verb_and_target splits.
        assert_eq!(
            appctl_args_to_flat(r#"{"action":"focus","window":"firefox"}"#),
            "focus firefox"
        );
        assert_eq!(
            appctl_args_to_flat(r#"{"action":"quit","window":"code"}"#),
            "quit code"
        );
        assert_eq!(
            appctl_args_to_flat(r#"{"action":"kill","target":"spotify"}"#),
            "kill spotify"
        );
        // A PID kill survives numerically.
        assert_eq!(
            appctl_args_to_flat(r#"{"action":"kill","target":"1234"}"#),
            "kill 1234"
        );
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(appctl_args_to_flat("focus firefox"), "focus firefox");
        assert_eq!(appctl_args_to_flat("kill 1234"), "kill 1234");
        // Unknown-verb JSON falls back to the raw string.
        assert_eq!(
            appctl_args_to_flat(r#"{"action":"nope"}"#),
            r#"{"action":"nope"}"#
        );
    }

    /// Per-verb drift test: every grammar verb's flat rendering must split
    /// into a verb execute's dispatch (and completions' verb list) accepts.
    #[test]
    fn appctl_grammar_flat_renderings_are_accepted_by_the_parser() {
        for v in APPCTL_GRAMMAR.verbs {
            let operand = v.operands[0].name;
            let flat = appctl_args_to_flat(&format!(
                r#"{{"action":"{}","{operand}":"firefox"}}"#,
                v.name
            ));
            let (verb, target) = parse_verb_and_target(&flat);
            assert!(
                ["focus", "quit", "close", "kill"].contains(&verb),
                "{verb:?} is not a dispatch verb"
            );
            assert_eq!(target, "firefox");
        }
    }
}

#[cfg(test)]
mod window_matching_tests {
    use super::*;

    fn win(wm_class: &str, title: &str) -> RunningWindow {
        RunningWindow {
            window_id: Some(1),
            kwin_id: None,
            wm_class: wm_class.into(),
            title: title.into(),
            pid: 1,
            desktop: None,
        }
    }

    /// F4: bare `contains` returned the FIRST window holding those letters, and
    /// enumeration order is arbitrary — so `quit code` could close QtCreator.
    /// An exact match must win regardless of where it sits in the list.
    #[test]
    fn an_exact_class_beats_an_incidental_substring() {
        let windows = vec![
            win("qtcreator", "main.cpp"),
            win("vscodium", "notes.md"),
            win("code", "lychi — executor.rs"),
        ];
        let found = find_window(&windows, "code").expect("should match");
        assert_eq!(found.wm_class, "code", "an exact class match must win");
    }

    /// Order must not decide the answer: same set, exact match placed first.
    #[test]
    fn the_winner_does_not_depend_on_enumeration_order() {
        let a = vec![win("code", "editor"), win("qtcreator", "main.cpp")];
        let b = vec![win("qtcreator", "main.cpp"), win("code", "editor")];
        assert_eq!(find_window(&a, "code").unwrap().wm_class, "code");
        assert_eq!(find_window(&b, "code").unwrap().wm_class, "code");
    }

    #[test]
    fn a_prefix_beats_a_mid_string_hit() {
        let windows = vec![win("vscodium", "x"), win("firefox", "y")];
        assert_eq!(find_window(&windows, "fire").unwrap().wm_class, "firefox");
    }

    /// The case only the EXACT tier can answer: a query that is a prefix of
    /// another window's class, and the exact name of this one. "code" is a
    /// prefix of "codium", so prefix-matching alone cannot separate them —
    /// deleting the exact tier makes this fail.
    #[test]
    fn an_exact_match_beats_a_prefix_of_a_longer_name() {
        // "code-insiders" genuinely starts with "code", so both score at the
        // prefix tier and only the exact tier separates them.
        let windows = vec![win("code-insiders", "notes.md"), win("code", "editor")];
        assert_eq!(
            find_window(&windows, "code").unwrap().wm_class,
            "code",
            "the window NAMED code must win over one merely starting with it"
        );
    }

    /// Reverse-DNS classes are common on Wayland; the leaf must match too.
    #[test]
    fn a_reverse_dns_class_matches_on_its_last_segment() {
        let windows = vec![win("org.gnome.Nautilus", "Home")];
        assert_eq!(
            find_window(&windows, "nautilus").unwrap().wm_class,
            "org.gnome.Nautilus"
        );
    }

    /// A whole word in the title is a real match; letters mid-word are not,
    /// and must not outrank it.
    #[test]
    fn a_title_word_beats_an_incidental_class_substring() {
        let windows = vec![win("vscodium", "scratch"), win("slack", "Slack | general")];
        assert_eq!(find_window(&windows, "slack").unwrap().wm_class, "slack");
    }

    /// Substring is still allowed — a partial recall should find something.
    #[test]
    fn a_substring_still_matches_when_nothing_better_exists() {
        let windows = vec![win("qtcreator", "main.cpp")];
        assert!(find_window(&windows, "creator").is_some());
        assert!(matches_window(&windows[0], "creator"));
    }

    #[test]
    fn no_match_is_none() {
        let windows = vec![win("firefox", "Mozilla Firefox")];
        assert!(find_window(&windows, "inkscape").is_none());
        assert!(!matches_window(&windows[0], "inkscape"));
    }
}
