use std::fs;
use std::process::Command;
use std::time::Instant;

use async_trait::async_trait;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;

pub struct SysInfoHandler;

impl SysInfoHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SysInfoHandler {
    fn default() -> Self {
        Self::new()
    }
}

const SUBCOMMANDS: &[&str] = &[
    "ip",
    "cpu",
    "mem",
    "disk",
    "temp",
    "gpu",
    "battery",
    "net",
    "audio",
    "display",
    "os",
    "speedtest",
];

/// `sysinfo`'s grammar: a single free-form read with an optional `topic`
/// Choice — reusing [`SUBCOMMANDS`] directly, the SAME const the dispatch
/// match, triggers, and completions are built from, so the schema enum cannot
/// drift from dispatch. Free-form (not one verb per topic) so "omit the topic"
/// stays expressible: the empty flat form is the combined overview, which a
/// verb-per-topic grammar could not render. Everything here is a read —
/// nothing mutates.
const SYSINFO_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Read local system information (hardware, OS, network, sensors). \
               Read-only and instant, except: `net` also looks up the public IP \
               via ifconfig.me (the user consents first), and `speedtest` \
               transfers ~11 MB to Cloudflare (consented every run, takes \
               ~15-45s).",
        mutates: false,
        operands: &[Operand {
            name: "topic",
            desc: "Which reading to take: ip (local addresses only), cpu, mem, \
                   disk, temp, gpu, battery, net (interfaces + WiFi + public \
                   IP), audio, display, os, or speedtest. Omit for a combined \
                   overview (ip, memory, disk, uptime, temps, battery, WiFi, \
                   volume).",
            required: false,
            kind: ArgKind::Choice(SUBCOMMANDS),
            prefix: None,
        }],
    }],
};

/// Normalize the tool's `args` to the flat subcommand string `execute` parses,
/// via the ONE structured→flat decider ([`Grammar::flatten_json`]). A human or
/// legacy/flat caller passes through unchanged; a structured call with no
/// `topic` flattens to `""`, the combined overview. Keeps
/// `execute`/`assess_risk` on `&str` — and `assess_risk` MUST keep calling
/// this first, so the consent gate sees the same verb execute dispatches on.
fn sysinfo_args_to_flat(args: &str) -> String {
    SYSINFO_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
}

