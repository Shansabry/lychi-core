//! systemd service control — a signature Linux feature that a macOS launcher
//! (Raycast/Alfred) fundamentally cannot offer. List running services, check a
//! service's status, and start/stop/restart/enable/disable it, straight from
//! the launcher.
//!
//! Adaptive by design (no hardcoded assumptions about the user's setup):
//!   - **Scope auto-detection** — a name is resolved against `--user` units
//!     first (no privilege needed); only if it isn't a user unit do we fall
//!     back to the system manager. So `service syncthing restart` just works
//!     for a user service, and `service nginx restart` targets the system one.
//!   - **Privilege via polkit, not a hanging password prompt** — mutating a
//!     *system* service needs root. Rather than `sudo` (which would block on a
//!     tty password prompt the launcher has no way to answer), we use `pkexec`
//!     when available, which pops the desktop's polkit auth dialog. If pkexec
//!     is missing we surface a clear message instead of hanging.
//!
//! Commands:
//!   - `services`                    → list running services
//!   - `service <name>`              → status of <name> (alias: `service <name> status`)
//!   - `service <name> start|stop|restart|reload|enable|disable`
//!
//! Read-only verbs (`services`, `status`) are Low risk and auto-execute; the
//! mutating verbs are gated to a confirmation by the Rules Engine.

use std::process::Command;

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CompletionItem, ExecContext, OutputType, RiskAssessment, RiskLevel,
};
use crate::error::LychiError;

pub struct ServicesHandler;

impl ServicesHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ServicesHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Verbs that change service state (as opposed to read-only status/list). Used
/// both here and by the Rules Engine to decide when to require confirmation.
pub const MUTATING_VERBS: &[&str] = &[
    "start", "stop", "restart", "reload", "enable", "disable", "kill",
];

/// Read-only verbs that never need confirmation.
const READONLY_VERBS: &[&str] = &["status", "show", "is-active", "is-enabled"];

fn have(tool: &str) -> bool {
    which::which(tool).is_ok()
}

/// Normalize a unit name — append `.service` if the user gave a bare name and
/// it has no unit suffix (so `nginx` → `nginx.service`, but `foo.socket` and
/// `foo.timer` are left alone).
fn normalize_unit(name: &str) -> String {
    let known_suffixes = [
        ".service", ".socket", ".timer", ".target", ".mount", ".path", ".scope",
    ];
    if known_suffixes.iter().any(|s| name.ends_with(s)) {
        name.to_string()
    } else {
        format!("{name}.service")
    }
}

/// Does this unit exist in the `--user` manager? Determines which scope to act
/// in without the caller having to know or specify.
fn is_user_unit(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["--user", "--quiet", "list-unit-files", unit])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
        // Fall back to is-active for transient units not in list-unit-files.
        || Command::new("systemctl")
            .args(["--user", "is-active", unit])
            .output()
            .map(|o| {
                let s = String::from_utf8_lossy(&o.stdout);
                let s = s.trim();
                s == "active" || s == "activating" || s == "reloading"
            })
            .unwrap_or(false)
}

/// Run `systemctl` and capture stdout, returning a friendly error on failure.
fn systemctl(scope_user: bool, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("systemctl");
    if scope_user {
        cmd.arg("--user");
    }
    let output = cmd
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run systemctl: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if output.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        Err(msg)
    }
}

/// List running services (system scope) as a compact table.
fn list_running() -> Result<String, String> {
    let out = systemctl(
        false,
        &[
            "list-units",
            "--type=service",
            "--state=running",
            "--no-pager",
            "--no-legend",
            "--plain",
        ],
    )?;
    let mut lines: Vec<String> = Vec::new();
    for line in out.lines() {
        // Columns: UNIT LOAD ACTIVE SUB DESCRIPTION...
        let mut parts = line.split_whitespace();
        if let Some(unit) = parts.next() {
            let desc = parts.clone().skip(3).collect::<Vec<_>>().join(" ");
            let name = unit.strip_suffix(".service").unwrap_or(unit);
            if desc.is_empty() {
                lines.push(name.to_string());
            } else {
                lines.push(format!("{name}  —  {desc}"));
            }
        }
    }
    if lines.is_empty() {
        return Ok("No running services".to_string());
    }
    let count = lines.len();
    Ok(format!(
        "{count} running service{}:\n\n{}",
        if count == 1 { "" } else { "s" },
        lines.join("\n")
    ))
}

