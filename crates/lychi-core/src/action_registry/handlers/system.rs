use async_trait::async_trait;
use std::process::Command;
use std::time::Instant;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
    RiskAssessment, RiskLevel,
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

/// Actions that require confirmation (destructive / irreversible). Aliased to
/// the central classifier so the list has a single audit surface
/// ([`crate::rules::verbs`]); this name is kept for local/test use.
pub const DESTRUCTIVE_ACTIONS: &[&str] = crate::rules::verbs::DESTRUCTIVE_SYSTEM_ACTIONS;

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
        // Chain: systemd (`systemctl`) → logind (`loginctl`, also on elogind
        // systems w/o systemd) → sysvinit/util-linux binaries. Covers
        // Void/Artix/Alpine/Devuan, not just systemd distros.
        SimpleAction {
            name: "shutdown",
            description: "Power off the system",
            run: || {
                run_first_available(&[
                    ("systemctl", &["poweroff"]),
                    ("loginctl", &["poweroff"]),
                    ("poweroff", &[]),
                ])
            },
        },
        SimpleAction {
            name: "reboot",
            description: "Restart the system",
            run: || {
                run_first_available(&[
                    ("systemctl", &["reboot"]),
                    ("loginctl", &["reboot"]),
                    ("reboot", &[]),
                ])
            },
        },
        SimpleAction {
            name: "suspend",
            description: "Suspend (sleep) the system",
            run: || run_first_available(&[("systemctl", &["suspend"]), ("loginctl", &["suspend"])]),
        },
        SimpleAction {
            name: "hibernate",
            description: "Hibernate the system",
            run: || {
                run_first_available(&[("systemctl", &["hibernate"]), ("loginctl", &["hibernate"])])
            },
        },
        SimpleAction {
            name: "lock",
            description: "Lock the screen",
            // `loginctl lock-session` emits the logind lock signal the DE's
            // locker listens for. `xdg-screensaver lock` is a portable X11
            // fallback for bare WMs without logind lock wiring.
            run: || {
                run_first_available(&[
                    ("loginctl", &["lock-session"]),
                    ("xdg-screensaver", &["lock"]),
                ])
            },
        },
        SimpleAction {
            name: "logout",
            description: "Log out of the current session",
            run: logout_session,
        },
        // --- Audio ---
        // Chain: PipeWire (`wpctl`) → PulseAudio (`pactl`) → ALSA (`amixer`).
        // PulseAudio is still default on many stable/LTS distros.
        SimpleAction {
            name: "mute",
            description: "Mute system audio",
            run: || {
                run_first_available(&[
                    ("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "1"]),
                    ("pactl", &["set-sink-mute", "@DEFAULT_SINK@", "1"]),
                    ("amixer", &["set", "Master", "mute"]),
                ])
            },
        },
        SimpleAction {
            name: "unmute",
            description: "Unmute system audio",
            run: || {
                run_first_available(&[
                    ("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "0"]),
                    ("pactl", &["set-sink-mute", "@DEFAULT_SINK@", "0"]),
                    ("amixer", &["set", "Master", "unmute"]),
                ])
            },
        },
        // --- Network ---
        // Chain: NetworkManager (`nmcli`) → kernel `rfkill` (network-manager
        // independent; works on iwd/networkd/connman systems).
        SimpleAction {
            name: "wifi on",
            description: "Enable WiFi",
            run: || {
                run_first_available(&[
                    ("nmcli", &["radio", "wifi", "on"]),
                    ("rfkill", &["unblock", "wifi"]),
                ])
            },
        },
        SimpleAction {
            name: "wifi off",
            description: "Disable WiFi",
            run: || {
                run_first_available(&[
                    ("nmcli", &["radio", "wifi", "off"]),
                    ("rfkill", &["block", "wifi"]),
                ])
            },
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
#[derive(Clone)]
struct BtDevice {
    mac: String,
    name: String,
    connected: bool,
}

/// Paired devices plus which are connected, cached briefly.
///
/// This runs from `completions()`, i.e. on **every keystroke**. It used to spawn
/// `bluetoothctl devices Paired` and then `bluetoothctl info <mac>` once per
/// device — N+1 subprocesses per keypress, synchronously, on a tokio worker.
/// Measured at ~7.5ms per spawn: 15ms with one paired device, ~68ms with eight,
/// and typing "connect bluetooth" (17 keystrokes) cost 0.26-0.77s of pure
/// process spawning.
///
/// Two fixes. The per-device `info` call is replaced by one
/// `bluetoothctl devices Connected` — the same answer in bulk, so the cost is
/// 2 subprocesses regardless of how many devices are paired. And the result is
/// cached, so a burst of keystrokes spawns nothing at all.
struct BtCache {
    devices: Vec<BtDevice>,
    fetched_at: Instant,
}

static BT_CACHE: std::sync::Mutex<Option<BtCache>> = std::sync::Mutex::new(None);

/// Short enough that plugging in headphones shows up while you are still
/// typing; long enough that a burst of keystrokes costs nothing. Matches the
/// window-list cache in `app_control`.
const BT_CACHE_TTL_SECS: u64 = 2;

/// MAC addresses from `bluetoothctl devices …` output.
///
/// Shared by the paired and connected queries: same command, same line format
/// (`Device AA:BB:CC:DD:EE:FF Name`), so parsing it twice would be two things
/// to keep in step.
fn parse_bt_macs(output: &str) -> std::collections::HashSet<String> {
    output
        .lines()
        .filter_map(|l| l.strip_prefix("Device ")?.split_once(' '))
        .map(|(mac, _)| mac.to_string())
        .collect()
}

/// List paired Bluetooth devices via `bluetoothctl devices Paired`.
fn list_bt_devices() -> Vec<BtDevice> {
    if let Ok(cache) = BT_CACHE.lock()
        && let Some(ref c) = *cache
        && c.fetched_at.elapsed().as_secs() < BT_CACHE_TTL_SECS
    {
        return c.devices.clone();
    }
    let devices = fetch_bt_devices();
    if let Ok(mut cache) = BT_CACHE.lock() {
        *cache = Some(BtCache {
            devices: devices.clone(),
            fetched_at: Instant::now(),
        });
    }
    devices
}

/// The uncached read: two subprocesses, never N+1.
fn fetch_bt_devices() -> Vec<BtDevice> {
    let output = match run_cmd_output("bluetoothctl", &["devices", "Paired"]) {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    // One bulk query instead of `info <mac>` per device. Failure here is not
    // fatal — an empty set just means nothing reports as connected, which is
    // the same thing the per-device call returned when it failed.
    let connected: std::collections::HashSet<String> =
        run_cmd_output("bluetoothctl", &["devices", "Connected"])
            .map(|o| parse_bt_macs(&o))
            .unwrap_or_default();
    // Each line: "Device AA:BB:CC:DD:EE:FF Device Name"
    output
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("Device ")?;
            let (mac, name) = rest.split_once(' ')?;
            Some(BtDevice {
                connected: connected.contains(mac),
                mac: mac.to_string(),
                name: name.trim().to_string(),
            })
        })
        .collect()
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

/// The two verbs, and the two words that name the radio. Every accepted phrase
/// is a *product* of these — `<verb> <noun>` or `<noun> <verb>` — rather than a
/// list of literal strings.
///
/// This used to be two hand-written phrase tables (one for parsing, one for
/// completions) and they had already drifted: parsing accepted `bt connect`
/// while completions did not, so that phrasing executed but offered no device
/// rows. One table, both directions.
const BT_VERBS: [&str; 2] = ["connect", "disconnect"];
const BT_NOUNS: [&str; 2] = ["bluetooth", "bt"];

/// If `lower` opens with a bluetooth verb phrase, return the canonical verb and
/// the rest of the input. `require_space` distinguishes "this is the whole
/// command, args follow" (execute) from "the user is still typing" (completions).
fn bt_phrase(lower: &str, require_space: bool) -> Option<(&'static str, &str)> {
    for verb in BT_VERBS {
        for noun in BT_NOUNS {
            for phrase in [format!("{verb} {noun}"), format!("{noun} {verb}")] {
                let rest = if require_space {
                    lower.strip_prefix(&format!("{phrase} "))
                } else {
                    lower.strip_prefix(&phrase)
                };
                if let Some(rest) = rest {
                    return Some((
                        if verb == "connect" {
                            "connect"
                        } else {
                            "disconnect"
                        },
                        rest,
                    ));
                }
            }
        }
    }
    None
}

/// Handle `connect bluetooth <device>` / `disconnect bluetooth <device>`.
/// Returns `Some(Ok/Err)` if matched, `None` if not a bluetooth connect/disconnect command.
fn try_bluetooth_connect(input: &str) -> Option<Result<String, String>> {
    let lower = input.to_lowercase();

    let (action, rest) = bt_phrase(&lower, true)?;
    let query = rest.trim();

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
    // Same PipeWire → PulseAudio → ALSA chain as mute; each tool takes a
    // different value syntax (wpctl relative "5%+", pactl "+5%", amixer "5%+").
    if lower == "volume up" {
        return Some(
            run_first_available(&[
                ("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%+"]),
                ("pactl", &["set-sink-volume", "@DEFAULT_SINK@", "+5%"]),
                ("amixer", &["set", "Master", "5%+"]),
            ])
            .map(|()| read_volume()),
        );
    }
    if lower == "volume down" {
        return Some(
            run_first_available(&[
                ("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%-"]),
                ("pactl", &["set-sink-volume", "@DEFAULT_SINK@", "-5%"]),
                ("amixer", &["set", "Master", "5%-"]),
            ])
            .map(|()| read_volume()),
        );
    }
    if let Some(rest) = lower.strip_prefix("volume ") {
        let rest = rest.trim();
        if let Some(n) = parse_percent(rest) {
            let frac = format!("{:.2}", n as f64 / 100.0);
            let pct = format!("{n}%");
            return Some(
                run_first_available(&[
                    ("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &frac]),
                    ("pactl", &["set-sink-volume", "@DEFAULT_SINK@", &pct]),
                    ("amixer", &["set", "Master", &pct]),
                ])
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

/// Read current volume, cosmetic read-back after a change.
///
/// Tries PipeWire (`wpctl`) then PulseAudio (`pactl`); if neither is present or
/// parsing fails, degrades to a generic "Volume changed" (the change itself
/// already succeeded — this is only the confirmation text).
fn read_volume() -> String {
    // PipeWire: "Volume: 0.50" or "Volume: 0.50 [MUTED]"
    if let Ok(out) = run_cmd_output("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]) {
        let trimmed = out.trim();
        if let Some(rest) = trimmed.strip_prefix("Volume: ")
            && let Some(val) = rest.split_whitespace().next()
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

    // PulseAudio: `pactl get-sink-volume @DEFAULT_SINK@` → a line containing
    // e.g. "front-left: 32768 /  50% / ...". Take the first percentage.
    if let Ok(out) = run_cmd_output("pactl", &["get-sink-volume", "@DEFAULT_SINK@"])
        && let Some(pct) = out
            .split('%')
            .next()
            .and_then(|s| s.rsplit(|c: char| !c.is_ascii_digit()).next())
            .filter(|s| !s.is_empty())
    {
        return format!("Volume: {pct}%");
    }

    "Volume changed".to_string()
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

/// One candidate in a fallback chain: a program plus its args.
type Candidate<'a> = (&'a str, &'a [&'a str]);

/// Run the first *installed* program in a fallback chain.
///
/// Linux is not one platform: audio might be PipeWire (`wpctl`), PulseAudio
/// (`pactl`), or bare ALSA (`amixer`); wifi might be NetworkManager (`nmcli`) or
/// kernel rfkill; power might be systemd (`systemctl`) or sysvinit (`poweroff`).
/// Rather than assume one tool and fail cryptically elsewhere, we try each in
/// order and use the first one that exists on this system.
///
/// Semantics, chosen deliberately:
/// - A program that is **not installed** (`ErrorKind::NotFound`) is skipped —
///   we fall through to the next candidate.
/// - A program that **runs but fails** is a *real* error (e.g. polkit denied,
///   bad device): we return it immediately rather than masking it by trying a
///   different tool that would also fail or, worse, do the wrong thing.
/// - If **no** candidate is installed, we return a single actionable message
///   naming what to install, instead of a raw "No such file or directory".
fn run_first_available(chain: &[Candidate]) -> Result<(), String> {
    for (program, args) in chain {
        match Command::new(program).args(*args).output() {
            Ok(output) => {
                return if output.status.success() {
                    Ok(())
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Err(format!("{program} failed: {}", stderr.trim()))
                };
            }
            // Not installed — try the next tool in the chain.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            // Installed but couldn't be launched (permissions, etc.) — real error.
            Err(e) => return Err(format!("Failed to run {program}: {e}")),
        }
    }
    let tools: Vec<&str> = chain.iter().map(|(p, _)| *p).collect();
    Err(format!(
        "None of the required tools are installed (tried: {}). Install one of them.",
        tools.join(", ")
    ))
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

/// Log out of the current session.
///
/// Prefer the DE-native logout (which runs the session's save-state hooks)
/// based on `XDG_CURRENT_DESKTOP`, then fall back to logind's blunter
/// `terminate-session`/`terminate-user` (works on any systemd/elogind system).
fn logout_session() -> Result<(), String> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_lowercase();

    // DE-native logout first — it saves session state and is the "right" logout.
    let de_native: Option<Candidate> = if desktop.contains("kde") || desktop.contains("plasma") {
        // qdbus argument list; 1,-1,-1 = logout, no confirm, default shutdown mode.
        Some((
            "qdbus",
            &["org.kde.Shutdown", "/Shutdown", "org.kde.Shutdown.logout"],
        ))
    } else if desktop.contains("gnome") {
        Some(("gnome-session-quit", &["--logout", "--no-prompt"]))
    } else if desktop.contains("xfce") {
        Some(("xfce4-session-logout", &["--logout"]))
    } else if desktop.contains("sway") {
        Some(("swaymsg", &["exit"]))
    } else if desktop.contains("hyprland") {
        Some(("hyprctl", &["dispatch", "exit"]))
    } else {
        None
    };

    if let Some(candidate) = de_native
        && run_first_available(&[candidate]).is_ok()
    {
        return Ok(());
    }

    // Fall back to logind (systemd/elogind). terminate-user, then session-self.
    run_first_available(&[
        ("loginctl", &["terminate-user", &whoami()]),
        ("loginctl", &["terminate-session", "self"]),
    ])
}

/// The base action VERBS the agent chooses between — the machine-readable enum
/// fed to the tool schema so a constrained model (cloud `enum` / local grammar)
/// can only emit a valid one. The operand ("50" for `volume`, "30m" for
/// `shutdown in`, a device name for `connect bluetooth`) rides the separate
/// free-text `value`. Kept next to the parser it feeds so the two can't drift.
const SYSTEM_ACTION_VERBS: &[&str] = &[
    "shutdown",
    "cancel shutdown",
    "reboot",
    "suspend",
    "hibernate",
    "lock",
    "logout",
    "mute",
    "unmute",
    "volume",
    "brightness",
    "wifi",
    "bluetooth",
    "connect bluetooth",
    "disconnect bluetooth",
];

/// The JSON Schema for `system`'s args: a required `action` (constrained to
/// [`SYSTEM_ACTION_VERBS`]) plus an optional free `value` operand. Emitted as the
/// tool's `input_schema` so the model is constrained to a real verb.
fn system_input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": SYSTEM_ACTION_VERBS,
                        "description": "The system action to perform." },
            "value": { "type": "string",
                       "description": "Operand when the action needs one: a percentage for volume/brightness (e.g. \"50\"), \"up\"/\"down\", \"on\"/\"off\" for wifi/bluetooth, a duration for shutdown (e.g. \"30m\"), or a device name for bluetooth connect/disconnect. Omit for actions that take none (lock, mute, reboot…)." }
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

/// Normalize the tool's `args` to the flat `"<action> <value>"` string the
/// parser already understands. A constrained model sends the structured JSON
/// (`{"action":"volume","value":"50"}`); a human or a legacy/flat caller sends
/// the string directly. Accepting both keeps the schema win without rewriting
/// the three matchers below — and keeps `execute`/`assess_risk` on `&str`.
fn system_args_to_flat(args: &str) -> String {
    let t = args.trim();
    if !t.starts_with('{') {
        return t.to_string();
    }
    match serde_json::from_str::<serde_json::Value>(t) {
        Ok(v) => {
            let action = v
                .get("action")
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .trim();
            let value = v.get("value").and_then(|a| a.as_str()).unwrap_or("").trim();
            if value.is_empty() {
                action.to_string()
            } else {
                format!("{action} {value}")
            }
        }
        // Not the JSON we expected — fall back to the raw string; the parser
        // will reject it with the usual "unknown action" message.
        Err(_) => t.to_string(),
    }
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

    fn mutates_state(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "System controls (power, audio, brightness, wifi, bluetooth)"
    }
    fn usage(&self) -> &str {
        "shutdown, reboot, suspend, hibernate, lock, logout, mute, unmute, volume <up|down|0-100>, brightness <up|down|0-100>, wifi <on|off>, bluetooth <on|off>, connect bluetooth <device>, disconnect bluetooth <device>, shutdown in <duration> (e.g. 'shutdown in 30m'), cancel shutdown"
    }
    fn input_schema(&self) -> Option<serde_json::Value> {
        Some(system_input_schema())
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::System
    }

    fn assess_risk(
        &self,
        args: &str,
        _ctx: &crate::action_registry::RiskContext<'_>,
    ) -> RiskAssessment {
        // Only destructive actions (shutdown, reboot, hibernate, logout) need
        // confirmation. Reversible toggles (mute, volume, brightness, wifi,
        // bluetooth) auto-execute. This ownership lives here, not in the Rules
        // Engine.
        let flat = system_args_to_flat(args);
        let action = flat.to_lowercase();
        if crate::rules::verbs::is_destructive_system_action(&action) {
            RiskAssessment::confirm(format!(
                "System action '{}' requires confirmation",
                args.trim()
            ))
        } else {
            RiskAssessment::level(RiskLevel::Low)
        }
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // A constrained model sends `{"action":..,"value":..}`; flatten it (and a
        // plain-string caller passes through) to the form the matchers parse.
        let flat = system_args_to_flat(args);
        let input = flat.trim();
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

        // Show paired devices for bluetooth connect/disconnect queries. Same
        // decider as `try_bluetooth_connect`, so anything that EXECUTES also
        // completes — no space required here, since the user is mid-phrase.
        // Emit the canonical `<verb> bluetooth` phrasing whatever the user
        // typed, so the row's `run` is a string `try_bluetooth_connect` accepts.
        let bt_prefix = bt_phrase(&lower, false).map(|(verb, _)| format!("{verb} bluetooth"));

        if let Some(action) = bt_prefix.as_deref() {
            let devices = list_bt_devices();
            for dev in &devices {
                // Read from the bulk query, not a subprocess per device.
                let status = if dev.connected { "connected" } else { "paired" };
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

    /// The parser both bluetooth queries share. If it drifted, `connected`
    /// would silently never match and every device would render as "paired".
    #[test]
    fn bt_macs_are_parsed_from_bluetoothctl_output() {
        let out = "Device AA:BB:CC:DD:EE:FF Sony WH-1000XM4\n\
                   Device 11:22:33:44:55:66 Magic Keyboard\n";
        let macs = parse_bt_macs(out);
        assert_eq!(macs.len(), 2);
        assert!(macs.contains("AA:BB:CC:DD:EE:FF"));
        assert!(macs.contains("11:22:33:44:55:66"));
    }

    /// Real output carries a header line and blank lines; neither is a device.
    #[test]
    fn bt_parser_ignores_non_device_lines() {
        let out = "Agent registered\n\
                   \n\
                   Device AA:BB:CC:DD:EE:FF Headphones\n\
                   [bluetooth]# \n";
        let macs = parse_bt_macs(out);
        assert_eq!(macs.len(), 1, "got {macs:?}");
        assert!(macs.contains("AA:BB:CC:DD:EE:FF"));
    }

    /// No adapter, no bluetoothctl, no devices — all the same shape, and none
    /// of them may panic on a path that runs per keystroke.
    #[test]
    fn bt_parser_handles_empty_and_malformed_output() {
        assert!(parse_bt_macs("").is_empty());
        assert!(parse_bt_macs("No default controller available\n").is_empty());
        assert!(parse_bt_macs("Device\n").is_empty(), "no mac/name split");
        assert!(
            parse_bt_macs("Device AA:BB\n").is_empty(),
            "no space to split"
        );
    }

    #[test]
    fn assess_risk_confirms_destructive_auto_executes_reversible() {
        let h = SystemCommand::new();
        // Destructive → confirm (Medium + custom message).
        for a in ["shutdown", "reboot", "hibernate", "logout"] {
            assert_eq!(
                h.assess_risk(a, &Default::default()).level,
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
                h.assess_risk(a, &Default::default()).level,
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
    fn run_first_available_skips_missing_and_reports_none_installed() {
        // A chain of only-nonexistent programs → the "none installed" message,
        // NOT a raw "No such file or directory".
        let missing: &[&str] = &[];
        let err = run_first_available(&[
            ("lychi_nonexistent_tool_a", missing),
            ("lychi_nonexistent_tool_b", missing),
        ])
        .unwrap_err();
        assert!(err.contains("None of the required tools"), "got: {err}");
        assert!(err.contains("lychi_nonexistent_tool_a"), "got: {err}");
    }

    #[test]
    fn run_first_available_uses_first_installed() {
        // `true` exists on every Linux and exits 0; a missing tool before it is
        // skipped, and we stop at `true` (success) without reaching a later one.
        let empty: &[&str] = &[];
        let ok = run_first_available(&[
            ("lychi_nonexistent_tool", empty),
            ("true", empty),
            ("lychi_should_not_be_reached", empty),
        ]);
        assert!(ok.is_ok(), "expected `true` to satisfy the chain: {ok:?}");
    }

    #[test]
    fn run_first_available_surfaces_real_failure_without_falling_through() {
        // `false` is installed and exits non-zero. That's a REAL failure — we
        // must return it, not silently try the next candidate (which could do
        // the wrong thing). So the later `true` is never reached.
        let empty: &[&str] = &[];
        let err = run_first_available(&[("false", empty), ("true", empty)]).unwrap_err();
        assert!(err.contains("false failed"), "got: {err}");
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
    fn system_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; it flattens to the string
        // the matchers already parse.
        assert_eq!(
            system_args_to_flat(r#"{"action":"volume","value":"50"}"#),
            "volume 50"
        );
        // No operand → just the verb.
        assert_eq!(system_args_to_flat(r#"{"action":"mute"}"#), "mute");
        // Empty value is treated as no operand.
        assert_eq!(
            system_args_to_flat(r#"{"action":"lock","value":""}"#),
            "lock"
        );
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(system_args_to_flat("volume 50"), "volume 50");
        assert_eq!(system_args_to_flat("shutdown"), "shutdown");
        // A multi-word operand survives (bluetooth device name).
        assert_eq!(
            system_args_to_flat(r#"{"action":"connect bluetooth","value":"My Headphones"}"#),
            "connect bluetooth My Headphones"
        );
    }

    #[test]
    fn system_schema_enum_matches_the_real_verbs() {
        // The schema's action enum must be exactly SYSTEM_ACTION_VERBS, so the
        // model is constrained to verbs the parser actually handles.
        let schema = system_input_schema();
        let en = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert_eq!(en.len(), SYSTEM_ACTION_VERBS.len());
        for v in SYSTEM_ACTION_VERBS {
            assert!(en.iter().any(|e| e == v), "enum missing {v}");
        }
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
        // G3: every phrasing is a product of BT_VERBS x BT_NOUNS, so the
        // parse side and the completion side cannot drift apart. The old code
        // had two hand-written tables and they HAD drifted: `bt connect`
        // executed but offered no device rows.
        for verb in super::BT_VERBS {
            for noun in super::BT_NOUNS {
                for phrase in [format!("{verb} {noun}"), format!("{noun} {verb}")] {
                    assert!(
                        try_bluetooth_connect(&format!("{phrase} speaker")).is_some(),
                        "execute rejected {phrase:?}"
                    );
                    assert!(
                        super::bt_phrase(&phrase, false).is_some(),
                        "completions rejected {phrase:?}"
                    );
                }
            }
        }

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
                connected: false,
            },
            BtDevice {
                mac: "11:22:33:44:55:66".into(),
                name: "AirPods Pro".into(),
                connected: true,
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
