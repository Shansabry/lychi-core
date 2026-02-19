pub mod shell;

use crate::action_registry::RiskLevel;
use crate::config::schema::PrivacyConfig;
use shell::ShellRules;

/// Pre-execution validation request.
pub struct ValidationRequest<'a> {
    pub action_id: &'a str,
    pub args: &'a str,
    pub routed_by: &'a str,
    pub default_risk: RiskLevel,
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
        // C6: Check privacy consent for network-sensitive commands first
        if let Some(decision) = self.check_privacy(req, privacy) {
            return decision;
        }

        match req.action_id {
            "run" => self.shell_rules.validate(req.args),
            "system" => {
                // System commands always require confirmation
                ValidationDecision::Confirm {
                    reason: format!("System action '{}' requires confirmation", req.args.trim()),
                }
            }
            // C6: speedtest uploads data to Cloudflare — require consent
            "sysinfo" if req.args.trim().eq_ignore_ascii_case("speedtest") => {
                ValidationDecision::Confirm {
                    reason: "Speed test will download 10 MB and upload 1 MB to Cloudflare"
                        .to_string(),
                }
            }
            _ => {
                // All other handlers: use their default risk
                match req.default_risk {
                    RiskLevel::Low => ValidationDecision::Execute,
                    RiskLevel::Medium => ValidationDecision::Confirm {
                        reason: format!("Action '{}' has medium risk", req.action_id),
                    },
                    RiskLevel::High => ValidationDecision::Confirm {
                        reason: format!("Action '{}' has high risk", req.action_id),
                    },
                }
            }
        }
    }

    /// C6: Check if a command requires privacy consent before executing.
    /// Returns Some(Confirm) if consent is needed, None if privacy is not relevant.
    fn check_privacy(
        &self,
        req: &ValidationRequest,
        privacy: &PrivacyConfig,
    ) -> Option<ValidationDecision> {
        match req.action_id {
            // weather with no args or "here" → needs IP geolocation
            "weather"
                if (req.args.trim().is_empty()
                    || req.args.trim().eq_ignore_ascii_case("here"))
                    && !privacy.allow_ip_geolocation =>
            {
                Some(ValidationDecision::Confirm {
                    reason: "Weather will detect your location by sending your IP to freeipapi.com. Allow and remember?".to_string(),
                })
            }
            // sysinfo net → public IP lookup via ifconfig.me
            "sysinfo"
                if matches!(req.args.trim().to_lowercase().as_str(), "net" | "ip")
                    && !privacy.allow_public_ip =>
            {
                Some(ValidationDecision::Confirm {
                    reason: "This will look up your public IP via ifconfig.me. Allow and remember?"
                        .to_string(),
                })
            }
            _ => None,
        }
    }
}

impl Default for RulesEngine {
    fn default() -> Self {
        Self::new()
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
        }
    }

    fn req<'a>(action_id: &'a str, args: &'a str) -> ValidationRequest<'a> {
        ValidationRequest {
            action_id,
            args,
            routed_by: "explicit",
            default_risk: RiskLevel::Low,
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
    fn system_always_confirms() {
        let engine = RulesEngine::new();
        let result = engine.validate(&req("system", "shutdown"), &privacy());
        assert!(matches!(result, ValidationDecision::Confirm { .. }));
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

    #[test]
    fn privacy_weather_requires_consent() {
        let engine = RulesEngine::new();
        // Without consent, weather with no args should require confirmation
        let result = engine.validate(&req("weather", ""), &privacy());
        assert!(matches!(result, ValidationDecision::Confirm { .. }));

        // "here" is an alias for auto-detect — also requires consent
        let result = engine.validate(&req("weather", "here"), &privacy());
        assert!(matches!(result, ValidationDecision::Confirm { .. }));
        let result = engine.validate(&req("weather", "Here"), &privacy());
        assert!(matches!(result, ValidationDecision::Confirm { .. }));

        // With consent, it should pass through to default risk (Low → Execute)
        let result = engine.validate(&req("weather", ""), &privacy_all_allowed());
        assert_eq!(result, ValidationDecision::Execute);
        let result = engine.validate(&req("weather", "here"), &privacy_all_allowed());
        assert_eq!(result, ValidationDecision::Execute);

        // Weather with explicit location is always fine
        let result = engine.validate(&req("weather", "London"), &privacy());
        assert_eq!(result, ValidationDecision::Execute);
    }

    #[test]
    fn privacy_sysinfo_net_requires_consent() {
        let engine = RulesEngine::new();
        // Without consent, sysinfo net should require confirmation
        let result = engine.validate(&req("sysinfo", "net"), &privacy());
        assert!(matches!(result, ValidationDecision::Confirm { .. }));

        let result = engine.validate(&req("sysinfo", "ip"), &privacy());
        assert!(matches!(result, ValidationDecision::Confirm { .. }));

        // With consent, it should pass through
        let result = engine.validate(&req("sysinfo", "net"), &privacy_all_allowed());
        assert_eq!(result, ValidationDecision::Execute);

        // Other sysinfo subcommands are fine regardless
        let result = engine.validate(&req("sysinfo", "cpu"), &privacy());
        assert_eq!(result, ValidationDecision::Execute);
    }

    #[test]
    fn speedtest_always_confirms() {
        let engine = RulesEngine::new();
        let result = engine.validate(&req("sysinfo", "speedtest"), &privacy_all_allowed());
        assert!(matches!(result, ValidationDecision::Confirm { .. }));
    }
}
