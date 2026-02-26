use async_trait::async_trait;
use std::process::Command;
use std::time::Instant;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem, OutputType, RiskLevel};
use crate::error::LychiError;

pub struct SystemCommand;

impl SystemCommand {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SystemCommand {
    fn default() -> Self {
        Self::new()
    }
}

/// Actions that require confirmation (destructive / irreversible).
pub const DESTRUCTIVE_ACTIONS: &[&str] = &["shutdown", "reboot", "hibernate", "logout"];

/// Static action with no parameters.
struct SimpleAction {
    name: &'static str,
    description: &'static str,
    run: fn() -> Result<(), String>,
}

/// All simple (non-parameterized) actions.
fn simple_actions() -> &'static [SimpleAction] {
    &[
        // --- Power ---
        SimpleAction {
            name: "shutdown",
            description: "Power off the system",
            run: || run_cmd("systemctl", &["poweroff"]),
        },
        SimpleAction {
            name: "reboot",
            description: "Restart the system",
            run: || run_cmd("systemctl", &["reboot"]),
        },
        SimpleAction {
            name: "suspend",
            description: "Suspend (sleep) the system",
            run: || run_cmd("systemctl", &["suspend"]),
        },
        SimpleAction {
            name: "hibernate",
            description: "Hibernate the system",
            run: || run_cmd("systemctl", &["hibernate"]),
        },
        SimpleAction {
            name: "lock",
            description: "Lock the screen",
            run: || run_cmd("loginctl", &["lock-session"]),
        },
        SimpleAction {
            name: "logout",
            description: "Log out of the current session",
            run: || {
                if run_cmd("loginctl", &["terminate-user", &whoami()]).is_ok() {
                    return Ok(());
                }
                run_cmd("loginctl", &["terminate-session", "self"])
            },
        },
        // --- Audio ---
        SimpleAction {
            name: "mute",
            description: "Mute system audio",
            run: || run_cmd("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "1"]),
        },
        SimpleAction {
            name: "unmute",
            description: "Unmute system audio",
            run: || run_cmd("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "0"]),
        },
        // --- Network ---
        SimpleAction {
            name: "wifi on",
            description: "Enable WiFi",
            run: || run_cmd("nmcli", &["radio", "wifi", "on"]),
        },
        SimpleAction {
            name: "wifi off",
            description: "Disable WiFi",
            run: || run_cmd("nmcli", &["radio", "wifi", "off"]),
        },
        SimpleAction {
            name: "bluetooth on",
            description: "Enable Bluetooth",
            run: || run_cmd("rfkill", &["unblock", "bluetooth"]),
        },
        SimpleAction {
            name: "bluetooth off",
            description: "Disable Bluetooth",
            run: || run_cmd("rfkill", &["block", "bluetooth"]),
        },
    ]
}

/// Handle parameterized actions (volume/brightness with args).
/// Returns `Some(Ok/Err)` if matched, `None` if not a parameterized action.
fn try_parameterized(input: &str) -> Option<Result<String, String>> {
    let lower = input.to_lowercase();

    // --- Volume ---
    if lower == "volume up" {
        return Some(
            run_cmd("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%+"])
                .map(|()| read_volume()),
        );
    }
    if lower == "volume down" {
        return Some(
            run_cmd("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%-"])
                .map(|()| read_volume()),
        );
    }
    if let Some(rest) = lower.strip_prefix("volume ") {
        let rest = rest.trim();
        if let Some(n) = parse_percent(rest) {
            let frac = format!("{:.2}", n as f64 / 100.0);
            return Some(
                run_cmd("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &frac])
                    .map(|()| format!("Volume set to {n}%")),
            );
        }
    }

    // --- Brightness ---
    if lower == "brightness up" {
        return Some(run_cmd("brightnessctl", &["set", "10%+"]).map(|()| read_brightness()));
    }
    if lower == "brightness down" {
        return Some(run_cmd("brightnessctl", &["set", "10%-"]).map(|()| read_brightness()));
    }
    if let Some(rest) = lower.strip_prefix("brightness ") {
        let rest = rest.trim();
        if let Some(n) = parse_percent(rest) {
            let arg = format!("{n}%");
            return Some(
                run_cmd("brightnessctl", &["set", &arg])
                    .map(|()| format!("Brightness set to {n}%")),
            );
        }
    }

    None
}

/// Parse a percentage value from a string like "50", "50%", "100".
fn parse_percent(s: &str) -> Option<u32> {
    let s = s.trim_end_matches('%').trim();
    let n: u32 = s.parse().ok()?;
    if n <= 150 {
        // Allow up to 150% for volume (wpctl supports it)
        Some(n)
    } else {
        None
    }
}

/// Read current volume via wpctl.
fn read_volume() -> String {
    match run_cmd_output("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]) {
        Ok(out) => {
            // Output like "Volume: 0.50" or "Volume: 0.50 [MUTED]"
            let trimmed = out.trim();
            if let Some(rest) = trimmed.strip_prefix("Volume: ") {
                let parts: Vec<&str> = rest.split_whitespace().collect();
                if let Some(val) = parts.first()
                    && let Ok(f) = val.parse::<f64>()
                {
                    let pct = (f * 100.0).round() as u32;
                    let muted = if rest.contains("[MUTED]") {
                        " (muted)"
                    } else {
                        ""
                    };
                    return format!("Volume: {pct}%{muted}");
                }
            }
            "Volume changed".to_string()
        }
        Err(_) => "Volume changed".to_string(),
    }
}

/// Read current brightness via brightnessctl.
fn read_brightness() -> String {
    match run_cmd_output("brightnessctl", &["info"]) {
        Ok(out) => {
            // Look for "Current brightness: 1234 (56%)"
            for line in out.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("Current brightness:")
                    && let Some(start) = trimmed.rfind('(')
                    && let Some(end) = trimmed.rfind(')')
                {
                    return format!("Brightness: {}", &trimmed[start + 1..end]);
                }
            }
            "Brightness changed".to_string()
        }
        Err(_) => "Brightness changed".to_string(),
    }
}

