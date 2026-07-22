//! Central destructive-verb classifier — one audit surface for "which verbs
//! mutate the system / are destructive?". Companion to the shell/uri/path
//! deciders.
//!
//! Design: the handlers (system, services, packages) still own their own
//! argument *parsing* — each has a different shape (`systemctl <verb> <unit>`,
//! `pkg install <name>`, `system <action>`). What they SHOULD NOT each own is
//! the *classification lists*; those live here so there is a single place to
//! audit and extend "what counts as destructive." Each handler extracts its verb
//! and asks this module.

/// Power/session actions that are destructive or irreversible and must confirm.
/// Matched as a prefix of the action word (`shutdown`, `reboot`, `hibernate`,
/// `logout`, and spellings like `shutdown-now`).
pub const DESTRUCTIVE_SYSTEM_ACTIONS: &[&str] = &["shutdown", "reboot", "hibernate", "logout"];

/// systemctl verbs that change service state (vs read-only status/list).
pub const MUTATING_SERVICE_VERBS: &[&str] = &[
    "start", "stop", "restart", "reload", "enable", "disable", "kill",
];

/// Read-only systemctl verbs that never need confirmation.
pub const READONLY_SERVICE_VERBS: &[&str] = &["status", "show", "is-active", "is-enabled"];

/// Package-manager verbs that mutate the system (need confirmation). The packages
/// handler dispatches these; `search` is read-only and absent.
pub const MUTATING_PACKAGE_VERBS: &[&str] = &["install", "remove", "uninstall", "upgrade"];

/// Is this power/session action destructive (needs confirmation)? Takes the
/// already-lowercased, trimmed action word; matches by prefix.
pub fn is_destructive_system_action(action: &str) -> bool {
    let action = action.trim();
    DESTRUCTIVE_SYSTEM_ACTIONS
        .iter()
        .any(|d| action.starts_with(d))
}

/// Does this systemctl verb mutate service state? Case-insensitive exact match.
pub fn is_mutating_service_verb(verb: &str) -> bool {
    let v = verb.trim().to_ascii_lowercase();
    MUTATING_SERVICE_VERBS.contains(&v.as_str())
}

/// Does this package-manager verb mutate the system? Case-insensitive exact match.
pub fn is_mutating_package_verb(verb: &str) -> bool {
    let v = verb.trim().to_ascii_lowercase();
    MUTATING_PACKAGE_VERBS.contains(&v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_actions() {
        assert!(is_destructive_system_action("shutdown"));
        assert!(is_destructive_system_action("reboot now"));
        assert!(is_destructive_system_action("logout"));
        assert!(!is_destructive_system_action("mute"));
        assert!(!is_destructive_system_action("brightness up"));
    }

    #[test]
    fn service_verbs() {
        assert!(is_mutating_service_verb("stop"));
        assert!(is_mutating_service_verb("ENABLE"));
        assert!(!is_mutating_service_verb("status"));
        assert!(!is_mutating_service_verb("is-active"));
    }

    #[test]
    fn package_verbs() {
        assert!(is_mutating_package_verb("install"));
        assert!(is_mutating_package_verb("Remove"));
        assert!(is_mutating_package_verb("uninstall"));
        assert!(is_mutating_package_verb("upgrade"));
        assert!(!is_mutating_package_verb("search"));
        assert!(!is_mutating_package_verb("info"));
    }
}
