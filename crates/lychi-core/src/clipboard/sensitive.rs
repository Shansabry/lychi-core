//! Deciding whether a clipboard copy must never be recorded.
//!
//! A clipboard history is a plaintext log of everything the user copies, which
//! on a normal day includes passwords, 2FA codes, API keys and recovery
//! phrases. C6 makes this the launcher's problem, not the user's: the safe
//! default is to skip, and the user opts out.
//!
//! Two independent signals, checked in this order:
//!
//! 1. **The sensitivity hint the source app sets.** Password managers mark the
//!    selection as secret so clipboard managers will skip it. This is the
//!    signal that actually works, because the app that knows the text is a
//!    password is the one that put it there.
//! 2. **A user-configured app exclusion.** For everything that sets no hint.
//!
//! ## The hint, concretely
//!
//! The de-facto standard on Linux is an extra MIME type named
//! `x-kde-passwordManagerHint` offered on the same selection as `text/plain`.
//! KDE originated it, `wl-clipboard` sets it for `wl-copy --sensitive`, and
//! CopyQ, Klipper and others honour it. Verified against a real selection on
//! this machine: `wl-copy --sensitive` yields
//!
//! ```text
//! text/plain
//! text/plain;charset=utf-8
//! x-kde-passwordManagerHint
//! ```
//!
//! and the text stays readable — so a manager that does not look at the type
//! list captures the password without noticing anything unusual. That is
//! precisely the bug this module closes.
//!
//! It is a *cooperative* scheme, not an enforced one: an app that sets no hint
//! gets no protection, which is why the app exclusion list exists alongside it.
//!
//! ## Why enumerate types instead of reading them
//!
//! We ask only for the *list* of offered types, never the hint's value. The
//! list is enough to decide, and requesting the payload of a type a password
//! manager marked secret is the opposite of the intent. On the two clipboard
//! stacks Lychi already shells out to:
//!
//! - Wayland: `wl-paste --list-types`
//! - X11 / XWayland: `xclip -selection clipboard -t TARGETS -o`
//!
//! Both are already dependencies of the clipboard and selection paths, so this
//! adds no new tool requirement.

use std::process::Command;

/// The MIME type password managers attach to a secret selection.
///
/// Compared case-insensitively: this travels as an X11 atom / Wayland MIME
/// string set by many different apps, and the casing is convention, not spec.
const SENSITIVE_HINT: &str = "x-kde-passwordManagerHint";

/// Why a copy was not recorded. Carried so the caller can log the reason —
/// clipboard history silently missing an entry is otherwise indistinguishable
/// from a bug, and "why didn't my copy show up" is unanswerable without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The source app marked the selection as a secret.
    SensitiveHint,
    /// The focused app is on the user's exclusion list.
    ExcludedApp(String),
}

impl SkipReason {
    pub fn as_log_str(&self) -> String {
        match self {
            Self::SensitiveHint => "source app marked it sensitive".to_string(),
            Self::ExcludedApp(app) => format!("app '{app}' is excluded"),
        }
    }
}

/// Does this list of offered MIME types carry the password-manager hint?
///
/// Pure, so the rule is testable without a compositor. Callers pass whatever
/// the platform enumerated.
pub fn types_are_sensitive<S: AsRef<str>>(types: &[S]) -> bool {
    types
        .iter()
        .any(|t| t.as_ref().trim().eq_ignore_ascii_case(SENSITIVE_HINT))
}

/// Is `wm_class` excluded by this list?
///
/// Both sides are normalised the same way the rest of the context layer
/// normalises window classes, so a config entry of `KeePassXC`, `keepassxc` or
/// `org.keepassxc.KeePassXC` all match the same window.
pub fn app_is_excluded(wm_class: &str, excluded: &[String]) -> bool {
    if wm_class.is_empty() {
        return false;
    }
    let target = crate::context::active_window::normalize_wm_class(wm_class);
    if target.is_empty() {
        return false;
    }
    excluded
        .iter()
        .map(|e| crate::context::active_window::normalize_wm_class(e))
        .any(|e| !e.is_empty() && e == target)
}

/// Enumerate the MIME types currently offered on the CLIPBOARD selection.
///
/// Preferred tool first, the other as a fallback — XWayland apps expose their
/// selection over X11 even on Wayland, so trying both is what makes the hint
/// visible for Qt/Electron password managers running under XWayland.
///
/// Returns `None` when neither tool could answer. That is deliberately distinct
/// from `Some(vec![])`: "we could not tell" must not be read as "not
/// sensitive", and [`should_skip`] fails closed on it.
pub fn offered_types(is_wayland: bool) -> Option<Vec<String>> {
    let attempts: [&str; 2] = if is_wayland {
        ["wl-paste", "xclip"]
    } else {
        ["xclip", "wl-paste"]
    };
    for tool in attempts {
        if let Some(types) = list_types_with(tool) {
            return Some(types);
        }
    }
    None
}