fn run_cmd(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run {program}: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("{program} failed: {}", stderr.trim()))
    }
}

fn run_cmd_output(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run {program}: {e}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("{program} failed: {}", stderr.trim()))
    }
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default()
}

/// All action names for completions (simple + parameterized).
const ALL_ACTION_NAMES: &[(&str, &str)] = &[
    ("shutdown", "Power off the system"),
    ("reboot", "Restart the system"),
    ("suspend", "Suspend (sleep) the system"),
    ("hibernate", "Hibernate the system"),
    ("lock", "Lock the screen"),
    ("logout", "Log out of the current session"),
    ("mute", "Mute system audio"),
    ("unmute", "Unmute system audio"),
    ("volume up", "Increase volume by 5%"),
    ("volume down", "Decrease volume by 5%"),
    ("volume <n>", "Set volume to n%"),
    ("brightness up", "Increase brightness by 10%"),
    ("brightness down", "Decrease brightness by 10%"),
    ("brightness <n>", "Set brightness to n%"),
    ("wifi on", "Enable WiFi"),
    ("wifi off", "Disable WiFi"),
    ("bluetooth on", "Enable Bluetooth"),
    ("bluetooth off", "Disable Bluetooth"),
];

#[async_trait]
impl ActionHandler for SystemCommand {
    fn id(&self) -> &str {
        "system"
    }

