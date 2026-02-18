pub mod shell;

use crate::action_registry::RiskLevel;
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
    pub fn validate(&self, req: &ValidationRequest) -> ValidationDecision {
        match req.action_id {
            "run" => self.shell_rules.validate(req.args),
            "system" => {
                // System commands always require confirmation
                ValidationDecision::Confirm {
                    reason: format!("System action '{}' requires confirmation", req.args.trim()),
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
}

impl Default for RulesEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            engine.validate(&req("open", "firefox")),
            ValidationDecision::Execute
        );
        assert_eq!(
            engine.validate(&req("web", "rust")),
            ValidationDecision::Execute
        );
        assert_eq!(
            engine.validate(&req("calc", "2+2")),
            ValidationDecision::Execute
        );
        assert_eq!(
            engine.validate(&req("yt", "lofi")),
            ValidationDecision::Execute
        );
        assert_eq!(
            engine.validate(&req("file", "~/Downloads")),
            ValidationDecision::Execute
        );
        assert_eq!(
            engine.validate(&req("url", "github.com")),
            ValidationDecision::Execute
        );
    }

    #[test]
    fn system_always_confirms() {
        let engine = RulesEngine::new();
        let result = engine.validate(&req("system", "shutdown"));
        assert!(matches!(result, ValidationDecision::Confirm { .. }));
    }

    #[test]
    fn safe_shell_commands_execute() {
        let engine = RulesEngine::new();
        assert_eq!(
            engine.validate(&req("run", "ls -la")),
            ValidationDecision::Execute
        );
        assert_eq!(
            engine.validate(&req("run", "code .")),
            ValidationDecision::Execute
        );
        assert_eq!(
            engine.validate(&req("run", "cat file.txt")),
            ValidationDecision::Execute
        );
    }

    #[test]
    fn dangerous_shell_commands_confirm() {
        let engine = RulesEngine::new();
        let result = engine.validate(&req("run", "rm -rf /tmp/foo"));
        assert!(matches!(result, ValidationDecision::Confirm { .. }));

        let result = engine.validate(&req("run", "sudo apt update"));
        assert!(matches!(result, ValidationDecision::Confirm { .. }));
    }

    #[test]
    fn denylist_blocks() {
        let engine = RulesEngine::new();
        let result = engine.validate(&req("run", ":(){ :|:& };:"));
        assert!(matches!(result, ValidationDecision::Deny { .. }));

        let result = engine.validate(&req("run", "rm -rf /"));
        assert!(matches!(result, ValidationDecision::Deny { .. }));
    }

    #[test]
    fn moderate_shell_commands_confirm() {
        let engine = RulesEngine::new();
        let result = engine.validate(&req("run", "mkdir new-dir"));
        assert!(matches!(result, ValidationDecision::Confirm { .. }));

        let result = engine.validate(&req("run", "cargo init my-project"));
        assert!(matches!(result, ValidationDecision::Confirm { .. }));
    }
}