#[async_trait]
impl ActionHandler for SysInfoHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::{ArgTransform, Trigger};
        static TRIGGERS: &[Trigger] = &[
            Trigger::keywords(&["sysinfo"]),
            // Bare-word shortcuts ignore any trailing args and use the keyword.
            Trigger::new(
                &[
                    "ip",
                    "cpu",
                    "mem",
                    "disk",
                    "temp",
                    "gpu",
                    "battery",
                    "net",
                    "audio",
                    "display",
                    "os",
                    "speedtest",
                ],
                ArgTransform::KeywordOnly,
            ),
        ];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "sysinfo"
    }

    fn description(&self) -> &str {
        "System info — ip, cpu, mem, disk, temp, gpu, battery, net, audio, display, os"
    }
    fn usage(&self) -> &str {
        "Subcommands: ip, cpu, mem, disk, temp, gpu, battery, net, audio, display, os. Empty args shows a full overview"
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(SYSINFO_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::System
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::System
    }

    fn assess_risk(
        &self,
        args: &str,
        _ctx: &crate::action_registry::RiskContext<'_>,
    ) -> crate::action_registry::RiskAssessment {
        use crate::action_registry::{ConsentKind, RiskAssessment, RiskLevel};
        // A structured caller sends `{"topic":..}`; flatten it (a plain-string
        // caller passes through) so the consent match sees the same verb execute
        // will dispatch on.
        let args = sysinfo_args_to_flat(args);
        let args = args.as_str();
        // The consent declaration parses args EXACTLY as `execute` does (trim +
        // lowercase, same alias arms). This match and the dispatch match must
        // agree — the Rules Engine keeping its own alias list is how `sysinfo
        // speed` ran the speedtest unconsented while `sysinfo ip` prompted
        // about a public-IP lookup it never performs. `alias_consent_matches_
        // dispatch` pins the pairing.
        match args.trim().to_lowercase().as_str() {
            // read_network fetches the public IP via ifconfig.me. The grant
            // persists via the typed consent_feature on the result DTO; the
            // domain stays in the prompt for the user's sake, not the code's.
            "net" | "network" => RiskAssessment::level(RiskLevel::Low).with_consent(
                ConsentKind::PublicIp,
                "This will look up your public IP via ifconfig.me. Allow and remember?",
            ),
            "speedtest" | "speed" => RiskAssessment::level(RiskLevel::Low).with_consent(
                ConsentKind::LargeTransfer,
                "Speed test will download 10 MB and upload 1 MB to Cloudflare",
            ),
            // "ip" prints LOCAL addresses only — no consent. The old gate
            // prompted for it anyway, a false prompt that trains click-through.
            _ => RiskAssessment::level(self.default_risk()),
        }
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let start = Instant::now();
        // A structured caller sends `{"topic":..}`; flatten it (and a
        // plain-string caller passes through) to the subcommand the match parses.
        let flat = sysinfo_args_to_flat(args);
        let cmd = flat.trim().to_lowercase();

        let output = match cmd.as_str() {
            "" => {
                // No subcommand — show a brief overview
                let ip = read_local_ips();
                let mem = read_mem_info();
                let disk = read_disk_info();
                let uptime = read_uptime();
                let temps = read_temps_summary();
                let battery = read_battery_summary();
                let net = read_net_summary();
                let audio = read_audio_summary();
                let mut out = format!(
                    "IP: {}\n{}\n---\n{}\n---\n{}",
                    ip.trim(),
                    mem.trim(),
                    disk.trim(),
                    uptime.trim()
                );
                if !temps.is_empty() {
                    out.push_str(&format!("\n---\n{temps}"));
                }
                if !battery.is_empty() {
                    out.push_str(&format!("\n---\n{battery}"));
                }
                if !net.is_empty() {
                    out.push_str(&format!("\n---\n{net}"));
                }
                if !audio.is_empty() {
                    out.push_str(&format!("\n---\n{audio}"));
                }
                Ok(out)
            }
            "ip" => {
                let local = read_local_ips();
                Ok(format!("Local: {}", local.trim()))
            }
            "cpu" => {
                let model = run_cmd(
                    "sh",
                    &["-c", "grep -m1 'model name' /proc/cpuinfo | cut -d: -f2"],
                )
                .unwrap_or_default();
                let cores = run_cmd("nproc", &[]).unwrap_or_default();
                let load = run_cmd("sh", &["-c", "cat /proc/loadavg"]).unwrap_or_default();
                Ok(format!(
                    "CPU:{}\nCores: {}\nLoad: {}",
                    model.trim(),
                    cores.trim(),
                    load.trim()
                ))
            }
            "mem" => Ok(read_mem_info()),
            "disk" => Ok(read_disk_info()),
            "temp" => Ok(read_temps()),
            "gpu" => Ok(read_gpu()),
            "battery" | "bat" => Ok(read_battery()),
            // The two network arms shell out to curl (3s and up-to-45s worst
            // case) — that work goes to the blocking pool. The runtime has four
            // workers; a speedtest used to occupy one for its whole duration.
            "net" | "network" => Ok(tokio::task::spawn_blocking(read_network)
                .await
                .unwrap_or_else(|e| format!("network probe failed: {e}"))),
            "audio" | "sound" | "volume" => Ok(read_audio()),
            "display" | "monitor" | "screen" => Ok(read_display()),
            "os" | "system" | "uptime" => Ok(read_os()),
            "speedtest" | "speed" => Ok(tokio::task::spawn_blocking(read_speedtest)
                .await
                .unwrap_or_else(|e| format!("speedtest failed: {e}"))),
            _ => Err(format!(
                "Unknown: '{cmd}'. Try: ip, cpu, mem, disk, temp, gpu, battery, net, audio, display, os, speedtest"
            )),
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        match output {
            Ok(text) => Ok(ActionResult::ok(text, OutputType::Terminal).with_duration(duration_ms)),
            Err(e) => Ok(ActionResult::err(e).with_duration(duration_ms)),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let lower = partial.to_lowercase();
        SUBCOMMANDS
            .iter()
            .filter(|s| s.contains(&lower) || lower.is_empty())
            .map(|s| CompletionItem {
                label: s.to_string(),
                icon_path: None,
                score: if s.starts_with(&lower) { 100 } else { 50 },
                description: None,
                reason: None,
                thumb_b64: None,
                run: Some(format!("sysinfo {s}")),
                ..Default::default()
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Sensor helpers — read from /sys/class/hwmon (no external deps needed)
// ---------------------------------------------------------------------------

/// Read a millidegree file and format as °C.
fn read_temp_file(path: &str) -> Option<f64> {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|m| m / 1000.0)
}

/// Discover hwmon sensors and return (name, temp_°C) pairs.
fn read_hwmon_temps() -> Vec<(String, f64)> {
    let mut results = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/hwmon") else {
        return results;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let name = fs::read_to_string(dir.join("name"))
            .unwrap_or_default()
            .trim()
            .to_string();
        // Read temp1_input (primary sensor for each hwmon device)
        if let Some(temp) = read_temp_file(&dir.join("temp1_input").to_string_lossy()) {
            results.push((name, temp));
        }
    }
    // Sort by name for stable output
    results.sort_by(|a, b| a.0.cmp(&b.0));
    results
}

/// Full temp output for `sysinfo temp`.
fn read_temps() -> String {
    let temps = read_hwmon_temps();
    if temps.is_empty() {
        return "No temperature sensors found".to_string();
    }
    let mut lines = Vec::new();
    for (name, temp) in &temps {
        let label = match name.as_str() {
            "k10temp" | "coretemp" => "CPU",
            "amdgpu" => "AMD GPU",
            "nvme" | "nvme0" => "NVMe",
            "iwlwifi_1" | "iwlwifi" => "WiFi",
            "acpitz" => "ACPI",
            _ => name,
        };
        lines.push(format!("{label}: {temp:.0}°C"));
    }
    // Also try nvidia-smi for discrete NVIDIA GPU
    if let Ok(nv) = run_cmd(
        "nvidia-smi",
        &[
            "--query-gpu=temperature.gpu",
            "--format=csv,noheader,nounits",
        ],
    ) {
        let nv = nv.trim();
        if !nv.is_empty() {
            lines.push(format!("NVIDIA GPU: {nv}°C"));
        }
    }
    lines.join("\n")
}

/// One-line temp summary for the overview.
fn read_temps_summary() -> String {
    let temps = read_hwmon_temps();
    let mut parts = Vec::new();
    for (name, temp) in &temps {
        let label = match name.as_str() {
            "k10temp" | "coretemp" => "CPU",
            "amdgpu" => "GPU",
            "nvme" | "nvme0" => "NVMe",
            _ => continue,
        };
        parts.push(format!("{label}: {temp:.0}°C"));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("Temps: {}", parts.join(", "))
}

/// Full GPU output for `sysinfo gpu`.
fn read_gpu() -> String {
    let mut sections = Vec::new();

    // AMD GPU via hwmon
    if let Some(amd) = read_amd_gpu() {
        sections.push(amd);
    }

    // NVIDIA GPU via nvidia-smi
    if let Ok(nv) = run_cmd(
        "nvidia-smi",
        &[
            "--query-gpu=name,temperature.gpu,utilization.gpu,memory.used,memory.total,power.draw",
            "--format=csv,noheader",
        ],
    ) {
        let nv = nv.trim();
        if !nv.is_empty() {
            // CSV: name, temp, util%, mem_used, mem_total, power
            let parts: Vec<&str> = nv.split(", ").collect();
            if parts.len() >= 6 {
                sections.push(format!(
                    "NVIDIA: {}\nTemp: {}°C\nUsage: {}%\nVRAM: {} / {}\nPower: {} W",
                    parts[0].trim(),
                    parts[1].trim(),
                    parts[2].trim().trim_end_matches(" %"),
                    parts[3].trim(),
                    parts[4].trim(),
                    parts[5].trim().trim_end_matches(" W"),
                ));
            } else {
                sections.push(format!("NVIDIA: {nv}"));
            }
        }
    }

    // Fallback: lspci for GPU names
    if sections.is_empty()
        && let Ok(lspci) = run_cmd("sh", &["-c", "lspci | grep -iE 'VGA|3D|Display'"])
    {
        let lspci = lspci.trim();
        if !lspci.is_empty() {
            sections.push(lspci.to_string());
        }
    }

    if sections.is_empty() {
        "No GPU info available".to_string()
    } else {
        sections.join("\n---\n")
    }
}

/// Read AMD GPU info from hwmon (amdgpu driver).
fn read_amd_gpu() -> Option<String> {
    let Ok(entries) = fs::read_dir("/sys/class/hwmon") else {
        return None;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let name = fs::read_to_string(dir.join("name"))
            .unwrap_or_default()
            .trim()
            .to_string();
        if name != "amdgpu" {
            continue;
        }
        let mut lines = vec!["AMD GPU".to_string()];
        if let Some(temp) = read_temp_file(&dir.join("temp1_input").to_string_lossy()) {
            lines.push(format!("Temp: {temp:.0}°C"));
        }
        // freq1_input is in Hz
        if let Ok(freq) = fs::read_to_string(dir.join("freq1_input"))
            && let Ok(hz) = freq.trim().parse::<u64>()
        {
            lines.push(format!("Clock: {} MHz", hz / 1_000_000));
        }
        // power1_average is in microwatts
        if let Ok(power) = fs::read_to_string(dir.join("power1_average"))
            && let Ok(uw) = power.trim().parse::<f64>()
        {
            lines.push(format!("Power: {:.1} W", uw / 1_000_000.0));
        }
        // GPU utilization via sysfs (amdgpu specific)
        if let Ok(busy) = fs::read_to_string("/sys/class/drm/card1/device/gpu_busy_percent")
            .or_else(|_| fs::read_to_string("/sys/class/drm/card0/device/gpu_busy_percent"))
        {
            lines.push(format!("Usage: {}%", busy.trim()));
        }
        // VRAM
        if let Ok(used) = fs::read_to_string("/sys/class/drm/card1/device/mem_info_vram_used")
            .or_else(|_| fs::read_to_string("/sys/class/drm/card0/device/mem_info_vram_used"))
            && let Ok(total) = fs::read_to_string("/sys/class/drm/card1/device/mem_info_vram_total")
                .or_else(|_| fs::read_to_string("/sys/class/drm/card0/device/mem_info_vram_total"))
            && let (Ok(u), Ok(t)) = (used.trim().parse::<u64>(), total.trim().parse::<u64>())
        {
            lines.push(format!("VRAM: {} / {} MB", u / 1_048_576, t / 1_048_576));
        }
        return Some(lines.join("\n"));
    }
    None
}

/// Full battery output for `sysinfo battery`.
fn read_battery() -> String {
    let mut results = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/power_supply") else {
        return "No battery found".to_string();
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let supply_type = fs::read_to_string(dir.join("type"))
            .unwrap_or_default()
            .trim()
            .to_string();
        if supply_type != "Battery" {
            continue;
        }
        let name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let status = fs::read_to_string(dir.join("status"))
            .unwrap_or_else(|_| "Unknown".into())
            .trim()
            .to_string();
        let capacity = fs::read_to_string(dir.join("capacity"))
            .unwrap_or_else(|_| "?".into())
            .trim()
            .to_string();
        let mut lines = vec![format!("{name}: {capacity}% ({status})")];
        // Energy/charge info if available
        if let (Ok(now), Ok(full)) = (
            fs::read_to_string(dir.join("energy_now")),
            fs::read_to_string(dir.join("energy_full")),
        ) && let (Ok(n), Ok(f)) = (now.trim().parse::<f64>(), full.trim().parse::<f64>())
        {
            lines.push(format!(
                "Energy: {:.1} / {:.1} Wh",
                n / 1_000_000.0,
                f / 1_000_000.0
            ));
        }
        // Power draw
        if let Ok(rate) = fs::read_to_string(dir.join("power_now"))
            && let Ok(w) = rate.trim().parse::<f64>()
        {
            lines.push(format!("Power: {:.1} W", w / 1_000_000.0));
        }
        // Cycle count
        if let Ok(cycles) = fs::read_to_string(dir.join("cycle_count")) {
            let c = cycles.trim();
            if c != "0" && !c.is_empty() {
                lines.push(format!("Cycles: {c}"));
            }
        }
        results.push(lines.join("\n"));
    }
    if results.is_empty() {
        "No battery found".to_string()
    } else {
        results.join("\n---\n")
    }
}

/// One-line battery summary for overview.
fn read_battery_summary() -> String {
    let Ok(entries) = fs::read_dir("/sys/class/power_supply") else {
        return String::new();
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let supply_type = fs::read_to_string(dir.join("type"))
            .unwrap_or_default()
            .trim()
            .to_string();
        if supply_type != "Battery" {
            continue;
        }
        let status = fs::read_to_string(dir.join("status"))
            .unwrap_or_else(|_| "Unknown".into())
            .trim()
            .to_string();
        let capacity = fs::read_to_string(dir.join("capacity"))
            .unwrap_or_else(|_| "?".into())
            .trim()
            .to_string();
        return format!("Battery: {capacity}% ({status})");
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

/// Full network info for `sysinfo net`.
fn read_network() -> String {
    let mut lines = Vec::new();

    // WiFi SSID + signal
    if let Ok(wifi) = run_cmd("nmcli", &["-t", "-f", "active,ssid,signal", "dev", "wifi"]) {
        for line in wifi.lines() {
            if line.starts_with("yes:") {
                let parts: Vec<&str> = line.splitn(3, ':').collect();
                if parts.len() >= 3 {
                    lines.push(format!("WiFi: {} ({}% signal)", parts[1], parts[2]));
                }
            }
        }
    }

    // Active interfaces with IPs
    if let Ok(ip_out) = run_cmd("ip", &["-o", "-4", "addr", "show", "up", "scope", "global"]) {
        for line in ip_out.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            // Format: index iface inet addr/mask ...
            if parts.len() >= 4 {
                let iface = parts[1];
                let addr = parts[3].split('/').next().unwrap_or(parts[3]);
                lines.push(format!("{iface}: {addr}"));
            }
        }
    }

    // C6: Public IP lookup is gated by the Rules Engine — the user must consent
    // to public IP lookup (privacy.allow_public_ip) before reaching here.
    if let Ok(pub_ip) = run_cmd("sh", &["-c", "curl -s --max-time 3 https://ifconfig.me"]) {
        let pub_ip = pub_ip.trim();
        if !pub_ip.is_empty() {
            lines.push(format!("Public: {pub_ip}"));
        }
    }

    if lines.is_empty() {
        "No network info available".to_string()
    } else {
        lines.join("\n")
    }
}

/// One-line network summary for overview.
fn read_net_summary() -> String {
    if let Ok(wifi) = run_cmd("nmcli", &["-t", "-f", "active,ssid,signal", "dev", "wifi"]) {
        for line in wifi.lines() {
            if line.starts_with("yes:") {
                let parts: Vec<&str> = line.splitn(3, ':').collect();
                if parts.len() >= 3 {
                    return format!("WiFi: {} ({}%)", parts[1], parts[2]);
                }
            }
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

/// Full audio info for `sysinfo audio`.
fn read_audio() -> String {
    let mut lines = Vec::new();

    // Try PipeWire/WirePlumber first (most modern setups)
    let has_wpctl = run_cmd("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]).ok();

    if let Some(ref vol) = has_wpctl {
        let vol = vol.trim();
        if !vol.is_empty() {
            // "Volume: 0.90" or "Volume: 0.90 [MUTED]"
            if let Some(v) = vol.strip_prefix("Volume: ") {
                let (num, muted) = if let Some(n) = v.strip_suffix(" [MUTED]") {
                    (n, " [MUTED]")
                } else {
                    (v, "")
                };
                if let Ok(f) = num.parse::<f64>() {
                    lines.push(format!("Volume: {:.0}%{muted}", f * 100.0));
                }
            }
        }
    }

    // Sink name via pactl
    if let Ok(sink) = run_cmd("pactl", &["get-default-sink"]) {
        let sink = sink.trim();
        if !sink.is_empty() {
            // Try to get a friendly description
            if let Ok(info) = run_cmd("pactl", &["list", "sinks", "short"]) {
                // Find matching sink line for a cleaner name
                for line in info.lines() {
                    if line.contains(sink) {
                        lines.push(format!("Output: {sink}"));
                        break;
                    }
                }
                if lines.len() <= 1 {
                    lines.push(format!("Output: {sink}"));
                }
            } else {
                lines.push(format!("Output: {sink}"));
            }
        }
    }

    // Input device
    if let Ok(source) = run_cmd("pactl", &["get-default-source"]) {
        let source = source.trim();
        if !source.is_empty() {
            lines.push(format!("Input: {source}"));
        }
    }

    if lines.is_empty() {
        "No audio info available".to_string()
    } else {
        lines.join("\n")
    }
}

/// One-line audio summary for overview.
fn read_audio_summary() -> String {
    if let Ok(vol) = run_cmd("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]) {
        let vol = vol.trim();
        if let Some(v) = vol.strip_prefix("Volume: ") {
            let (num, muted) = if let Some(n) = v.strip_suffix(" [MUTED]") {
                (n, " MUTED")
            } else {
                (v, "")
            };
            if let Ok(f) = num.parse::<f64>() {
                return format!("Audio: {:.0}%{muted}", f * 100.0);
            }
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

/// Full display info for `sysinfo display`.
///
/// `xrandr` only reports real outputs under X11 (or a single XWayland virtual
/// output under Wayland — not the true per-monitor layout). So on a native
/// Wayland session we ask the compositor's own tool first — `kscreen-doctor`
/// (KDE), `wlr-randr` (wlroots), or `swaymsg` (Sway) — and only fall back to
/// xrandr on X11, then to the resolution-less drm-sysfs listing as a last
/// resort. This is the Gap-D fix for the previous xrandr-only path that
/// degraded on Wayland (the project's own target).
fn read_display() -> String {
    // Wayland: try the compositor-native tool before xrandr.
    if crate::context::is_wayland()
        && let Some(out) = read_display_wayland()
        && !out.is_empty()
    {
        return out;
    }

    let mut lines = Vec::new();

    // X11 / XWayland: xrandr.
    if let Ok(xr) = run_cmd("xrandr", &["--current"]) {
        for line in xr.lines() {
            if line.contains(" connected") {
                // e.g. "eDP-1 connected 1920x1080+0+0 (...) 344mm x 193mm"
                let parts: Vec<&str> = line.split_whitespace().collect();
                let name = parts.first().unwrap_or(&"?");
                let mut resolution = "";
                let mut is_primary = false;
                for p in &parts[2..] {
                    if *p == "primary" {
                        is_primary = true;
                        continue;
                    }
                    // Resolution looks like "1920x1080+0+0"
                    if p.contains('x') && p.contains('+') {
                        resolution = p.split('+').next().unwrap_or(p);
                        break;
                    }
                }
                let primary_tag = if is_primary { " (primary)" } else { "" };
                // Try to get refresh rate from the next lines
                lines.push(format!("{name}: {resolution}{primary_tag}"));
            }
            // Refresh rate line: "   1920x1080     60.00*+  59.94  ..."
            if line.starts_with("   ") && line.contains('*') {
                let trimmed = line.trim();
                if let Some(star_pos) = trimmed.find('*') {
                    // Walk backwards from * to find the Hz value
                    let before_star = &trimmed[..star_pos];
                    if let Some(hz) = before_star.split_whitespace().last()
                        && let Some(last_line) = lines.last_mut()
                    {
                        last_line.push_str(&format!(" @ {hz}Hz"));
                    }
                }
            }
        }
    }

    // Physical size from xrandr output isn't great, check drm for EDID model names
    if lines.is_empty() {
        // Fallback: list drm connectors
        if let Ok(entries) = fs::read_dir("/sys/class/drm") {
            for entry in entries.flatten() {
                let dir = entry.path();
                let status = fs::read_to_string(dir.join("status")).unwrap_or_default();
                if status.trim() == "connected" {
                    let name = dir
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    lines.push(format!("{name}: connected"));
                }
            }
        }
    }

    if lines.is_empty() {
        "No display info available".to_string()
    } else {
        lines.join("\n")
    }
}

/// Query per-monitor info on a native Wayland session via the compositor's own
/// CLI. Returns `None` if no such tool is installed (caller falls back to
/// xrandr / drm-sysfs). Order: wlr-randr (wlroots/Sway/Hyprland), kscreen-doctor
/// (KDE), swaymsg (Sway JSON).
fn read_display_wayland() -> Option<String> {
    // wlr-randr: blocks of "OUTPUT "Name"\n  ... \n  Current mode: 1920x1080 ...".
    if let Ok(out) = run_cmd("wlr-randr", &[]) {
        let mut lines = Vec::new();
        let mut current: Option<String> = None;
        for raw in out.lines() {
            // A new output starts at column 0 (no leading whitespace).
            if !raw.starts_with(char::is_whitespace) && !raw.trim().is_empty() {
                if let Some(name) = raw.split_whitespace().next() {
                    current = Some(name.to_string());
                }
            } else if let Some(rest) = raw.trim().strip_prefix("Enabled: ") {
                // no-op; kept for clarity of format
                let _ = rest;
            } else if raw.contains('*') || raw.trim().starts_with("Current") {
                // A current-mode line: "1920x1080 px, 60.000 Hz" (possibly '*').
                if let Some(name) = &current {
                    let mode = raw
                        .trim()
                        .trim_start_matches("Current mode:")
                        .trim()
                        .trim_end_matches('*')
                        .trim();
                    lines.push(format!("{name}: {mode}"));
                    current = None; // one line per output is enough
                }
            }
        }
        if !lines.is_empty() {
            return Some(lines.join("\n"));
        }
    }

    // kscreen-doctor -o (KDE). See parse_kscreen for the format.
    if let Ok(out) = run_cmd("kscreen-doctor", &["-o"]) {
        let parsed = parse_kscreen(&strip_ansi(&out));
        if !parsed.is_empty() {
            return Some(parsed);
        }
    }

    None
}

/// Parse `kscreen-doctor -o` output (already ANSI-stripped) into one line per
/// enabled output. Split out for testing against real output.
///
/// Each output is a block starting `Output: <id> <name> <uuid>`, followed by
/// indented lines including `enabled`/`disabled` and a `Modes:` line whose
/// current mode is the token marked with `*`, e.g. `9:1920x1080@24.00*` (the
/// leading `N:` is the mode index; a `!` marks the preferred mode).
fn parse_kscreen(clean: &str) -> String {
    let mut lines = Vec::new();
    for block in clean.split("Output:").skip(1) {
        let name = block.split_whitespace().nth(1).unwrap_or("?");
        if !block.contains("enabled") {
            continue; // skip disabled/disconnected outputs
        }
        // Current mode = the Modes token containing '*'. Strip the "N:" index
        // prefix and the '*'/'!' markers, then prettify "1920x1080@24.00".
        let mode = block
            .split_whitespace()
            .find(|t| t.contains('*') && t.contains('x'))
            .map(|t| {
                let t = t.trim_end_matches(['*', '!']);
                // Drop a leading "N:" mode index if present.
                let t = t.split_once(':').map(|(_, rest)| rest).unwrap_or(t);
                format!("{}Hz", t.replace('@', " @ "))
            })
            .unwrap_or_default();
        if mode.is_empty() {
            lines.push(format!("{name}: enabled"));
        } else {
            lines.push(format!("{name}: {mode}"));
        }
    }
    lines.join("\n")
}

/// Strip ANSI color/escape sequences from CLI output (kscreen-doctor colorizes).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // ESC — skip until a letter terminates the sequence (CSI ... [a-zA-Z]).
            for e in chars.by_ref() {
                if e.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// OS / System
// ---------------------------------------------------------------------------

/// Full OS info for `sysinfo os`.
fn read_os() -> String {
    let mut lines = Vec::new();

    // Distro name + version from os-release
    if let Ok(release) = fs::read_to_string("/etc/os-release") {
        let mut name = String::new();
        let mut version = String::new();
        for line in release.lines() {
            if let Some(n) = line.strip_prefix("PRETTY_NAME=") {
                name = n.trim_matches('"').to_string();
            } else if let Some(v) = line.strip_prefix("VERSION=") {
                version = v.trim_matches('"').to_string();
            }
        }
        if !name.is_empty() {
            lines.push(name);
        } else if !version.is_empty() {
            lines.push(version);
        }
    }

    // Kernel
    if let Ok(kernel) = run_cmd("uname", &["-r"]) {
        lines.push(format!("Kernel: {}", kernel.trim()));
    }

    // Hostname
    if let Ok(host) = run_cmd("hostname", &[]) {
        lines.push(format!("Host: {}", host.trim()));
    }

    // Uptime (portable — see read_uptime).
    let uptime = read_uptime();
    if !uptime.is_empty() {
        lines.push(uptime);
    }

    // Desktop environment
    if let Ok(de) = std::env::var("XDG_CURRENT_DESKTOP") {
        lines.push(format!("Desktop: {de}"));
    }

    // Session type (Wayland/X11)
    if let Ok(session) = std::env::var("XDG_SESSION_TYPE") {
        lines.push(format!("Session: {session}"));
    }

    if lines.is_empty() {
        "No OS info available".to_string()
    } else {
        lines.join("\n")
    }
}

// ---------------------------------------------------------------------------
// Speed test
// ---------------------------------------------------------------------------

/// Quick speed test using curl download + ping latency.
/// Downloads a 10MB test file from Cloudflare and measures throughput.
fn read_speedtest() -> String {
    let mut lines = Vec::new();

    // Latency — ping Cloudflare DNS
    if let Ok(ping) = run_cmd(
        "sh",
        &[
            "-c",
            "ping -c 3 -W 2 1.1.1.1 2>/dev/null | tail -1 | cut -d/ -f5",
        ],
    ) {
        let ping = ping.trim();
        if !ping.is_empty() {
            lines.push(format!("Ping: {ping} ms (1.1.1.1)"));
        }
    }

    // Download speed — 10MB file from Cloudflare
    // curl -w outputs speed_download in bytes/sec
    if let Ok(dl) = run_cmd(
        "sh",
        &[
            "-c",
            "curl -s -o /dev/null -w '%{speed_download}' --max-time 15 'https://speed.cloudflare.com/__down?bytes=10000000'",
        ],
    ) {
        let dl = dl.trim();
        if let Ok(bps) = dl.parse::<f64>() {
            let mbps = bps * 8.0 / 1_000_000.0;
            lines.push(format!("Download: {mbps:.1} Mbps"));
        }
    }

    // Upload speed — 1MB payload to Cloudflare
    // Must use a temp file because curl can't determine content-length from stdin
    if let Ok(ul) = run_cmd(
        "sh",
        &[
            "-c",
            "f=$(mktemp); dd if=/dev/zero bs=1M count=1 of=\"$f\" 2>/dev/null; curl -s -o /dev/null -w '%{speed_upload}' --max-time 15 -X POST --data-binary @\"$f\" 'https://speed.cloudflare.com/__up'; rm -f \"$f\"",
        ],
    ) {
        let ul = ul.trim();
        if let Ok(bps) = ul.parse::<f64>() {
            let mbps = bps * 8.0 / 1_000_000.0;
            lines.push(format!("Upload: {mbps:.1} Mbps"));
        }
    }

    if lines.is_empty() {
        "Speed test failed — check internet connection".to_string()
    } else {
        lines.join("\n")
    }
}

fn run_cmd(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run {program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if stderr.is_empty() {
            Err(format!("{program} exited with {}", out.status))
        } else {
            Err(stderr)
        }
    }
}

// ── Portable readers (busybox/Alpine-safe) ─────────────────────────────────
//
// The overview/ip/mem readers used GNU-only tool flags (`hostname -I`,
// `free --si`, `df --total`) that busybox and some minimal images don't
// implement. These helpers read the same data from `/proc` and iproute2
// (universal on Linux) so the info shows up on Alpine/busybox too, and fall
// back to the GNU tools where a nicer human format is wanted.

/// Local IPv4 addresses, space-separated (like `hostname -I`), via iproute2
/// which is present on every modern Linux — unlike GNU `hostname -I`.
fn read_local_ips() -> String {
    // `ip -o -4 addr show scope global` → lines like
    // "2: wlan0    inet 192.168.1.5/24 brd ... scope global ...".
    if let Ok(out) = run_cmd("ip", &["-o", "-4", "addr", "show", "scope", "global"]) {
        let ips: Vec<String> = out
            .lines()
            .filter_map(|l| {
                let after = l.split("inet ").nth(1)?;
                let cidr = after.split_whitespace().next()?;
                Some(cidr.split('/').next()?.to_string())
            })
            .collect();
        if !ips.is_empty() {
            return ips.join(" ");
        }
    }
    // Last resort: GNU hostname (may not exist / lack -I on busybox).
    run_cmd("hostname", &["-I"]).unwrap_or_default()
}

/// Memory summary from `/proc/meminfo` (universal). Falls back to `free` for
/// its familiar table when meminfo can't be read.
fn read_mem_info() -> String {
    let Ok(meminfo) = fs::read_to_string("/proc/meminfo") else {
        return run_cmd("free", &["-h", "--si"]).unwrap_or_default();
    };
    // Values are in kB.
    let kb = |key: &str| -> Option<u64> {
        meminfo
            .lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
    };
    let total = kb("MemTotal:");
    let avail = kb("MemAvailable:");
    match (total, avail) {
        (Some(t), Some(a)) => {
            let used = t.saturating_sub(a);
            let gib = |kb: u64| format!("{:.1} GiB", kb as f64 / 1024.0 / 1024.0);
            format!(
                "Memory: {} used / {} total ({} available)",
                gib(used),
                gib(t),
                gib(a)
            )
        }
        _ => run_cmd("free", &["-h", "--si"]).unwrap_or_default(),
    }
}

/// Uptime, pretty ("up 3 hours, 12 minutes"). `uptime -p` is a procps-ng flag
/// busybox lacks; derive from `/proc/uptime` (universal) instead.
fn read_uptime() -> String {
    let Ok(raw) = fs::read_to_string("/proc/uptime") else {
        return run_cmd("uptime", &["-p"]).unwrap_or_default();
    };
    let secs = raw
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    format_uptime(secs)
}

/// Pure formatter for uptime seconds → "up 3 hours, 12 minutes". Split out so
/// it's unit-testable without reading `/proc`.
fn format_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    let mut parts = Vec::new();
    let unit = |n: u64, s: &str| format!("{n} {s}{}", if n == 1 { "" } else { "s" });
    if days > 0 {
        parts.push(unit(days, "day"));
    }
    if hours > 0 {
        parts.push(unit(hours, "hour"));
    }
    // Always show minutes when there are no larger units, so uptime is never blank.
    if mins > 0 || parts.is_empty() {
        parts.push(unit(mins, "minute"));
    }
    format!("up {}", parts.join(", "))
}

/// Disk summary. `df` itself is universal; only the GNU `--total`/`-x` flags
/// aren't. Pass a busybox-safe invocation and skip pseudo-filesystems by
/// filtering the output instead of relying on `-x`.
fn read_disk_info() -> String {
    // `-h` (human) and `-P` (POSIX one-line-per-fs) are both in busybox df.
    let Ok(out) = run_cmd("df", &["-hP"]) else {
        return String::new();
    };
    let mut lines: Vec<String> = Vec::new();
    for (i, line) in out.lines().enumerate() {
        if i == 0 {
            lines.push(line.to_string()); // header
            continue;
        }
        // Skip pseudo/virtual filesystems (what GNU `-x tmpfs -x devtmpfs` did,
        // generalized): match on the mount source in column 1.
        let src = line.split_whitespace().next().unwrap_or("");
        let skip = src.starts_with("tmpfs")
            || src.starts_with("devtmpfs")
            || src.starts_with("efivarfs")
            || src == "overlay"
            || src.starts_with("/dev/loop");
        if !skip {
            lines.push(line.to_string());
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// EVERY alias the dispatch match accepts must carry the same consent as
    /// its canonical spelling, and consent-free subcommands must stay free.
    /// This is the drift that shipped: the Rules Engine knew "speedtest" but
    /// not "speed", "net" but not "network", and prompted for "ip" which
    /// never leaves the machine.
    #[test]
    fn alias_consent_matches_dispatch() {
        use crate::action_registry::{ConsentKind, RiskContext};
        let h = SysInfoHandler;
        let ctx = RiskContext::default();
        let kind = |args: &str| h.assess_risk(args, &ctx).consent.map(|c| c.kind);

        // Public-IP arms: both spellings, case/whitespace-insensitively.
        for args in ["net", "network", "NET", " network "] {
            assert_eq!(kind(args), Some(ConsentKind::PublicIp), "{args:?}");
        }
        // Speedtest arms.
        for args in ["speedtest", "speed", "Speed"] {
            assert_eq!(kind(args), Some(ConsentKind::LargeTransfer), "{args:?}");
        }
        // "ip" prints local addresses only — prompting for it was a false
        // prompt that trained click-through.
        for args in ["ip", "cpu", "mem", "disk", "", "os"] {
            assert_eq!(kind(args), None, "{args:?}");
        }
    }

    #[test]
    fn format_uptime_pluralizes_and_never_blank() {
        assert_eq!(format_uptime(0), "up 0 minutes");
        assert_eq!(format_uptime(60), "up 1 minute");
        assert_eq!(format_uptime(3600), "up 1 hour");
        assert_eq!(format_uptime(3660), "up 1 hour, 1 minute");
        assert_eq!(format_uptime(90000), "up 1 day, 1 hour");
        assert_eq!(
            format_uptime(2 * 86400 + 3 * 3600 + 12 * 60),
            "up 2 days, 3 hours, 12 minutes"
        );
    }

    #[test]
    fn strip_ansi_removes_color_codes() {
        // kscreen-doctor colorizes with SGR sequences like "\x1b[38;5;2m".
        let colored = "\u{1b}[38;5;2mOutput:\u{1b}[0m 1 eDP-1 enabled";
        assert_eq!(strip_ansi(colored), "Output: 1 eDP-1 enabled");
        // Plain text is unchanged.
        assert_eq!(strip_ansi("no escapes here"), "no escapes here");
    }

    #[test]
    fn sysinfo_args_flatten_from_structured_json() {
        // A structured caller sends the typed object; it flattens to the bare
        // subcommand the dispatch match parses.
        assert_eq!(sysinfo_args_to_flat(r#"{"topic":"cpu"}"#), "cpu");
        assert_eq!(sysinfo_args_to_flat(r#"{"topic":"battery"}"#), "battery");
        // No topic → the full-overview empty string.
        assert_eq!(sysinfo_args_to_flat("{}"), "");
        assert_eq!(sysinfo_args_to_flat(r#"{"topic":""}"#), "");
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(sysinfo_args_to_flat("cpu"), "cpu");
        assert_eq!(sysinfo_args_to_flat(""), "");
        assert_eq!(sysinfo_args_to_flat("{not json"), "{not json");
    }

    /// Drift test: the grammar's topic Choice IS `SUBCOMMANDS` — the same
    /// const dispatch, triggers, and completions are built from — and every
    /// value flattens to itself, i.e. to a string the dispatch match handles.
    /// The consent gate rides the same adapter, so a structured `net` call
    /// must reach the same consent verdict as the flat spelling.
    #[test]
    fn sysinfo_grammar_topics_match_dispatch_and_consent() {
        use crate::action_registry::{ConsentKind, RiskContext};
        let schema = SYSINFO_GRAMMAR.handler_schema();
        let en = schema["properties"]["topic"]["enum"].as_array().unwrap();
        assert_eq!(en.len(), SUBCOMMANDS.len());
        for v in SUBCOMMANDS {
            assert!(en.iter().any(|e| e == v), "enum missing {v}");
            assert_eq!(sysinfo_args_to_flat(&format!(r#"{{"topic":"{v}"}}"#)), *v);
        }
        // Nothing here mutates.
        assert!(!SYSINFO_GRAMMAR.verbs[0].mutates);
        // Consent flows through the structured path identically.
        let h = SysInfoHandler;
        let ctx = RiskContext::default();
        let kind = |args: &str| h.assess_risk(args, &ctx).consent.map(|c| c.kind);
        assert_eq!(kind(r#"{"topic":"net"}"#), Some(ConsentKind::PublicIp));
        assert_eq!(
            kind(r#"{"topic":"speedtest"}"#),
            Some(ConsentKind::LargeTransfer)
        );
        assert_eq!(kind(r#"{"topic":"ip"}"#), None);
    }

    #[test]
    fn parse_kscreen_extracts_name_and_current_mode() {
        // Real (ANSI-stripped) kscreen-doctor -o shape: the current mode is the
        // "N:WxH@Hz" token marked with '*'; '!' marks the preferred mode.
        let sample = "Output: 1 HDMI-A-1 da0542b8\n\
             \tenabled\n\tconnected\n\
             \tModes:  1:1920x1080@60.00!  9:1920x1080@24.00*  10:1920x1080@23.98\n\
             \tGeometry: 1920,0 1920x1080\n";
        assert_eq!(parse_kscreen(sample), "HDMI-A-1: 1920x1080 @ 24.00Hz");

        // A disabled output is skipped entirely.
        let disabled = "Output: 2 DP-2 disconnected\n\tdisabled\n";
        assert_eq!(parse_kscreen(disabled), "");

        // Enabled but no current-mode marker → falls back to "enabled".
        let no_mode = "Output: 3 eDP-1 enabled\n\tconnected\n";
        assert_eq!(parse_kscreen(no_mode), "eDP-1: enabled");
    }
}
