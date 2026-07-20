//! Package manager — search and install system packages, straight from the
//! launcher. Another signature Linux capability a macOS launcher can't offer.
//!
//! Adaptive by design (per Lychi's no-hardcoding rule): the native manager is
//! *detected*, not assumed. We probe for dnf / apt / pacman / zypper (whichever
//! the distro ships) and treat flatpak as an always-optional universal layer on
//! top. So `install neovim` runs `dnf install` on Fedora, `apt install` on
//! Debian, `pacman -S` on Arch — with zero configuration.
//!
//! Commands:
//!   - `search <query>`   → search available packages (read-only)
//!   - `install <pkg>`    → install a package (needs root → pkexec)
//!
//! Search is Low risk and auto-executes. Install mutates the system and is gated
//! to a confirmation by the Rules Engine, then escalates via polkit (`pkexec`)
//! rather than `sudo` — which would hang on a tty password prompt the launcher
//! can't answer.

use std::process::Command;

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CompletionItem, ExecContext, OutputType, RiskAssessment, RiskLevel,
};
use crate::error::LychiError;

pub struct PackagesHandler;

impl PackagesHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PackagesHandler {
    fn default() -> Self {
        Self::new()
    }
}

fn have(tool: &str) -> bool {
    which::which(tool).is_ok()
}

/// The native system package manager, detected once per call. Order follows the
/// major distro families; the first installed one wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Manager {
    Dnf,
    Apt,
    Pacman,
    Zypper,
}

impl Manager {
    fn detect() -> Option<Manager> {
        // dnf before yum-era, apt covers Debian/Ubuntu, then Arch, then SUSE.
        for (bin, mgr) in [
            ("dnf", Manager::Dnf),
            ("apt", Manager::Apt),
            ("pacman", Manager::Pacman),
            ("zypper", Manager::Zypper),
        ] {
            if have(bin) {
                return Some(mgr);
            }
        }
        None
    }

    fn binary(self) -> &'static str {
        match self {
            Manager::Dnf => "dnf",
            Manager::Apt => "apt",
            Manager::Pacman => "pacman",
            Manager::Zypper => "zypper",
        }
    }

    /// Search args (read-only, no privilege).
    fn search_args(self, query: &str) -> Vec<String> {
        let q = query.to_string();
        match self {
            Manager::Dnf => vec!["search".into(), q],
            Manager::Apt => vec!["search".into(), q],
            Manager::Pacman => vec!["-Ss".into(), q],
            Manager::Zypper => vec!["--no-refresh".into(), "search".into(), q],
        }
    }

    /// Install args (mutating, non-interactive — the caller escalates privilege).
    fn install_args(self, pkg: &str) -> Vec<String> {
        let p = pkg.to_string();
        match self {
            Manager::Dnf => vec!["install".into(), "-y".into(), p],
            Manager::Apt => vec!["install".into(), "-y".into(), p],
            Manager::Pacman => vec!["-S".into(), "--noconfirm".into(), p],
            Manager::Zypper => vec!["--non-interactive".into(), "install".into(), p],
        }
    }
}

/// Parse a manager's search output into a compact `name — summary` list. Each
/// manager formats differently, so normalize to the essentials. Best-effort:
/// unrecognized lines are skipped rather than shown raw.
fn parse_search(mgr: Manager, raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    match mgr {
        // dnf: "Matched fields:" headers, then " pkg.arch\tSummary".
        Manager::Dnf => {
            for line in raw.lines() {
                let line = line.trim_end();
                if line.starts_with("Matched fields") || line.trim().is_empty() {
                    continue;
                }
                let t = line.trim_start();
                if let Some((name_arch, summary)) = t.split_once('\t') {
                    let name = name_arch.split('.').next().unwrap_or(name_arch);
                    out.push(format!("{}  —  {}", name.trim(), summary.trim()));
                } else if let Some((name_arch, summary)) = t.split_once("  ") {
                    let name = name_arch.split('.').next().unwrap_or(name_arch);
                    out.push(format!("{}  —  {}", name.trim(), summary.trim()));
                }
            }
        }
        // apt: "name/suite version arch\n  Summary".
        Manager::Apt => {
            let mut lines = raw.lines().peekable();
            while let Some(line) = lines.next() {
                if line.starts_with("Sorting") || line.starts_with("Full Text") {
                    continue;
                }
                if let Some((name, _)) = line.split_once('/') {
                    let summary = lines
                        .peek()
                        .filter(|l| l.starts_with("  "))
                        .map(|l| l.trim())
                        .unwrap_or("");
                    if !summary.is_empty() {
                        lines.next();
                    }
                    out.push(format!("{}  —  {}", name.trim(), summary));
                }
            }
        }
        // pacman: "repo/name version\n    Summary".
        Manager::Pacman => {
            let mut lines = raw.lines().peekable();
            while let Some(line) = lines.next() {
                if let Some((repo_name, _)) = line.split_once(' ') {
                    let name = repo_name.split('/').next_back().unwrap_or(repo_name);
                    let summary = lines
                        .peek()
                        .filter(|l| l.starts_with("    ") || l.starts_with('\t'))
                        .map(|l| l.trim())
                        .unwrap_or("");
                    if !summary.is_empty() {
                        lines.next();
                    }
                    out.push(format!("{}  —  {}", name.trim(), summary));
                }
            }
        }
        // zypper: pipe-delimited table "S | Name | Summary | Type".
        Manager::Zypper => {
            for line in raw.lines() {
                let cols: Vec<&str> = line.split('|').map(|c| c.trim()).collect();
                if cols.len() >= 3 && cols[1] != "Name" && !cols[1].is_empty() {
                    out.push(format!("{}  —  {}", cols[1], cols[2]));
                }
            }
        }
    }
    out
}

