use super::ValidationDecision;

/// Hard-blocked patterns — NEVER execute, regardless of confirmation.
const DENYLIST: &[&str] = &[
    ":(){ ",      // fork bomb
    "rm -rf /\0", // sentinel — handled specially below
    "dd if=/dev/zero of=/dev/sd",
    "dd if=/dev/random of=/dev/sd",
    "mkfs.",     // format filesystem
    "> /dev/sd", // overwrite disk device
];

/// Dangerous shell patterns that require confirmation.
const DANGEROUS_PATTERNS: &[&str] = &[
    "rm ",
    "rm\t",
    "rmdir",
    "sudo ",
    "chmod ",
    "chown ",
    "dd ",
    "kill ",
    "pkill ",
    "shutdown",
    "reboot",
    "systemctl stop",
    "systemctl disable",
    "docker stop",
    "docker kill",
    "docker rm",
    " > ",
    " >> ",
    ">/",
    ">>/",
    "| xargs",
    "eval ",
    "exec ",
    "fork",
];

/// Moderate shell patterns (file creation/modification).
const MODERATE_PATTERNS: &[&str] = &[
    "mkdir",
    "touch ",
    "cp ",
    "mv ",
    "cargo init",
    "cargo new",
    "npm init",
    "git init",
    "wget ",
    "curl ",
    "pip install",
    "npm install",
    "apt ",
    "dnf ",
    "pacman ",
];

/// Shell-specific safety rules.
pub struct ShellRules;

impl ShellRules {
    pub fn new() -> Self {
        Self
    }

    /// Validate a shell command and return the appropriate decision.
    pub fn validate(&self, args: &str) -> ValidationDecision {
        let args_lower = args.to_lowercase();
        let trimmed = args_lower.trim();

        // Check denylist first — hard block
        if let Some(reason) = self.check_denylist(trimmed) {
            return ValidationDecision::Deny { reason };
        }

        // Check for dangerous patterns
        for pat in DANGEROUS_PATTERNS {
            if trimmed.contains(pat) {
                return ValidationDecision::Confirm {
                    reason: format!("Shell command matches dangerous pattern: {pat}"),
                };
            }
        }

        // Check for pipe or redirect operators
        if trimmed.contains('|') || trimmed.contains('>') {
            return ValidationDecision::Confirm {
                reason: "Shell command contains pipe or redirect operator".to_string(),
            };
        }

        // Check for moderate patterns
        for pat in MODERATE_PATTERNS {
            if trimmed.contains(pat) {
                return ValidationDecision::Confirm {
                    reason: format!("Shell command modifies filesystem: {pat}"),
                };
            }
        }

        // Safe command — auto-execute
        ValidationDecision::Execute
    }

    fn check_denylist(&self, cmd: &str) -> Option<String> {
        // Special case: "rm -rf /" (with nothing after or with space)
        // but NOT "rm -rf /tmp" (has more path)
        if cmd.starts_with("rm ")
            && cmd.contains("-rf")
            && (cmd.ends_with(" /") || cmd.contains(" / ") || cmd.ends_with(" /*"))
        {
            return Some("Blocked: rm -rf / is never allowed".to_string());
        }

        for pat in DENYLIST {
            // Skip the sentinel entry
            if pat.contains('\0') {
                continue;
            }
            if cmd.contains(pat) {
                return Some(format!("Blocked: command matches denylist pattern '{pat}'"));
            }
        }

        None
    }
}

impl Default for ShellRules {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_commands() {
        let rules = ShellRules::new();
        assert_eq!(rules.validate("ls -la"), ValidationDecision::Execute);
        assert_eq!(rules.validate("code ."), ValidationDecision::Execute);
        assert_eq!(rules.validate("cat file.txt"), ValidationDecision::Execute);
        assert_eq!(rules.validate("pwd"), ValidationDecision::Execute);
        assert_eq!(rules.validate("whoami"), ValidationDecision::Execute);
        assert_eq!(rules.validate("echo hello"), ValidationDecision::Execute);
    }