    fn description(&self) -> &str {
        "System controls (power, audio, brightness, wifi, bluetooth)"
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let input = args.trim();
        let start = Instant::now();

        if input.is_empty() {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some(
                    "Usage: system <action>. Try: shutdown, mute, volume 50, brightness up, wifi off"
                        .to_string(),
                ),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            });
        }

        // Try parameterized actions first (volume N, brightness N)
        if let Some(result) = try_parameterized(input) {
            let duration_ms = start.elapsed().as_millis() as u64;
            return match result {
                Ok(msg) => Ok(ActionResult {
                    success: true,
                    output: Some(msg),
                    error: None,
                    duration_ms,
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
                    error: Some(e),
                    duration_ms,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: None,
                    executed_args: None,
                }),
            };
        }

        // Try simple actions (exact name match)
        let action_name = input.to_lowercase();
        let action = simple_actions().iter().find(|a| a.name == action_name);

        match action {
            Some(a) => {
                let result = (a.run)();
                let duration_ms = start.elapsed().as_millis() as u64;
                match result {
                    Ok(()) => Ok(ActionResult {
                        success: true,
                        output: Some(format!("{} initiated", a.description)),
                        error: None,
                        duration_ms,
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
                        error: Some(e),
                        duration_ms,
                        routed_by: None,
                        open_url: None,
                        needs_confirmation: None,
                        risk_level: None,
                        output_type: None,
                        executed_args: None,
                    }),
                }
            }
            None => Ok(ActionResult {
                success: false,
                output: None,
                error: Some(format!(
                    "Unknown action '{input}'. Try: shutdown, mute, volume 50, brightness up, wifi off"
                )),
                duration_ms: start.elapsed().as_millis() as u64,
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
        let lower = partial.to_lowercase();
        ALL_ACTION_NAMES
            .iter()
            .filter(|(name, _)| name.contains(&lower) || lower.is_empty())
            .map(|(name, desc)| CompletionItem {
                label: name.to_string(),
                icon_path: None,
                score: if name.starts_with(&lower) { 100 } else { 50 },
                description: Some(desc.to_string()),
                reason: None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_percent() {
        assert_eq!(parse_percent("50"), Some(50));
        assert_eq!(parse_percent("50%"), Some(50));
        assert_eq!(parse_percent("0"), Some(0));
        assert_eq!(parse_percent("100"), Some(100));
        assert_eq!(parse_percent("150"), Some(150));
        assert_eq!(parse_percent("151"), None);
        assert_eq!(parse_percent("abc"), None);
        assert_eq!(parse_percent(""), None);
    }

    #[test]
    fn test_parameterized_matches() {
        // Volume up/down should match
        assert!(try_parameterized("volume up").is_some());
        assert!(try_parameterized("volume down").is_some());
        assert!(try_parameterized("Volume Up").is_some());

        // Volume N should match
        assert!(try_parameterized("volume 50").is_some());
        assert!(try_parameterized("volume 50%").is_some());

        // Brightness up/down should match
        assert!(try_parameterized("brightness up").is_some());
        assert!(try_parameterized("brightness down").is_some());

        // Brightness N should match
        assert!(try_parameterized("brightness 80").is_some());

        // Non-parameterized should not match
        assert!(try_parameterized("mute").is_none());
        assert!(try_parameterized("shutdown").is_none());
        assert!(try_parameterized("wifi on").is_none());
    }

    #[test]
    fn test_simple_action_names() {
        let actions = simple_actions();
        let names: Vec<&str> = actions.iter().map(|a| a.name).collect();
        assert!(names.contains(&"shutdown"));
        assert!(names.contains(&"mute"));
        assert!(names.contains(&"unmute"));
        assert!(names.contains(&"wifi on"));
        assert!(names.contains(&"wifi off"));
        assert!(names.contains(&"bluetooth on"));
        assert!(names.contains(&"bluetooth off"));
    }

    #[test]
    fn test_destructive_actions() {
        // Destructive actions should all be in the simple actions list
        for &name in DESTRUCTIVE_ACTIONS {
            assert!(
                simple_actions().iter().any(|a| a.name == name),
                "Destructive action '{name}' not found in simple_actions"
            );
        }

        // Non-destructive actions should NOT be in the destructive list
        assert!(!DESTRUCTIVE_ACTIONS.contains(&"mute"));
        assert!(!DESTRUCTIVE_ACTIONS.contains(&"wifi on"));
        assert!(!DESTRUCTIVE_ACTIONS.contains(&"brightness up"));
        assert!(!DESTRUCTIVE_ACTIONS.contains(&"lock"));
    }
}
