pub mod path;
pub mod shell;
pub mod uri;
pub mod verbs;

use crate::action_registry::{ConsentKind, RiskAssessment, RiskLevel};
use crate::config::schema::PrivacyConfig;
use shell::ShellRules;

/// Pre-execution validation request.
pub struct ValidationRequest<'a> {
    pub action_id: &'a str,
    pub args: &'a str,
    pub routed_by: &'a str,
    /// The handler's own risk verdict for this invocation (level + optional
    /// custom message). Produced by `ActionHandler::assess_risk`. The Rules
    /// Engine layers only cross-cutting policy (shell denylist, privacy consent)
    /// on top of this — it no longer reaches into handler internals.
    pub risk: &'a RiskAssessment,
}

/// The outcome of pre-execution validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationDecision {
    /// Safe to execute immediately.
    Execute,
    /// Requires user confirmation before executing.
    Confirm { reason: String },
    /// Blocked — will not execute under any circumstances.
    Deny { reason: String },
}

/// Rules Engine — validates actions before execution.
///
/// Gates every execution path. Deterministic: same input → same decision.
#[derive(Clone)]
pub struct RulesEngine {
    shell_rules: ShellRules,
}

impl RulesEngine {
    pub fn new() -> Self {
        Self {
            shell_rules: ShellRules::new(),
        }
    }

    /// Validate whether an action should execute, require confirmation, or be denied.
    /// Privacy config gates network calls that send user data to third parties (C6).
    pub fn validate(&self, req: &ValidationRequest, privacy: &PrivacyConfig) -> ValidationDecision {
        let decision = self.decide(req, privacy);
        // Log every gate decision — this is the security-critical brick, and a
        // beta report of "it blocked my command" / "asked to confirm" needs a
        // trail. Deny/Confirm are notable (warn); Execute is routine (debug).
        match &decision {
            ValidationDecision::Deny { reason } => {
                tracing::warn!(action = %req.action_id, %reason, "[rules] DENY")
            }
            ValidationDecision::Confirm { reason } => {
                tracing::info!(action = %req.action_id, %reason, "[rules] CONFIRM")
            }
            ValidationDecision::Execute => {
                tracing::debug!(action = %req.action_id, "[rules] execute")
            }
        }
        decision
    }

    /// The pure decision logic (logged by `validate`).
    fn decide(&self, req: &ValidationRequest, privacy: &PrivacyConfig) -> ValidationDecision {
        // C6: privacy consent, checked first. The HANDLER declares what an
        // invocation discloses (it is the one parser of its own args — see
        // `ConsentKind`); this engine only holds it against what the user has
        // already granted. The engine used to keep its own list of sensitive
        // args, and the two parsers drifted three ways: `sysinfo speed` and
        // `sysinfo network` bypassed consent, `sysinfo ip` prompted falsely.
        if let Some(consent) = &req.risk.consent
            && !consent_granted(consent.kind, privacy)
        {
            return ValidationDecision::Confirm {
                reason: consent.prompt.clone(),
            };
        }

        // Cross-cutting policy the Rules Engine genuinely owns (not handler-local):
        // the shell denylist and force-kill safety. Everything else defers to the
        // handler's own risk assessment.
        match req.action_id {
            "run" => return self.shell_rules.validate(req.args),
            "appctl" if req.args.trim_start().starts_with("kill ") => {
                let target = req
                    .args
                    .trim_start()
                    .strip_prefix("kill ")
                    .unwrap_or("")
                    .trim();
                return ValidationDecision::Confirm {
                    reason: format!("Force-kill '{target}'? This may cause data loss."),
                };
            }
            _ => {}
        }

        // Handler-declared risk. The handler owns "which of my invocations is
        // risky?" via `assess_risk`; the engine just turns the verdict into a
        // decision, using the handler's custom message when one was supplied.
        match req.risk.level {
            RiskLevel::Low => ValidationDecision::Execute,
            RiskLevel::Medium | RiskLevel::High => {
                ValidationDecision::Confirm {
                    reason: req.risk.reason.clone().unwrap_or_else(|| {
                        format!("Action '{}' requires confirmation", req.action_id)
                    }),
                }
            }
        }
    }
}