    #[test]
    fn dangerous_patterns() {
        let rules = ShellRules::new();
        assert!(matches!(
            rules.validate("rm -rf /tmp/foo"),
            ValidationDecision::Confirm { .. }
        ));
        assert!(matches!(
            rules.validate("sudo apt update"),
            ValidationDecision::Confirm { .. }
        ));
        assert!(matches!(
            rules.validate("chmod 777 file"),
            ValidationDecision::Confirm { .. }
        ));
        assert!(matches!(
            rules.validate("kill -9 1234"),
            ValidationDecision::Confirm { .. }
        ));
    }

    #[test]
    fn pipe_and_redirect_are_dangerous() {
        let rules = ShellRules::new();
        assert!(matches!(
            rules.validate("cat file | grep foo"),
            ValidationDecision::Confirm { .. }
        ));
        assert!(matches!(
            rules.validate("echo hello > output.txt"),
            ValidationDecision::Confirm { .. }
        ));
    }

    #[test]
    fn moderate_patterns() {
        let rules = ShellRules::new();
        assert!(matches!(
            rules.validate("mkdir new-dir"),
            ValidationDecision::Confirm { .. }
        ));
        assert!(matches!(
            rules.validate("cargo init my-project"),
            ValidationDecision::Confirm { .. }
        ));
        assert!(matches!(
            rules.validate("npm install express"),
            ValidationDecision::Confirm { .. }
        ));
        assert!(matches!(
            rules.validate("cp file1 file2"),
            ValidationDecision::Confirm { .. }
        ));
    }

    #[test]
    fn docker_lifecycle_confirms_but_reads_are_safe() {
        let rules = ShellRules::new();
        // Service-affecting docker verbs require confirmation.
        assert!(matches!(
            rules.validate("docker stop api-db"),
            ValidationDecision::Confirm { .. }
        ));
        assert!(matches!(
            rules.validate("docker kill api-db"),
            ValidationDecision::Confirm { .. }
        ));
        assert!(matches!(
            rules.validate("docker rm api-db"),
            ValidationDecision::Confirm { .. }
        ));
        // `exec` (interactive shell) also confirms — matches the `exec ` pattern.
        assert!(matches!(
            rules.validate("docker exec -it api-db sh"),
            ValidationDecision::Confirm { .. }
        ));
        // Read-only / reversible verbs auto-execute.
        assert!(matches!(
            rules.validate("docker logs api-db"),
            ValidationDecision::Execute
        ));
        assert!(matches!(
            rules.validate("docker restart api-db"),
            ValidationDecision::Execute
        ));
        assert!(matches!(
            rules.validate("docker ps"),
            ValidationDecision::Execute
        ));
    }

    #[test]
    fn denylist_fork_bomb() {
        let rules = ShellRules::new();
        assert!(matches!(
            rules.validate(":(){ :|:& };:"),
            ValidationDecision::Deny { .. }
        ));
    }

    #[test]
    fn denylist_rm_rf_root() {
        let rules = ShellRules::new();
        assert!(matches!(
            rules.validate("rm -rf /"),
            ValidationDecision::Deny { .. }
        ));
        assert!(matches!(
            rules.validate("rm -rf /*"),
            ValidationDecision::Deny { .. }
        ));
        // But rm -rf /tmp should be confirm, not deny
        assert!(matches!(
            rules.validate("rm -rf /tmp"),
            ValidationDecision::Confirm { .. }
        ));
    }

    #[test]
    fn denylist_disk_wipe() {
        let rules = ShellRules::new();
        assert!(matches!(
            rules.validate("dd if=/dev/zero of=/dev/sda"),
            ValidationDecision::Deny { .. }
        ));
    }

    #[test]
    fn denylist_mkfs() {
        let rules = ShellRules::new();
        assert!(matches!(
            rules.validate("mkfs.ext4 /dev/sda1"),
            ValidationDecision::Deny { .. }
        ));
    }
}