/// Run a read-only search. Returns a combined list from the native manager and
/// (if present) flatpak, capped so the panel stays usable.
fn search(query: &str) -> Result<String, String> {
    const CAP: usize = 25;
    let mut sections: Vec<String> = Vec::new();

    if let Some(mgr) = Manager::detect() {
        let output = Command::new(mgr.binary())
            .args(mgr.search_args(query))
            .output()
            .map_err(|e| format!("Failed to run {}: {e}", mgr.binary()))?;
        // Search tools exit non-zero on "no matches" — that's not an error.
        let text = String::from_utf8_lossy(&output.stdout);
        let mut items = parse_search(mgr, &text);
        let total = items.len();
        items.truncate(CAP);
        if !items.is_empty() {
            let more = if total > CAP {
                format!("\n… and {} more", total - CAP)
            } else {
                String::new()
            };
            sections.push(format!("{}:\n{}{}", mgr.binary(), items.join("\n"), more));
        }
    }

    if have("flatpak") {
        if let Ok(output) = Command::new("flatpak").args(["search", query]).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut items: Vec<String> = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| {
                    // flatpak columns are tab-separated: Name Desc AppID Version Branch Remote
                    let cols: Vec<&str> = l.split('\t').map(|c| c.trim()).collect();
                    if cols.len() >= 3 && cols[0] != "Name" {
                        Some(format!(
                            "{}  —  {}  ({})",
                            cols[0],
                            cols.get(1).unwrap_or(&""),
                            cols[2]
                        ))
                    } else {
                        None
                    }
                })
                .collect();
            let total = items.len();
            items.truncate(CAP);
            if !items.is_empty() {
                let more = if total > CAP {
                    format!("\n… and {} more", total - CAP)
                } else {
                    String::new()
                };
                sections.push(format!("flatpak:\n{}{}", items.join("\n"), more));
            }
        }
    }

    if sections.is_empty() {
        if Manager::detect().is_none() && !have("flatpak") {
            return Err(
                "No supported package manager found (dnf, apt, pacman, zypper, or flatpak)."
                    .to_string(),
            );
        }
        return Ok(format!("No packages found for \"{query}\""));
    }
    Ok(sections.join("\n\n"))
}

/// Install a package. Native install needs root → pkexec (graphical polkit
/// prompt). A `flatpak:` prefix targets flatpak (user scope, no root).
fn install(pkg: &str) -> Result<String, String> {
    let pkg = pkg.trim();
    if pkg.is_empty() {
        return Err("Usage: install <package>".to_string());
    }

    // Explicit flatpak target: `install flatpak:org.foo.Bar` or `install flatpak org.foo`.
    if let Some(app) = pkg
        .strip_prefix("flatpak:")
        .or_else(|| pkg.strip_prefix("flatpak "))
    {
        if !have("flatpak") {
            return Err("flatpak is not installed".to_string());
        }
        let status = Command::new("flatpak")
            .args(["install", "-y", app.trim()])
            .status()
            .map_err(|e| format!("Failed to run flatpak: {e}"))?;
        return if status.success() {
            Ok(format!("Installed {app} via flatpak ✓"))
        } else {
            Err(format!("flatpak failed to install {app}"))
        };
    }

    let Some(mgr) = Manager::detect() else {
        return Err("No native package manager found (dnf, apt, pacman, zypper).".to_string());
    };

    let args = mgr.install_args(pkg);
    // Native install needs root. Prefer pkexec (graphical) over a hanging sudo.
    if have("pkexec") {
        let mut full = vec![mgr.binary().to_string()];
        full.extend(args);
        let status = Command::new("pkexec")
            .args(&full)
            .status()
            .map_err(|e| format!("Failed to run pkexec: {e}"))?;
        return match status.code() {
            Some(0) => Ok(format!("Installed {pkg} via {} ✓", mgr.binary())),
            Some(126) | Some(127) => Err("Authorization dismissed or failed".to_string()),
            _ => Err(format!("{} failed to install {pkg}", mgr.binary())),
        };
    }

    Err(format!(
        "Installing a package needs root — install `pkexec` (polkit) for a graphical \
         prompt, or run: sudo {} {}",
        mgr.binary(),
        args.join(" ")
    ))
}

