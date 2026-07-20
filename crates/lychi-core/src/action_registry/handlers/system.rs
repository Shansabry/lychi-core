use async_trait::async_trait;
use std::process::Command;
use std::time::Instant;

use crate::action_registry::{
    ActionHandler, ActionResult, CompletionItem, ExecContext, OutputType, RiskAssessment, RiskLevel,
};
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

/// Parse a scheduled shutdown like "shutdown in 30 minutes", "shutdown in 1 hour".
/// Returns the number of minutes, or None if not matched.
fn parse_shutdown_in(input: &str) -> Option<u32> {
    let lower = input.to_lowercase();
    // Match: "shutdown in <n> [minutes|mins|min|m|hours|hour|hrs|hr|h]"
    let rest = lower
        .strip_prefix("shutdown in ")
        .or_else(|| lower.strip_prefix("shut down in "))?;
    let rest = rest.trim();

    // Try "<n> <unit>" or just "<n>" (default minutes)
    let (num_str, unit) = if let Some(pos) = rest.find(|c: char| !c.is_ascii_digit()) {
        let (n, u) = rest.split_at(pos);
        (n.trim(), u.trim())
    } else {
        (rest, "")
    };

    let n: u32 = num_str.parse().ok()?;
    if n == 0 {
        return None;
    }

    let minutes = match unit {
        "" | "m" | "min" | "mins" | "minute" | "minutes" => n,
        "h" | "hr" | "hrs" | "hour" | "hours" => n.checked_mul(60)?,
        _ => return None,
    };

    Some(minutes)
}

/// A paired Bluetooth device (MAC + name).
struct BtDevice {
    mac: String,
    name: String,
}

/// List paired Bluetooth devices via `bluetoothctl devices Paired`.
fn list_bt_devices() -> Vec<BtDevice> {
    let output = match run_cmd_output("bluetoothctl", &["devices", "Paired"]) {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    // Each line: "Device AA:BB:CC:DD:EE:FF Device Name"
    output
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("Device ")?;
            let (mac, name) = rest.split_once(' ')?;
            Some(BtDevice {
                mac: mac.to_string(),
                name: name.trim().to_string(),
            })
        })
        .collect()
}

/// Check if a device is currently connected.
fn bt_device_connected(mac: &str) -> bool {
    if let Ok(info) = run_cmd_output("bluetoothctl", &["info", mac]) {
        info.lines()
            .any(|l| l.trim().starts_with("Connected:") && l.contains("yes"))
    } else {
        false
    }
}

/// Fuzzy match a device name (case-insensitive substring).
fn find_bt_device<'a>(devices: &'a [BtDevice], query: &str) -> Option<&'a BtDevice> {
    let q = query.to_lowercase();
    // Exact match first
    if let Some(d) = devices.iter().find(|d| d.name.to_lowercase() == q) {
        return Some(d);
    }
    // Substring match
    devices.iter().find(|d| d.name.to_lowercase().contains(&q))
}

