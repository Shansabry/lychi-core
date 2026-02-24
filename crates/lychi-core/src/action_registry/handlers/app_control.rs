use std::process::Command;
use std::sync::Mutex;
use std::time::Instant;

use async_trait::async_trait;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem, OutputType, RiskLevel};
use crate::error::LychiError;

#[cfg(target_os = "linux")]
use super::kwin_windows;
#[cfg(target_os = "linux")]
use super::x11_windows;

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
struct RunningWindow {
    /// X11 window ID (only set on X11 sessions)
    window_id: Option<u32>,
    /// Window title
    title: String,
    /// WM class or app name (lowercase)
    wm_class: String,
    /// Process ID
    pid: u32,
}

/// Cached window list with TTL.
struct WindowCache {
    windows: Vec<RunningWindow>,
    fetched_at: Instant,
}

static WINDOW_CACHE: Mutex<Option<WindowCache>> = Mutex::new(None);
const CACHE_TTL_SECS: u64 = 2;

/// Detect session type from XDG_SESSION_TYPE.
#[cfg(target_os = "linux")]
fn is_wayland() -> bool {
    std::env::var("XDG_SESSION_TYPE")
        .map(|v| v == "wayland")
        .unwrap_or(false)
}

/// Get the list of running windows, using cache if fresh.
fn get_windows() -> Vec<RunningWindow> {
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
/// Wayland (KDE) → KWin D-Bus scripting (sees all windows).
/// X11 → native EWMH protocol via x11rb.
#[cfg(target_os = "linux")]
fn enumerate_windows() -> Vec<RunningWindow> {
    let wayland = is_wayland();
    tracing::info!("appctl: enumerate_windows (wayland={wayland})");
    if wayland {
        kwin_windows::enumerate_windows()
            .into_iter()
            .map(|w| RunningWindow {
                window_id: None,
                title: w.caption,
                wm_class: w.resource_class,
                pid: w.pid,
            })
            .collect()
    } else {
        x11_windows::enumerate_windows()
            .into_iter()
            .map(|w| RunningWindow {
                window_id: Some(w.window_id),
                title: w.title,
                wm_class: w.wm_class,
                pid: w.pid,
            })
            .collect()
    }
}

#[cfg(not(target_os = "linux"))]
fn enumerate_windows() -> Vec<RunningWindow> {
    Vec::new()
}

/// Fuzzy match a query against a window's title and class.
fn matches_window(window: &RunningWindow, query: &str) -> bool {
    let query_lower = query.to_lowercase();
    window.wm_class.contains(&query_lower) || window.title.to_lowercase().contains(&query_lower)
}

/// Find the best matching window for a query.
fn find_window<'a>(windows: &'a [RunningWindow], query: &str) -> Option<&'a RunningWindow> {
    let query_lower = query.to_lowercase();

    // Exact class match first
    if let Some(w) = windows.iter().find(|w| w.wm_class == query_lower) {
        return Some(w);
    }

    // Substring match on class
    if let Some(w) = windows.iter().find(|w| w.wm_class.contains(&query_lower)) {
        return Some(w);
    }

    // Substring match on title
    windows
        .iter()
        .find(|w| w.title.to_lowercase().contains(&query_lower))
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

/// Focus a window natively.
#[cfg(target_os = "linux")]
fn do_focus(window: &RunningWindow) -> Result<(), String> {
    if is_wayland() {
        kwin_windows::focus_window(&window.wm_class)
    } else if let Some(wid) = window.window_id {
        x11_windows::focus_window(wid)
    } else {
        Err("No window ID available".to_string())
    }
}

#[cfg(not(target_os = "linux"))]
fn do_focus(_window: &RunningWindow) -> Result<(), String> {
    Err("Window focus not supported on this platform".to_string())
}

/// Gracefully close a window natively.
#[cfg(target_os = "linux")]
fn do_close(window: &RunningWindow) -> Result<(), String> {
    if is_wayland() {
        kwin_windows::close_window(&window.wm_class)
    } else if let Some(wid) = window.window_id {
        x11_windows::close_window(wid)
    } else {
        Err("No window ID available".to_string())
    }
}