impl Default for RulesEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// THE one mapping from a consent kind to its privacy-config flag. Used by
/// `RulesEngine::decide` (the gate) AND by the executor when stamping
/// `consent_feature` on a pending confirmation — a second copy of this match
/// is exactly how the EXEC-3 alias drift happened. LargeTransfer has no flag:
/// it is consented per run, every run.
pub fn consent_granted(kind: ConsentKind, privacy: &PrivacyConfig) -> bool {
    match kind {
        ConsentKind::IpGeolocation => privacy.allow_ip_geolocation,
        ConsentKind::PublicIp => privacy.allow_public_ip,
        ConsentKind::LargeTransfer => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn privacy() -> PrivacyConfig {
        PrivacyConfig::default()
    }

    fn privacy_all_allowed() -> PrivacyConfig {
        PrivacyConfig {
            allow_ip_geolocation: true,
            allow_public_ip: true,
            ..PrivacyConfig::default()
        }
    }

    // Low-risk assessment (what most handlers return) — a shared constant so the
    // request helper can borrow it.
    const LOW: RiskAssessment = RiskAssessment {
        level: RiskLevel::Low,
        reason: None,
        consent: None,
    };

    fn req<'a>(action_id: &'a str, args: &'a str) -> ValidationRequest<'a> {
        // These tests exercise the engine's *cross-cutting* policy (shell rules,
        // force-kill, speedtest consent), which is independent of the handler's
        // risk. Handler-declared risk (system destructive, service/package
        // mutation) is now tested in those handlers' own modules.
        ValidationRequest {
            action_id,
            args,
            routed_by: "explicit",
            risk: &LOW,
        }
    }

    #[test]
    fn low_risk_handlers_auto_execute() {
        let engine = RulesEngine::new();
        let p = privacy();
        assert_eq!(
            engine.validate(&req("open", "firefox"), &p),
            ValidationDecision::Execute
        );
        assert_eq!(
            engine.validate(&req("web", "rust"), &p),
            ValidationDecision::Execute
        );
        assert_eq!(
            engine.validate(&req("calc", "2+2"), &p),
            ValidationDecision::Execute
        );
        assert_eq!(
            engine.validate(&req("yt", "lofi"), &p),
            ValidationDecision::Execute
        );
        assert_eq!(
            engine.validate(&req("file", "~/Downloads"), &p),
            ValidationDecision::Execute
        );
        assert_eq!(
            engine.validate(&req("url", "github.com"), &p),
            ValidationDecision::Execute
        );
    }

    #[test]
    fn handler_declared_risk_drives_confirmation() {
        // The engine turns a handler's risk verdict into a decision, using the
        // handler's custom message when supplied. (The per-handler logic for WHICH
        // invocations are risky lives in the handlers' own tests now.)
        let engine = RulesEngine::new();
        let p = privacy();

        let confirm = RiskAssessment::confirm("Are you sure?");
        let decision = engine.validate(
            &ValidationRequest {
                action_id: "anything",
                args: "",
                routed_by: "explicit",
                risk: &confirm,
            },
            &p,
        );
        assert_eq!(
            decision,
            ValidationDecision::Confirm {
                reason: "Are you sure?".to_string()
            }
        );

        // A Low verdict auto-executes.
        assert_eq!(
            engine.validate(&req("anything", ""), &p),
            ValidationDecision::Execute
        );
    }

    #[test]
    fn safe_shell_commands_execute() {
        let engine = RulesEngine::new();
        let p = privacy();
        assert_eq!(
            engine.validate(&req("run", "ls -la"), &p),
            ValidationDecision::Execute
        );
        assert_eq!(
            engine.validate(&req("run", "code ."), &p),
            ValidationDecision::Execute
        );
        assert_eq!(
            engine.validate(&req("run", "cat file.txt"), &p),
            ValidationDecision::Execute
        );
    }

    #[test]
    fn dangerous_shell_commands_confirm() {
        let engine = RulesEngine::new();
        let p = privacy();
        let result = engine.validate(&req("run", "rm -rf /tmp/foo"), &p);
        assert!(matches!(result, ValidationDecision::Confirm { .. }));

        let result = engine.validate(&req("run", "sudo apt update"), &p);
        assert!(matches!(result, ValidationDecision::Confirm { .. }));
    }

    #[test]
    fn denylist_blocks() {
        let engine = RulesEngine::new();
        let p = privacy();
        let result = engine.validate(&req("run", ":(){ :|:& };:"), &p);
        assert!(matches!(result, ValidationDecision::Deny { .. }));

        let result = engine.validate(&req("run", "rm -rf /"), &p);
        assert!(matches!(result, ValidationDecision::Deny { .. }));
    }

    #[test]
    fn moderate_shell_commands_confirm() {
        let engine = RulesEngine::new();
        let p = privacy();
        let result = engine.validate(&req("run", "mkdir new-dir"), &p);
        assert!(matches!(result, ValidationDecision::Confirm { .. }));

        let result = engine.validate(&req("run", "cargo init my-project"), &p);
        assert!(matches!(result, ValidationDecision::Confirm { .. }));
    }

    /// Build a request the way the executor does: through the REAL handler's
    /// `assess_risk`. This is the drift test — the gate is exercised against
    /// whatever the handler actually declares, so an alias added to dispatch
    /// without a consent declaration fails here, not in the field.
    fn validate_real(action_id: &str, args: &str, p: &PrivacyConfig) -> ValidationDecision {
        use crate::action_registry::{ActionHandler, RiskContext};
        let risk = match action_id {
            "sysinfo" => crate::action_registry::handlers::sysinfo::SysInfoHandler
                .assess_risk(args, &RiskContext::default()),
            "weather" => crate::action_registry::handlers::weather::WeatherHandler::new(
                "celsius".into(),
                String::new(),
            )
            .assess_risk(args, &RiskContext::default()),
            _ => panic!("unknown handler {action_id}"),
        };
        RulesEngine::new().validate(
            &ValidationRequest {
                action_id,
                args,
                routed_by: "explicit",
                risk: &risk,
            },
            p,
        )
    }

    #[test]
    fn privacy_weather_requires_consent() {
        // Every spelling execute treats as "locate me" needs consent — the old
        // engine-side list knew ""/"here" while execute normalized more
        // qualifiers, so `weather now` geolocated without asking.
        for args in ["", "here", "Here", "now"] {
            let result = validate_real("weather", args, &privacy());
            assert!(
                matches!(result, ValidationDecision::Confirm { .. }),
                "weather {args:?} must ask before geolocating"
            );
        }

        // With consent granted, it passes through to default risk (Low → Execute)
        for args in ["", "here"] {
            let result = validate_real("weather", args, &privacy_all_allowed());
            assert_eq!(result, ValidationDecision::Execute);
        }

        // Weather with explicit location is always fine
        let result = validate_real("weather", "London", &privacy());
        assert_eq!(result, ValidationDecision::Execute);
    }

    #[test]
    fn privacy_sysinfo_net_requires_consent() {
        // BOTH dispatch aliases of the public-IP arm — "network" ran
        // unconsented when the engine kept its own list.
        for args in ["net", "network"] {
            let result = validate_real("sysinfo", args, &privacy());
            assert!(
                matches!(result, ValidationDecision::Confirm { .. }),
                "sysinfo {args:?} must ask before fetching the public IP"
            );
        }

        // With consent, it passes through
        let result = validate_real("sysinfo", "net", &privacy_all_allowed());
        assert_eq!(result, ValidationDecision::Execute);

        // "ip" prints LOCAL addresses only. The old gate prompted for it — a
        // false prompt that trained click-through.
        let result = validate_real("sysinfo", "ip", &privacy());
        assert_eq!(result, ValidationDecision::Execute);

        // Other sysinfo subcommands are fine regardless
        let result = validate_real("sysinfo", "cpu", &privacy());
        assert_eq!(result, ValidationDecision::Execute);
    }

    /// The invariant the executor's `consent_feature` stamp relies on: consent
    /// is checked FIRST in decide(), so when an assessment carries an
    /// UNGRANTED consent, the Confirm that comes back IS the consent prompt —
    /// even when the risk level would produce its own confirmation. Reordering
    /// decide() breaks the typed-consent wire field; this test is the tripwire.
    #[test]
    fn an_ungranted_consent_wins_over_a_risk_confirmation() {
        use crate::action_registry::{ConsentKind, RiskLevel};
        let risk = RiskAssessment::confirm("risky either way")
            .with_consent(ConsentKind::PublicIp, "consent prompt");
        assert_eq!(risk.level, RiskLevel::Medium);
        let decision = RulesEngine::new().validate(
            &ValidationRequest {
                action_id: "anything",
                args: "",
                routed_by: "explicit",
                risk: &risk,
            },
            &privacy(),
        );
        assert_eq!(
            decision,
            ValidationDecision::Confirm {
                reason: "consent prompt".to_string()
            }
        );
        // Once granted, the risk-level confirmation takes over.
        let decision = RulesEngine::new().validate(
            &ValidationRequest {
                action_id: "anything",
                args: "",
                routed_by: "explicit",
                risk: &risk,
            },
            &privacy_all_allowed(),
        );
        assert_eq!(
            decision,
            ValidationDecision::Confirm {
                reason: "risky either way".to_string()
            }
        );
    }

    #[test]
    fn speedtest_always_confirms() {
        // Both aliases, and no privacy flag exempts a bulk transfer.
        for args in ["speedtest", "speed"] {
            let result = validate_real("sysinfo", args, &privacy_all_allowed());
            assert!(
                matches!(result, ValidationDecision::Confirm { .. }),
                "sysinfo {args:?} must confirm every run"
            );
        }
    }
}