/// Show a service's status (active state + enabled state + a couple of lines).
fn status(unit: &str) -> Result<String, String> {
    let user = is_user_unit(unit);
    let scope_label = if user { "user" } else { "system" };
    let active = systemctl(user, &["is-active", unit])
        .unwrap_or_else(|e| e)
        .trim()
        .to_string();
    let enabled = systemctl(user, &["is-enabled", unit])
        .unwrap_or_else(|_| "unknown".to_string())
        .trim()
        .to_string();
    let icon = match active.as_str() {
        "active" => "●",
        "failed" => "✗",
        _ => "○",
    };
    Ok(format!(
        "{icon} {unit} ({scope_label})\n  active:  {active}\n  enabled: {enabled}"
    ))
}

/// Perform a mutating action, choosing scope and privilege escalation adaptively.
fn control(verb: &str, unit: &str) -> Result<String, String> {
    let user = is_user_unit(unit);
    if user {
        // User scope never needs root.
        systemctl(true, &[verb, unit])?;
        return Ok(format!("{verb} {unit} (user) ✓"));
    }

    // System scope: needs privilege. Prefer polkit (pkexec) so the desktop can
    // prompt graphically instead of us hanging on a tty password prompt.
    if have("pkexec") {
        let output = Command::new("pkexec")
            .args(["systemctl", verb, unit])
            .output()
            .map_err(|e| format!("Failed to run pkexec: {e}"))?;
        if output.status.success() {
            return Ok(format!("{verb} {unit} (system) ✓"));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        // pkexec exits 126/127 when auth is dismissed or fails.
        if matches!(output.status.code(), Some(126) | Some(127)) {
            return Err("Authorization dismissed or failed".to_string());
        }
        return Err(if stderr.trim().is_empty() {
            format!("Failed to {verb} {unit}")
        } else {
            stderr.trim().to_string()
        });
    }

    // No pkexec — try unprivileged (works if polkit rules already allow it),
    // otherwise report clearly rather than hang.
    match systemctl(false, &[verb, unit]) {
        Ok(_) => Ok(format!("{verb} {unit} (system) ✓")),
        Err(e) => Err(format!(
            "{e}\n\nManaging a system service needs root — install `pkexec` (polkit) \
             for a graphical prompt, or run: sudo systemctl {verb} {unit}"
        )),
    }
}

/// Parse `<name> [verb]` args. Returns (unit, verb) where verb defaults to
/// "status". A leading verb form (`start nginx`) is also accepted.
fn parse(args: &str) -> Option<(String, String)> {
    let toks: Vec<&str> = args.split_whitespace().collect();
    match toks.as_slice() {
        [] => None,
        [name] => Some((normalize_unit(name), "status".to_string())),
        // `service start nginx` (verb first) or `service nginx start` (name first)
        [a, b] => {
            let a_is_verb = MUTATING_VERBS.contains(a) || READONLY_VERBS.contains(a);
            if a_is_verb {
                Some((normalize_unit(b), a.to_string()))
            } else {
                Some((normalize_unit(a), b.to_string()))
            }
        }
        _ => Some((normalize_unit(toks[0]), toks[1].to_string())),
    }
}

#[async_trait]
impl ActionHandler for ServicesHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["service", "systemctl"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "service"
    }

    fn description(&self) -> &str {
        "Control systemd services: list, status, start/stop/restart"
    }

    fn assess_risk(
        &self,
        args: &str,
        _ctx: &crate::action_registry::RiskContext<'_>,
    ) -> RiskAssessment {
        // Read-only verbs (status/list) auto-execute; mutating verbs
        // (start/stop/restart/…) need confirmation. This decision lives here,
        // where the handler already knows its verbs — not in the Rules Engine.
        if is_mutating(args) {
            RiskAssessment::confirm(format!("Run 'systemctl {}'?", args.trim()))
        } else {
            RiskAssessment::level(RiskLevel::Low)
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let p = partial.trim();
        // With a bare verb typed, offer the common actions as fill hints.
        let hints = [
            ("status", "service <name> status", "Show a service's status"),
            ("start", "service <name> start", "Start a service"),
            ("stop", "service <name> stop", "Stop a service"),
            ("restart", "service <name> restart", "Restart a service"),
            ("enable", "service <name> enable", "Enable at boot"),
            ("disable", "service <name> disable", "Disable at boot"),
        ];
        if p.is_empty() {
            return hints
                .iter()
                .enumerate()
                .map(|(i, (_, label, desc))| {
                    CompletionItem::new(
                        (*label).to_string(),
                        Some("__none__".into()),
                        900 - i as u16,
                    )
                    .with_fill("service ")
                    .with_description((*desc).to_string())
                })
                .collect();
        }
        Vec::new()
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        if !have("systemctl") {
            return Ok(ActionResult::err(
                "systemctl not found — this system doesn't appear to use systemd.",
            ));
        }

        let trimmed = args.trim();

        // Bare `service` (no args) lists running services, same as `services`.
        if trimmed.is_empty() {
            return match list_running() {
                Ok(out) => Ok(ActionResult::ok(out, OutputType::Terminal)),
                Err(e) => Ok(ActionResult::err(e)),
            };
        }

        let Some((unit, verb)) = parse(trimmed) else {
            return Ok(ActionResult::err(
                "Usage: service <name> [status|start|stop|restart]",
            ));
        };

        let result = match verb.as_str() {
            "status" | "show" | "is-active" | "is-enabled" => status(&unit),
            v if MUTATING_VERBS.contains(&v) => control(v, &unit),
            other => Err(format!(
                "Unknown action '{other}'. Use: status, start, stop, restart, enable, disable"
            )),
        };

        match result {
            Ok(out) => Ok(ActionResult::ok(out, OutputType::Terminal)),
            Err(e) => Ok(ActionResult::err(e)),
        }
    }
}

/// Separate handler for the bare `services` keyword → list running services.
/// Kept as a thin alias so the plural word is discoverable and always read-only.
pub struct ServicesListHandler;

impl ServicesListHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ServicesListHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ActionHandler for ServicesListHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["services"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "services"
    }

    fn description(&self) -> &str {
        "List running systemd services"
    }

    async fn execute(&self, _ctx: &ExecContext, _args: &str) -> Result<ActionResult, LychiError> {
        if !have("systemctl") {
            return Ok(ActionResult::err(
                "systemctl not found — this system doesn't appear to use systemd.",
            ));
        }
        match list_running() {
            Ok(out) => Ok(ActionResult::ok(out, OutputType::Terminal)),
            Err(e) => Ok(ActionResult::err(e)),
        }
    }
}

