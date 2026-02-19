use std::fs;
use std::process::Command;
use std::time::Instant;

use async_trait::async_trait;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem, OutputType};
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

#[async_trait]
impl ActionHandler for SysInfoHandler {
    fn id(&self) -> &str {
        "sysinfo"
    }

    fn description(&self) -> &str {
        "System info — ip, cpu, mem, disk, temp, gpu, battery, net, audio, display, os"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let start = Instant::now();
        let cmd = args.trim().to_lowercase();

        let output = match cmd.as_str() {
            "" => {
                // No subcommand — show a brief overview
                let ip = run_cmd("hostname", &["-I"]).unwrap_or_default();
                let mem = run_cmd("free", &["-h", "--si"]).unwrap_or_default();
                let disk = run_cmd("df", &["-h", "--total", "-x", "tmpfs", "-x", "devtmpfs"])
                    .unwrap_or_default();
                let uptime = run_cmd("uptime", &["-p"]).unwrap_or_default();
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
                let local = run_cmd("hostname", &["-I"]).unwrap_or_else(|e| e);
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
            "mem" => run_cmd("free", &["-h", "--si"]),
            "disk" => run_cmd("df", &["-h", "--total", "-x", "tmpfs", "-x", "devtmpfs"]),
            "temp" => Ok(read_temps()),
            "gpu" => Ok(read_gpu()),
            "battery" | "bat" => Ok(read_battery()),
            "net" | "network" => Ok(read_network()),
            "audio" | "sound" | "volume" => Ok(read_audio()),
            "display" | "monitor" | "screen" => Ok(read_display()),
            "os" | "system" | "uptime" => Ok(read_os()),
            "speedtest" | "speed" => Ok(read_speedtest()),
            _ => Err(format!(
                "Unknown: '{cmd}'. Try: ip, cpu, mem, disk, temp, gpu, battery, net, audio, display, os, speedtest"
            )),
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        match output {
            Ok(text) => Ok(ActionResult {
                success: true,
                output: Some(text),
                error: None,
                duration_ms,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: Some(OutputType::Terminal),
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
fn read_display() -> String {
    let mut lines = Vec::new();

    // Try xrandr (works on both X11 and XWayland)
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

    // Uptime
    if let Ok(uptime) = run_cmd("uptime", &["-p"]) {
        lines.push(uptime.trim().to_string());
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
