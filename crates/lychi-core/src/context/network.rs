//! Network context detection — SSID and VPN status.
//!
//! Detects active WiFi SSID via `nmcli` and VPN interfaces via `/sys/class/net/`.
//! VPN detection is pure filesystem (< 1ms), SSID detection spawns `nmcli` (~10-50ms).

use std::process::Command;

use serde::{Deserialize, Serialize};

/// Network context snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkContext {
    /// Active WiFi SSID, if connected.
    pub ssid: Option<String>,
    /// Whether a VPN tunnel is active (tun/wg/ppp interface detected).
    pub vpn_active: bool,
}

/// Pre-populate the network cache at startup so the first summon doesn't
/// block on `nmcli`. Call from `spawn_blocking` during app setup.
pub fn warmup() {
    let t0 = std::time::Instant::now();
    let result = detect();
    super::cache::set_network(&result);
    tracing::info!(
        "[network] warmup done: {}ms (ssid={:?}, vpn={})",
        t0.elapsed().as_millis(),
        result.as_ref().and_then(|n| n.ssid.as_deref()),
        result.as_ref().map(|n| n.vpn_active).unwrap_or(false)
    );
}

/// Detect network context. Returns `None` if no useful info is available.
pub fn detect() -> Option<NetworkContext> {
    let ssid = detect_ssid();
    let vpn_active = detect_vpn();

    if ssid.is_none() && !vpn_active {
        return None;
    }

    Some(NetworkContext { ssid, vpn_active })
}

/// Detect active WiFi SSID via `nmcli`.
///
/// Parses `nmcli -t -f active,ssid dev wifi` output for "yes:<SSID>" lines.
fn detect_ssid() -> Option<String> {
    let output = Command::new("nmcli")
        .args(["-t", "-f", "active,ssid", "dev", "wifi"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        // Format: "yes:MyNetwork" or "no:OtherNetwork"
        if let Some(ssid) = line.strip_prefix("yes:") {
            let ssid = ssid.trim();
            if !ssid.is_empty() {
                return Some(ssid.to_string());
            }
        }
    }
    None
}

/// Detect VPN by scanning `/sys/class/net/` for tunnel interfaces.
///
/// Pure filesystem check — no subprocess, < 1ms.
fn detect_vpn() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/net/") else {
        return false;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("tun")
            || name.starts_with("wg")
            || name.starts_with("ppp")
            || name.starts_with("tap")
            || name.starts_with("tailscale")
            || name.starts_with("proton")
            || name.starts_with("nordlynx")
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_vpn_no_crash() {
        // Should not crash even if /sys/class/net/ doesn't exist (e.g. macOS/CI)
        let _ = detect_vpn();
    }

    #[test]
    fn test_detect_ssid_no_crash() {
        // Should not crash even if nmcli is not installed
        let _ = detect_ssid();
    }

    #[test]
    fn test_detect_no_crash() {
        // Full detection should never crash
        let _ = detect();
    }
}