/// Handle `connect bluetooth <device>` / `disconnect bluetooth <device>`.
/// Returns `Some(Ok/Err)` if matched, `None` if not a bluetooth connect/disconnect command.
fn try_bluetooth_connect(input: &str) -> Option<Result<String, String>> {
    let lower = input.to_lowercase();

    let (action, query) = if let Some(q) = lower
        .strip_prefix("connect bluetooth ")
        .or_else(|| lower.strip_prefix("connect bt "))
        .or_else(|| lower.strip_prefix("bluetooth connect "))
        .or_else(|| lower.strip_prefix("bt connect "))
    {
        ("connect", q.trim())
    } else if let Some(q) = lower
        .strip_prefix("disconnect bluetooth ")
        .or_else(|| lower.strip_prefix("disconnect bt "))
        .or_else(|| lower.strip_prefix("bluetooth disconnect "))
        .or_else(|| lower.strip_prefix("bt disconnect "))
    {
        ("disconnect", q.trim())
    } else {
        return None;
    };

    if query.is_empty() {
        return Some(Err(
            "No device name specified. Try: connect bluetooth <device name>".into(),
        ));
    }

    let devices = list_bt_devices();
    if devices.is_empty() {
        return Some(Err("No paired Bluetooth devices found".into()));
    }

    let device = match find_bt_device(&devices, query) {
        Some(d) => d,
        None => {
            let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
            return Some(Err(format!(
                "No paired device matching '{}'. Available: {}",
                query,
                names.join(", ")
            )));
        }
    };

    let result = run_cmd("bluetoothctl", &[action, &device.mac]);
    Some(match result {
        Ok(()) => Ok(format!(
            "{}ed {}",
            if action == "connect" {
                "Connect"
            } else {
                "Disconnect"
            },
            device.name
        )),
        Err(e) => Err(format!("Failed to {} {}: {}", action, device.name, e)),
    })
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

    // --- Scheduled shutdown ---
    if let Some(minutes) = parse_shutdown_in(input) {
        return Some(run_cmd("shutdown", &[&format!("+{minutes}")]).map(|()| {
            if minutes >= 60 && minutes % 60 == 0 {
                format!(
                    "Shutdown scheduled in {} hour{}",
                    minutes / 60,
                    if minutes / 60 == 1 { "" } else { "s" }
                )
            } else {
                format!(
                    "Shutdown scheduled in {minutes} minute{}",
                    if minutes == 1 { "" } else { "s" }
                )
            }
        }));
    }

    // --- Cancel scheduled shutdown ---
    if lower == "cancel shutdown" || lower == "shutdown cancel" {
        return Some(
            run_cmd("shutdown", &["-c"]).map(|()| "Scheduled shutdown cancelled".to_string()),
        );
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
    ("shutdown in <n> minutes", "Schedule shutdown in n minutes"),
    ("cancel shutdown", "Cancel a scheduled shutdown"),
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
    (
        "connect bluetooth <device>",
        "Connect to a paired Bluetooth device",
    ),
    (
        "disconnect bluetooth <device>",
        "Disconnect a Bluetooth device",
    ),
];

#[async_trait]
impl ActionHandler for SystemCommand {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::{ArgTransform, Trigger};
        static TRIGGERS: &[Trigger] = &[
            Trigger::keywords(&["system"]),
            // Bare power words. `shutdown`/`poweroff` with trailing args are
            // intercepted structurally in patterns.rs; a bare word routes here.
            Trigger::new(&["shutdown", "poweroff"], ArgTransform::Fixed("shutdown")),
            Trigger::new(&["reboot", "restart"], ArgTransform::Fixed("reboot")),
            Trigger::new(&["lock"], ArgTransform::Fixed("lock")),
            Trigger::new(&["suspend", "sleep"], ArgTransform::Fixed("suspend")),
            Trigger::new(&["hibernate"], ArgTransform::Fixed("hibernate")),
            Trigger::new(&["logout", "signout"], ArgTransform::Fixed("logout")),
            Trigger::new(&["mute"], ArgTransform::Fixed("mute")),
            Trigger::new(&["unmute"], ArgTransform::Fixed("unmute")),
        ];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "system"
    }

    fn description(&self) -> &str {
        "System controls (power, audio, brightness, wifi, bluetooth)"
    }

    fn assess_risk(&self, args: &str) -> RiskAssessment {
        // Only destructive actions (shutdown, reboot, hibernate, logout) need
        // confirmation. Reversible toggles (mute, volume, brightness, wifi,
        // bluetooth) auto-execute. This ownership lives here, not in the Rules
        // Engine.
        let action = args.trim().to_lowercase();
        if DESTRUCTIVE_ACTIONS.iter().any(|d| action.starts_with(d)) {
            RiskAssessment::confirm(format!(
                "System action '{}' requires confirmation",
                args.trim()
            ))
        } else {
            RiskAssessment::level(RiskLevel::Low)
        }
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let input = args.trim();
        let start = Instant::now();

        if input.is_empty() {
            return Ok(ActionResult::err(
                "Usage: system <action>. Try: shutdown, mute, volume 50, brightness up, wifi off"
                    .to_string(),
            ));
        }

        // Try bluetooth connect/disconnect
        if let Some(result) = try_bluetooth_connect(input) {
            let duration_ms = start.elapsed().as_millis() as u64;
            return match result {
                Ok(msg) => Ok(ActionResult::ok(msg, OutputType::Status).with_duration(duration_ms)),
                Err(e) => Ok(ActionResult::err(e).with_duration(duration_ms)),
            };
        }

        // Try parameterized actions (volume N, brightness N, shutdown in N)
        if let Some(result) = try_parameterized(input) {
            let duration_ms = start.elapsed().as_millis() as u64;
            return match result {
                Ok(msg) => Ok(ActionResult::ok(msg, OutputType::Status).with_duration(duration_ms)),
                Err(e) => Ok(ActionResult::err(e).with_duration(duration_ms)),
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
                    Ok(()) => Ok(ActionResult::ok(
                        format!("{} initiated", a.description),
                        OutputType::Status,
                    )
                    .with_duration(duration_ms)),
                    Err(e) => Ok(ActionResult::err(e).with_duration(duration_ms)),
                }
            }
            None => Ok(ActionResult::err(format!(
                "Unknown action '{input}'. Try: shutdown, mute, volume 50, brightness up, wifi off"
            ))
            .with_duration(start.elapsed().as_millis() as u64)),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let lower = partial.to_lowercase();
        let mut items: Vec<CompletionItem> = ALL_ACTION_NAMES
            .iter()
            .filter(|(name, _)| name.contains(&lower) || lower.is_empty())
            .map(|(name, desc)| CompletionItem {
                label: name.to_string(),
                icon_path: None,
                score: if name.starts_with(&lower) { 100 } else { 50 },
                description: Some(desc.to_string()),
                reason: None,
                thumb_b64: None,
                // Names with a "<…>" placeholder (e.g. "volume <n>") need a
                // value: selecting them fills the input up to the placeholder
                // so the user types the value, then Enter runs it
                // (tab-to-complete). Concrete actions run immediately.
                run: if name.contains('<') {
                    None
                } else {
                    Some(format!("system {name}"))
                },
                fill: name.find('<').map(|i| format!("system {}", &name[..i])),
                ..Default::default()
            })
            .collect();

        // Show paired devices for bluetooth connect/disconnect queries
        let bt_prefix = if lower.starts_with("connect bluetooth")
            || lower.starts_with("connect bt")
            || lower.starts_with("bluetooth connect")
        {
            Some("connect bluetooth")
        } else if lower.starts_with("disconnect bluetooth")
            || lower.starts_with("disconnect bt")
            || lower.starts_with("bluetooth disconnect")
        {
            Some("disconnect bluetooth")
        } else {
            None
        };

        if let Some(action) = bt_prefix {
            let devices = list_bt_devices();
            for dev in &devices {
                let connected = bt_device_connected(&dev.mac);
                let status = if connected { "connected" } else { "paired" };
                items.push(CompletionItem {
                    label: format!("{action} {}", dev.name),
                    icon_path: None,
                    score: 90,
                    description: Some(format!("{} ({})", dev.name, status)),
                    reason: None,
                    thumb_b64: None,
                    run: Some(format!("system {action} {}", dev.name)),
                    ..Default::default()
                });
            }
        }

        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assess_risk_confirms_destructive_auto_executes_reversible() {
        let h = SystemCommand::new();
        // Destructive → confirm (Medium + custom message).
        for a in ["shutdown", "reboot", "hibernate", "logout"] {
            assert_eq!(
                h.assess_risk(a).level,
                RiskLevel::Medium,
                "{a} should confirm"
            );
        }
        // Reversible toggles → auto-execute (Low).
        for a in [
            "mute",
            "unmute",
            "volume up",
            "brightness 50",
            "wifi on",
            "bluetooth off",
            "lock",
            "suspend",
        ] {
            assert_eq!(
                h.assess_risk(a).level,
                RiskLevel::Low,
                "{a} should auto-execute"
            );
        }
    }

    #[tokio::test]
    async fn placeholder_actions_fill_input_concrete_actions_run() {
        let items = SystemCommand::new().completions("").await;
        assert!(!items.is_empty());
        for item in &items {
            if item.label.contains('<') {
                // Argument-needing hints like "volume <n>" fill the input up to
                // the placeholder (tab-to-complete), not run.
                assert!(
                    item.run.is_none(),
                    "placeholder action must not run directly: {}",
                    item.label
                );
                let fill = item.fill.as_deref().expect("placeholder has a fill");
                assert!(
                    fill.starts_with("system ") && !fill.contains('<'),
                    "fill must be a clean runnable prefix: {fill}"
                );
            } else {
                // Concrete actions run as `system <name>`.
                assert_eq!(
                    item.run.as_deref(),
                    Some(format!("system {}", item.label).as_str())
                );
                assert!(item.fill.is_none());
            }
        }
    }

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

    #[test]
    fn test_parse_shutdown_in() {
        // Minutes
        assert_eq!(parse_shutdown_in("shutdown in 30 minutes"), Some(30));
        assert_eq!(parse_shutdown_in("shutdown in 30 mins"), Some(30));
        assert_eq!(parse_shutdown_in("shutdown in 30 min"), Some(30));
        assert_eq!(parse_shutdown_in("shutdown in 30m"), Some(30));
        assert_eq!(parse_shutdown_in("shutdown in 30"), Some(30));
        assert_eq!(parse_shutdown_in("shutdown in 1 minute"), Some(1));

        // Hours
        assert_eq!(parse_shutdown_in("shutdown in 1 hour"), Some(60));
        assert_eq!(parse_shutdown_in("shutdown in 2 hours"), Some(120));
        assert_eq!(parse_shutdown_in("shutdown in 1h"), Some(60));
        assert_eq!(parse_shutdown_in("shutdown in 2hr"), Some(120));

        // "shut down" variant
        assert_eq!(parse_shutdown_in("shut down in 30 minutes"), Some(30));
        assert_eq!(parse_shutdown_in("shut down in 1 hour"), Some(60));

        // Case insensitive
        assert_eq!(parse_shutdown_in("Shutdown In 30 Minutes"), Some(30));

        // Edge cases
        assert_eq!(parse_shutdown_in("shutdown in 0 minutes"), None); // 0 not allowed
        assert_eq!(parse_shutdown_in("shutdown in abc"), None);
        assert_eq!(parse_shutdown_in("shutdown"), None);
        assert_eq!(parse_shutdown_in("shutdown in"), None);
    }

    #[test]
    fn test_scheduled_shutdown_parameterized() {
        // Scheduled shutdown should match as parameterized
        assert!(try_parameterized("shutdown in 30 minutes").is_some());
        assert!(try_parameterized("shutdown in 1 hour").is_some());
        assert!(try_parameterized("shut down in 30 minutes").is_some());

        // Cancel shutdown should match
        assert!(try_parameterized("cancel shutdown").is_some());
        assert!(try_parameterized("shutdown cancel").is_some());
    }

    #[test]
    fn test_bluetooth_connect_parsing() {
        // All prefix variants should match
        assert!(try_bluetooth_connect("connect bluetooth speaker").is_some());
        assert!(try_bluetooth_connect("connect bt speaker").is_some());
        assert!(try_bluetooth_connect("bluetooth connect speaker").is_some());
        assert!(try_bluetooth_connect("bt connect speaker").is_some());
        assert!(try_bluetooth_connect("disconnect bluetooth speaker").is_some());
        assert!(try_bluetooth_connect("disconnect bt speaker").is_some());
        assert!(try_bluetooth_connect("bluetooth disconnect speaker").is_some());
        assert!(try_bluetooth_connect("bt disconnect speaker").is_some());

        // Empty device name should not match
        assert!(try_bluetooth_connect("connect bluetooth ").is_some()); // gets "No device name" error
        assert!(try_bluetooth_connect("mute").is_none());
        assert!(try_bluetooth_connect("bluetooth on").is_none());
    }

    #[test]
    fn test_find_bt_device() {
        let devices = vec![
            BtDevice {
                mac: "AA:BB:CC:DD:EE:FF".into(),
                name: "Mi Portable BT Speaker 16W".into(),
            },
            BtDevice {
                mac: "11:22:33:44:55:66".into(),
                name: "AirPods Pro".into(),
            },
        ];

        // Exact match
        let d = find_bt_device(&devices, "AirPods Pro");
        assert!(d.is_some());
        assert_eq!(d.unwrap().mac, "11:22:33:44:55:66");

        // Substring match
        let d = find_bt_device(&devices, "speaker");
        assert!(d.is_some());
        assert_eq!(d.unwrap().mac, "AA:BB:CC:DD:EE:FF");

        // Case insensitive
        let d = find_bt_device(&devices, "airpods");
        assert!(d.is_some());

        // No match
        assert!(find_bt_device(&devices, "headphones").is_none());
    }
}