#[cfg(not(target_os = "linux"))]
fn do_close(_window: &RunningWindow) -> Result<(), String> {
    Err("Window close not supported on this platform".to_string())
}

#[async_trait]
impl ActionHandler for AppControlHandler {
    fn id(&self) -> &str {
        "appctl"
    }

    fn description(&self) -> &str {
        "Focus, quit, or kill running applications"
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let (verb, target) = parse_verb_and_target(args);

        if target.is_empty() {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some(format!(
                    "Usage: {verb} <app name>. Try 'focus firefox' or 'quit code'."
                )),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            });
        }

        let windows = get_windows();
        let window = match find_window(&windows, target) {
            Some(w) => w,
            None => {
                return Ok(ActionResult {
                    success: false,
                    output: None,
                    error: Some(format!("No running window matching '{target}'")),
                    duration_ms: 0,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: None,
                    executed_args: None,
                });
            }
        };

        match verb {
            "focus" => match do_focus(window) {
                Ok(()) => Ok(ActionResult {
                    success: true,
                    output: Some(format!("Focused: {}", window.title)),
                    error: None,
                    duration_ms: 0,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: Some(OutputType::Status),
                    executed_args: None,
                }),
                Err(e) => Ok(ActionResult {
                    success: false,
                    output: None,
                    error: Some(format!("Failed to focus '{}': {e}", window.title)),
                    duration_ms: 0,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: None,
                    executed_args: None,
                }),
            },
            "quit" | "close" => match do_close(window) {
                Ok(()) => Ok(ActionResult {
                    success: true,
                    output: Some(format!("Closed: {}", window.title)),
                    error: None,
                    duration_ms: 0,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: Some(OutputType::Status),
                    executed_args: None,
                }),
                Err(e) => Ok(ActionResult {
                    success: false,
                    output: None,
                    error: Some(format!("Failed to close '{}': {e}", window.title)),
                    duration_ms: 0,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: None,
                    executed_args: None,
                }),
            },
            "kill" => {
                let status = Command::new("kill")
                    .args(["-9", &window.pid.to_string()])
                    .status();

                match status {
                    Ok(s) if s.success() => Ok(ActionResult {
                        success: true,
                        output: Some(format!("Killed: {} (PID {})", window.title, window.pid)),
                        error: None,
                        duration_ms: 0,
                        routed_by: None,
                        open_url: None,
                        needs_confirmation: None,
                        risk_level: Some(RiskLevel::Medium),
                        output_type: Some(OutputType::Status),
                        executed_args: None,
                    }),
                    _ => Ok(ActionResult {
                        success: false,
                        output: None,
                        error: Some(format!(
                            "Failed to kill '{}' (PID {})",
                            window.title, window.pid
                        )),
                        duration_ms: 0,
                        routed_by: None,
                        open_url: None,
                        needs_confirmation: None,
                        risk_level: None,
                        output_type: None,
                        executed_args: None,
                    }),
                }
            }
            _ => Ok(ActionResult {
                success: false,
                output: None,
                error: Some(format!("Unknown verb '{verb}'. Use focus, quit, or kill.")),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            }),
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
                },
                CompletionItem {
                    label: "quit <app>".to_string(),
                    icon_path: None,
                    score: 800,
                    description: Some("Gracefully close".to_string()),
                },
                CompletionItem {
                    label: "kill <app>".to_string(),
                    icon_path: None,
                    score: 700,
                    description: Some("Force-kill process".to_string()),
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
                        label: display_name,
                        icon_path: None,
                        score: (1000 - i as u16).max(1),
                        description: Some(truncate(&w.title, 50)),
                    }
                })
                .collect();
        }

        // Fuzzy filter
        let target_lower = target.to_lowercase();
        windows
            .iter()
            .filter(|w| matches_window(w, &target_lower))
            .enumerate()
            .map(|(i, w)| {
                let display_name = if w.wm_class.is_empty() {
                    w.title.clone()
                } else {
                    w.wm_class.clone()
                };
                CompletionItem {
                    label: display_name,
                    icon_path: None,
                    score: (1000 - i as u16).max(1),
                    description: Some(truncate(&w.title, 50)),
                }
            })
            .collect()
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}
