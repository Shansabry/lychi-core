//! How this copy of Lychi was installed — and therefore who owns updating it.
//!
//! Lychi ships as an AppImage, a `.deb`, an `.rpm`, and (planned) Flatpak and
//! AUR packages. Only the AppImage may update itself: everywhere else a package
//! manager owns the installed files, and an in-app updater would either fail on
//! read-only paths or silently diverge from what `dnf`/`apt`/`flatpak` believes
//! is installed. Tauri's updater supports the AppImage bundle on Linux and not
//! the distro formats, so this is a real constraint rather than a policy choice.
//!
//! The detection is **runtime**, not a compile-time feature, because one built
//! binary is what gets repackaged into `.deb`/`.rpm` by the same CI run. A
//! `#[cfg]` flag would have to be right at bundle time for every target; an
//! environment probe is right at the only moment that matters.

/// Where this process was installed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// Running from an AppImage — self-contained and self-updatable.
    AppImage,
    /// Inside a Flatpak sandbox.
    Flatpak,
    /// A distro package (`.deb`/`.rpm`/AUR) or a plain build.
    ///
    /// These are indistinguishable at runtime and do not need to be told
    /// apart: the answer for all of them is the same — the system updates it.
    System,
}

impl InstallKind {
    /// Detect from the environment.
    pub fn detect() -> Self {
        Self::from_env(
            std::env::var("APPIMAGE").ok().as_deref(),
            std::path::Path::new("/.flatpak-info").exists(),
        )
    }

    /// The rule, as a pure function of its inputs, so every branch is testable
    /// on any machine (probing the build host answers only for the build host).
    ///
    /// `APPIMAGE` is set by the AppImage runtime to the path of the mounted
    /// image; `/.flatpak-info` exists only inside a Flatpak sandbox and is the
    /// canonical check. Flatpak wins when both are somehow present: a sandbox
    /// is the more restrictive environment, and guessing wrong toward
    /// "self-update" is the harmful direction.
    pub fn from_env(appimage_var: Option<&str>, has_flatpak_info: bool) -> Self {
        if has_flatpak_info {
            return InstallKind::Flatpak;
        }
        match appimage_var {
            Some(p) if !p.trim().is_empty() => InstallKind::AppImage,
            _ => InstallKind::System,
        }
    }

    /// May Lychi update *itself*?
    ///
    /// Only the AppImage. Everywhere else the answer is "your package manager
    /// does that", which the UI says rather than offering a button that cannot
    /// work.
    pub fn can_self_update(self) -> bool {
        matches!(self, InstallKind::AppImage)
    }

    /// What to tell the user about updates when [`Self::can_self_update`] is
    /// false. Phrased as the command they would actually run.
    pub fn update_hint(self) -> &'static str {
        match self {
            InstallKind::AppImage => "Lychi can update itself.",
            InstallKind::Flatpak => "Updates are managed by Flatpak — run `flatpak update`.",
            InstallKind::System => {
                "Updates are managed by your package manager (for example `dnf upgrade`, \
                 `apt upgrade`, or your AUR helper)."
            }
        }
    }

    /// Stable identifier for logs, `lychi doctor`, and the UI.
    pub fn as_str(self) -> &'static str {
        match self {
            InstallKind::AppImage => "appimage",
            InstallKind::Flatpak => "flatpak",
            InstallKind::System => "system",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appimage_is_detected_from_its_env_var() {
        let k = InstallKind::from_env(Some("/tmp/Lychi_0.1.0_amd64.AppImage"), false);
        assert_eq!(k, InstallKind::AppImage);
        assert!(k.can_self_update());
    }

    #[test]
    fn flatpak_is_detected_from_its_marker_file() {
        let k = InstallKind::from_env(None, true);
        assert_eq!(k, InstallKind::Flatpak);
        assert!(!k.can_self_update());
        assert!(k.update_hint().contains("flatpak update"));
    }

    /// A Flatpak that also exports APPIMAGE must NOT self-update: guessing
    /// wrong toward self-update is the harmful direction.
    #[test]
    fn flatpak_wins_when_both_signals_are_present() {
        assert_eq!(
            InstallKind::from_env(Some("/app/lychi.AppImage"), true),
            InstallKind::Flatpak
        );
    }

    #[test]
    fn a_distro_package_or_plain_build_is_system() {
        let k = InstallKind::from_env(None, false);
        assert_eq!(k, InstallKind::System);
        assert!(!k.can_self_update());
        assert!(k.update_hint().contains("package manager"));
    }

    /// An empty `APPIMAGE` is unset, per the usual env convention.
    #[test]
    fn an_empty_appimage_var_is_not_an_appimage() {
        assert_eq!(InstallKind::from_env(Some(""), false), InstallKind::System);
        assert_eq!(
            InstallKind::from_env(Some("   "), false),
            InstallKind::System
        );
    }

    #[test]
    fn only_the_appimage_may_self_update() {
        for k in [InstallKind::Flatpak, InstallKind::System] {
            assert!(!k.can_self_update(), "{k:?} must not self-update");
        }
        assert!(InstallKind::AppImage.can_self_update());
    }

    #[test]
    fn every_kind_has_a_distinct_identifier() {
        let ids: Vec<&str> = [
            InstallKind::AppImage,
            InstallKind::Flatpak,
            InstallKind::System,
        ]
        .iter()
        .map(|k| k.as_str())
        .collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }
}