fn list_types_with(tool: &str) -> Option<Vec<String>> {
    let output = match tool {
        "wl-paste" => Command::new("wl-paste").arg("--list-types").output().ok()?,
        "xclip" => Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "TARGETS", "-o"])
            .output()
            .ok()?,
        _ => return None,
    };
    if !output.status.success() {
        return None;
    }
    let types: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if types.is_empty() {
        // An empty clipboard and a tool that failed to talk to the display are
        // indistinguishable here; let the other tool try.
        return None;
    }
    Some(types)
}

/// The one decider: should this copy be kept out of history?
///
/// `offered` is the enumerated MIME list (`None` = could not enumerate) and
/// `wm_class` is the window that owned focus when the copy happened (empty when
/// unknown).
///
/// Fails **closed** on an un-enumerable clipboard *only* when the hint check is
/// enabled — if we cannot see whether the app marked it secret, recording it
/// anyway is the exact failure this exists to prevent, and the cost of the
/// alternative is one missing history row.
pub fn should_skip(
    cfg: &crate::config::schema::ClipboardPrivacyConfig,
    offered: Option<&[String]>,
    wm_class: &str,
) -> Option<SkipReason> {
    if cfg.respect_sensitive_hint {
        match offered {
            Some(types) if types_are_sensitive(types) => return Some(SkipReason::SensitiveHint),
            None => return Some(SkipReason::SensitiveHint),
            Some(_) => {}
        }
    }
    if app_is_excluded(wm_class, &cfg.excluded_apps) {
        return Some(SkipReason::ExcludedApp(wm_class.to_string()));
    }
    None
}

/// The policy the clipboard monitor is currently enforcing.
///
/// The monitor runs on a plain OS thread, so it cannot `await` the async
/// `RwLock<Config>` that owns the real setting. Rather than hand the thread its
/// own copy at spawn — which would silently ignore every later settings change,
/// leaving a user who just switched the toggle on still being recorded — the
/// config write path publishes here and the monitor reads on each poll.
///
/// This is a cache of one decider's output, not a second decider: nothing
/// writes it except [`publish_policy`], called from the one place config is
/// saved.
static POLICY: std::sync::RwLock<Option<crate::config::schema::ClipboardPrivacyConfig>> =
    std::sync::RwLock::new(None);

/// Publish the active clipboard privacy policy. Call after any config load or
/// save so the running monitor picks the change up on its next poll.
pub fn publish_policy(cfg: &crate::config::schema::ClipboardPrivacyConfig) {
    if let Ok(mut guard) = POLICY.write() {
        *guard = Some(cfg.clone());
    }
}