/// Is this a mutating `service` invocation (used by the Rules Engine to decide
/// whether to confirm)? Read-only status/list calls return false.
pub fn is_mutating(args: &str) -> bool {
    match parse(args.trim()) {
        Some((_, verb)) => MUTATING_VERBS.contains(&verb.as_str()),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assess_risk_confirms_mutating_verbs_only() {
        let h = ServicesHandler::new();
        assert_eq!(h.assess_risk("nginx restart", &Default::default()).level, RiskLevel::Medium);
        assert_eq!(h.assess_risk("stop nginx", &Default::default()).level, RiskLevel::Medium);
        // read-only → auto-execute
        assert_eq!(h.assess_risk("nginx", &Default::default()).level, RiskLevel::Low);
        assert_eq!(h.assess_risk("nginx status", &Default::default()).level, RiskLevel::Low);
    }

    #[test]
    fn normalize_appends_service_suffix() {
        assert_eq!(normalize_unit("nginx"), "nginx.service");
        assert_eq!(normalize_unit("nginx.service"), "nginx.service");
        assert_eq!(normalize_unit("foo.socket"), "foo.socket");
        assert_eq!(normalize_unit("bar.timer"), "bar.timer");
    }

    #[test]
    fn parse_name_only_defaults_to_status() {
        assert_eq!(
            parse("nginx"),
            Some(("nginx.service".to_string(), "status".to_string()))
        );
    }

    #[test]
    fn parse_name_then_verb() {
        assert_eq!(
            parse("nginx restart"),
            Some(("nginx.service".to_string(), "restart".to_string()))
        );
    }

    #[test]
    fn parse_verb_then_name() {
        // `service start nginx` — verb-first form is accepted too.
        assert_eq!(
            parse("start nginx"),
            Some(("nginx.service".to_string(), "start".to_string()))
        );
    }

    #[test]
    fn parse_empty_is_none() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("   "), None);
    }

    #[test]
    fn is_mutating_classifies_verbs() {
        assert!(is_mutating("nginx restart"));
        assert!(is_mutating("stop nginx"));
        assert!(is_mutating("nginx enable"));
        // read-only
        assert!(!is_mutating("nginx"));
        assert!(!is_mutating("nginx status"));
        assert!(!is_mutating(""));
    }

    #[test]
    fn mutating_verbs_are_stable() {
        // Guard against accidental edits that would let a mutation skip confirm.
        for v in ["start", "stop", "restart", "enable", "disable"] {
            assert!(MUTATING_VERBS.contains(&v), "{v} must be mutating");
        }
    }
}