#[async_trait]
impl ActionHandler for PackagesHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::{ArgTransform, Trigger};
        static TRIGGERS: &[Trigger] = &[
            Trigger::new(&["install"], ArgTransform::Prepend("install")),
            Trigger::keywords(&["pkg", "package"]),
        ];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "packages"
    }

    fn description(&self) -> &str {
        "Search and install system packages (dnf/apt/pacman/zypper/flatpak)"
    }

    fn assess_risk(&self, args: &str) -> RiskAssessment {
        // Search is read-only (auto); install mutates the system (root via
        // pkexec) and needs confirmation.
        if is_mutating(args) {
            RiskAssessment::confirm(format!("Install a package: {}?", args.trim()))
        } else {
            RiskAssessment::level(RiskLevel::Low)
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let p = partial.trim().to_ascii_lowercase();
        let hints = [
            ("search", "search <query>", "Search available packages"),
            ("install", "install <package>", "Install a package"),
        ];
        hints
            .iter()
            .filter(|(key, _, _)| p.is_empty() || key.starts_with(p.as_str()))
            .enumerate()
            .map(|(i, (_, label, desc))| {
                CompletionItem::new(
                    (*label).to_string(),
                    Some("__none__".into()),
                    850 - i as u16,
                )
                .with_description((*desc).to_string())
            })
            .collect()
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // Called as "search <q>" or "install <pkg>" (verb re-prepended by router).
        let trimmed = args.trim();
        let (verb, rest) = trimmed
            .split_once(char::is_whitespace)
            .unwrap_or((trimmed, ""));

        let result = match verb {
            "search" => search(rest.trim()),
            "install" => install(rest.trim()),
            _ => Err("Usage: search <query>  |  install <package>".to_string()),
        };

        match result {
            Ok(out) => Ok(ActionResult::ok(out, OutputType::Terminal)),
            Err(e) => Ok(ActionResult::err(e)),
        }
    }
}

/// Is this a mutating packages invocation (install)? Used by the Rules Engine to
/// decide whether to confirm. Search is read-only.
pub fn is_mutating(args: &str) -> bool {
    args.trim_start()
        .split_once(char::is_whitespace)
        .map(|(v, _)| v)
        .unwrap_or_else(|| args.trim())
        == "install"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assess_risk_confirms_install_not_search() {
        let h = PackagesHandler::new();
        assert_eq!(h.assess_risk("install neovim").level, RiskLevel::Medium);
        assert_eq!(h.assess_risk("search ripgrep").level, RiskLevel::Low);
    }

    #[test]
    fn install_and_search_args_per_manager() {
        assert_eq!(
            Manager::Dnf.install_args("vim"),
            vec!["install", "-y", "vim"]
        );
        assert_eq!(
            Manager::Pacman.install_args("vim"),
            vec!["-S", "--noconfirm", "vim"]
        );
        assert_eq!(Manager::Apt.search_args("vim"), vec!["search", "vim"]);
        assert_eq!(Manager::Pacman.search_args("vim"), vec!["-Ss", "vim"]);
    }

    #[test]
    fn parse_dnf_output() {
        let raw = "Matched fields: name (exact)\n ripgrep.x86_64\tLine-oriented search tool\nMatched fields: name\n ripgrep-all.noarch\tripgrep, but also search in PDFs";
        let items = parse_search(Manager::Dnf, raw);
        assert_eq!(items.len(), 2);
        assert!(
            items[0].starts_with("ripgrep  —  Line-oriented"),
            "{:?}",
            items[0]
        );
        assert!(items[1].starts_with("ripgrep-all  —  ripgrep, but also"));
    }

    #[test]
    fn parse_apt_output() {
        let raw = "Sorting...\nFull Text Search...\nripgrep/stable 13.0.0 amd64\n  Recursively search directories\nfd-find/stable 8.0 amd64\n  Simple find alternative";
        let items = parse_search(Manager::Apt, raw);
        assert_eq!(items.len(), 2);
        assert!(items[0].starts_with("ripgrep  —  Recursively search"));
        assert!(items[1].starts_with("fd-find  —  Simple find"));
    }

    #[test]
    fn parse_pacman_output() {
        let raw = "extra/ripgrep 13.0.0-3\n    A search tool that combines usability\ncommunity/fd 8.4.0-1\n    Simple, fast alternative to find";
        let items = parse_search(Manager::Pacman, raw);
        assert_eq!(items.len(), 2);
        assert!(
            items[0].starts_with("ripgrep  —  A search tool"),
            "{:?}",
            items[0]
        );
        assert!(items[1].starts_with("fd  —  Simple, fast"));
    }

    #[test]
    fn is_mutating_only_for_install() {
        assert!(is_mutating("install neovim"));
        assert!(!is_mutating("search neovim"));
        assert!(!is_mutating("search"));
    }

    #[test]
    fn install_rejects_empty() {
        assert!(install("").is_err());
    }
}