/// The policy the monitor should apply right now.
///
/// Falls back to [`Default`] — which protects — if nothing has been published
/// or the lock was poisoned. A poisoned lock must not silently downgrade to
/// recording passwords.
///
/// `respect_sensitive_hint` is forced **on** here, whatever the stored config
/// says. Honouring a password manager's explicit "this is a secret, don't
/// persist it" marker is a privacy contract, not a preference — there is no
/// legitimate reason to record a selection the source app flagged, so it is not
/// a user-facing toggle (the settings switch was removed). The struct field is
/// kept only so an older `config.toml` that still carries
/// `respect_sensitive_hint = false` deserializes without error; its value is
/// deliberately ignored. `excluded_apps` remains a real, user-controlled
/// preference and is passed through untouched.
pub fn current_policy() -> crate::config::schema::ClipboardPrivacyConfig {
    let mut policy = POLICY
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default();
    policy.respect_sensitive_hint = true;
    policy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::ClipboardPrivacyConfig;

    fn cfg(hint: bool, apps: &[&str]) -> ClipboardPrivacyConfig {
        ClipboardPrivacyConfig {
            respect_sensitive_hint: hint,
            excluded_apps: apps.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The exact type list `wl-copy --sensitive` produces, captured from a real
    /// selection on a KDE Wayland session.
    fn real_sensitive_types() -> Vec<String> {
        [
            "text/plain",
            "text/plain;charset=utf-8",
            "TEXT",
            "STRING",
            "UTF8_STRING",
            "x-kde-passwordManagerHint",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn ordinary_types() -> Vec<String> {
        ["text/plain", "text/plain;charset=utf-8", "UTF8_STRING"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn real_password_manager_selection_is_detected() {
        assert!(types_are_sensitive(&real_sensitive_types()));
    }

    #[test]
    fn ordinary_copy_is_not_sensitive() {
        assert!(!types_are_sensitive(&ordinary_types()));
    }

    #[test]
    fn hint_matching_ignores_case() {
        // The atom's casing is convention, not spec.
        assert!(types_are_sensitive(&["X-KDE-PASSWORDMANAGERHINT"]));
        assert!(types_are_sensitive(&["x-kde-passwordmanagerhint"]));
    }

    #[test]
    fn a_type_merely_containing_the_word_password_is_not_the_hint() {
        // Guards against a substring check sneaking in later.
        assert!(!types_are_sensitive(&[
            "application/x-my-password-export",
            "text/password"
        ]));
    }

    #[test]
    fn sensitive_copy_is_skipped() {
        let types = real_sensitive_types();
        assert_eq!(
            should_skip(&cfg(true, &[]), Some(&types), "keepassxc"),
            Some(SkipReason::SensitiveHint)
        );
    }

    #[test]
    fn ordinary_copy_is_recorded() {
        let types = ordinary_types();
        assert_eq!(should_skip(&cfg(true, &[]), Some(&types), "firefox"), None);
    }

    #[test]
    fn disabling_the_hint_check_records_a_sensitive_copy() {
        // The opt-out must actually opt out, or the setting is decoration.
        let types = real_sensitive_types();
        assert_eq!(
            should_skip(&cfg(false, &[]), Some(&types), "keepassxc"),
            None
        );
    }

    #[test]
    fn unenumerable_clipboard_fails_closed() {
        // Cannot see the hint => must assume it might be there.
        assert_eq!(
            should_skip(&cfg(true, &[]), None, "keepassxc"),
            Some(SkipReason::SensitiveHint)
        );
    }

    #[test]
    fn unenumerable_clipboard_is_recorded_when_the_check_is_off() {
        // Failing closed is a consequence of the hint check, not a second
        // independent rule — with the check off there is nothing to fail.
        assert_eq!(should_skip(&cfg(false, &[]), None, "firefox"), None);
    }

    #[test]
    fn excluded_app_is_skipped() {
        let types = ordinary_types();
        assert_eq!(
            should_skip(&cfg(true, &["keepassxc"]), Some(&types), "keepassxc"),
            Some(SkipReason::ExcludedApp("keepassxc".to_string()))
        );
    }

    #[test]
    fn exclusion_matching_survives_wm_class_spelling() {
        // The same app is spelled differently by X11, Wayland and .desktop
        // files; config written either way must match either way.
        assert!(app_is_excluded(
            "org.keepassxc.KeePassXC",
            &["keepassxc".to_string()]
        ));
        assert!(app_is_excluded("keepassxc", &["KeePassXC".to_string()]));
    }

    #[test]
    fn a_different_app_is_not_excluded() {
        assert!(!app_is_excluded("firefox", &["keepassxc".to_string()]));
    }

    #[test]
    fn unknown_window_is_not_excluded_by_an_empty_config_entry() {
        // An empty wm_class must not match a stray "" left in the config and
        // silently disable all clipboard capture.
        assert!(!app_is_excluded("", &[String::new()]));
        assert!(!app_is_excluded("firefox", &[String::new()]));
    }

    #[test]
    fn policy_before_anything_is_published_still_protects() {
        // The monitor thread can poll before startup publishes, and the lock
        // can be poisoned. Either way the fallback must protect, not record —
        // a fallback that returned a permissive policy would disable this
        // whole feature with no test failing anywhere.
        //
        // Reads the real static deliberately: `publish_policy` is only ever
        // called from the app layer, so in the test binary it is unset.
        let p = current_policy();
        assert!(p.respect_sensitive_hint);
        assert_eq!(
            should_skip(&p, None, ""),
            Some(SkipReason::SensitiveHint),
            "the unpublished fallback must fail closed"
        );
    }

    /// The sensitive-hint skip is enforced, not merely defaulted.
    ///
    /// A stored config that predates the removal of the settings toggle can
    /// still carry `respect_sensitive_hint = false`. That value must be ignored:
    /// `current_policy` forces the hint on regardless, so an old opt-out cannot
    /// re-enable recording of password-manager-flagged copies. Publishes into
    /// the shared static and restores it so no other test in this binary sees a
    /// permissive policy leak across.
    #[test]
    fn stored_opt_out_cannot_disable_the_hint() {
        let previous = POLICY.read().ok().and_then(|g| g.clone());
        publish_policy(&cfg(false, &["keepassxc"]));
        let effective = current_policy();
        assert!(
            effective.respect_sensitive_hint,
            "a stored respect_sensitive_hint=false must not disable the skip"
        );
        assert_eq!(
            effective.excluded_apps,
            vec!["keepassxc".to_string()],
            "the real preference (excluded_apps) must pass through untouched"
        );
        // Restore whatever was there (almost always None in the test binary).
        if let Ok(mut guard) = POLICY.write() {
            *guard = previous;
        }
    }

    #[test]
    fn default_config_protects_passwords_but_excludes_no_apps() {
        let d = ClipboardPrivacyConfig::default();
        assert!(d.respect_sensitive_hint, "C6: safe default is to skip");
        assert!(d.excluded_apps.is_empty(), "no guessed name table");
    }
}
