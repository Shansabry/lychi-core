//! What desktop we are on, and what it can actually do.
//!
//! One module, because this was previously answered in 25 places that disagreed.
//! The disagreements were not cosmetic — see `session_desktop_is_not_a_desktop_name`
//! for the one that silently cost KDE users all window context.
//!
//! Two questions live here and they are deliberately separate:
//!
//! **Identity** ("which desktop is this?") is only for quirk workarounds — the
//! Mutter fullscreen backdrop, the KWin focus revoke. A name cannot tell you
//! whether a feature exists, only which bug to expect.
//!
//! **Capability** ("can I take a screenshot?") is a probe. Every capability this
//! app cares about is directly probeable, and probes are strictly better than
//! names: GNOME's GlobalShortcuts portal version varies by release, so no name
//! check can answer whether it will work. Probing also survives distros that
//! spell the session differently, which name matching does not.

use std::sync::OnceLock;

/// A desktop environment identifier, per the freedesktop
/// [OnlyShowIn registry](http://specifications.freedesktop.org/menu/latest/onlyshowin-registry.html).
///
/// `GnomeClassic` and `GnomeFlashback` are registered as identities distinct
/// from `Gnome`, which is why sessions ship `XDG_CURRENT_DESKTOP=
/// "GNOME-Flashback:GNOME"`: the specific identity first, the generic fallback
/// second. Collapsing them loses the distinction that makes the chain useful —
/// both are X11-only sessions where GNOME-Wayland workarounds must not apply.
///
/// Note there is no `Plasma`: it is not a registered name and no desktop ever
/// sets it. KDE Plasma sets `XDG_CURRENT_DESKTOP=KDE` (verified: the shipped
/// `plasma.desktop` session declares `DesktopNames=KDE`). Code matching
/// "PLASMA" was dead, and worse, looked like a considered fallback.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Desktop {
    Kde,
    Gnome,
    GnomeClassic,
    GnomeFlashback,
    Xfce,
    Cinnamon,
    Mate,
    Lxqt,
    Budgie,
    Pantheon,
    Unity,
    Cosmic,
    Sway,
    Hyprland,
    Phosh,
    Dde,
    /// A name we do not have a variant for. Kept rather than dropped so
    /// diagnostics can report what was actually there.
    Other,
}

impl Desktop {
    /// Match one component of `XDG_CURRENT_DESKTOP`.
    ///
    /// ASCII-case-insensitive despite the registry saying names are
    /// case-sensitive ("'KDE' not 'kde'"): being strict here would only make us
    /// fail on a misconfigured session, and there is no name whose meaning
    /// depends on its case.
    fn from_component(s: &str) -> Self {
        // `X-` marks a name outside the registry; Cinnamon ships `X-Cinnamon`
        // and is the reason a strict registry lookup is not enough on its own.
        let s = s
            .strip_prefix("X-")
            .or_else(|| s.strip_prefix("x-"))
            .unwrap_or(s);
        match s.to_ascii_uppercase().as_str() {
            "KDE" => Self::Kde,
            "GNOME" => Self::Gnome,
            "GNOME-CLASSIC" => Self::GnomeClassic,
            "GNOME-FLASHBACK" => Self::GnomeFlashback,
            "XFCE" => Self::Xfce,
            "CINNAMON" => Self::Cinnamon,
            "MATE" => Self::Mate,
            "LXQT" => Self::Lxqt,
            "BUDGIE" => Self::Budgie,
            "PANTHEON" => Self::Pantheon,
            "UNITY" => Self::Unity,
            "COSMIC" => Self::Cosmic,
            "SWAY" => Self::Sway,
            "HYPRLAND" => Self::Hyprland,
            "PHOSH" => Self::Phosh,
            "DDE" => Self::Dde,
            _ => Self::Other,
        }
    }

    /// Whether this desktop is GNOME or one of its registered variants.
    ///
    /// Useful because Mutter's quirks apply to all of them — but note the
    /// variants are X11 sessions, so a caller wanting "Mutter on Wayland" must
    /// check the session type too rather than relying on this alone.
    pub fn is_gnome_family(self) -> bool {
        matches!(
            self,
            Self::Gnome | Self::GnomeClassic | Self::GnomeFlashback
        )
    }
}

/// How the session talks to the display server.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionType {
    Wayland,
    X11,
}

/// The session's identity, parsed once.
#[derive(Clone, Debug)]
pub struct SessionInfo {
    /// Every component of `XDG_CURRENT_DESKTOP`, in order. The first is what
    /// the session IS; later ones are what it is compatible WITH.
    /// `"Budgie:GNOME"` means "I am Budgie, treat me as GNOME if you must".
    pub desktops: Vec<Desktop>,
    pub session_type: SessionType,
    /// The environment as it actually was, for diagnostics. Verbatim and
    /// unparsed: a bug report needs to show what we received, not what we made
    /// of it.
    pub raw_current_desktop: String,
    pub raw_session_desktop: String,
    pub raw_session_type: String,
    pub raw_wayland_display: String,
}

impl SessionInfo {
    /// Does any component name this desktop?
    pub fn is(&self, d: Desktop) -> bool {
        self.desktops.contains(&d)
    }

    /// The desktop this session primarily IS — the first component.
    pub fn primary(&self) -> Desktop {
        self.desktops.first().copied().unwrap_or(Desktop::Other)
    }

    pub fn is_wayland(&self) -> bool {
        self.session_type == SessionType::Wayland
    }

    fn detect() -> Self {
        let raw_current_desktop = env("XDG_CURRENT_DESKTOP");
        let raw_session_desktop = env("XDG_SESSION_DESKTOP");
        let raw_session_type = env("XDG_SESSION_TYPE");
        let raw_wayland_display = env("WAYLAND_DISPLAY");

        // XDG_CURRENT_DESKTOP is the only spec'd source, and the only one whose
        // values are registry names. XDG_SESSION_DESKTOP is systemd's *session
        // file name* — a different namespace entirely: this machine ships
        // `plasma.desktop` declaring `DesktopNames=KDE`, so the two read
        // "plasma" and "KDE" for the same session. Consulting it first (which
        // `Compositor` did) means a distro that follows the filename convention
        // reports a desktop nobody has a name for.
        let desktops: Vec<Desktop> = raw_current_desktop
            .split(':')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(Desktop::from_component)
            .collect();

        Self {
            desktops,
            session_type: detect_session_type(&raw_session_type, &raw_wayland_display),
            raw_current_desktop,
            raw_session_desktop,
            raw_session_type,
            raw_wayland_display,
        }
    }
}

/// Session type from the two signals that carry it.
///
/// `WAYLAND_DISPLAY` is checked as well as `XDG_SESSION_TYPE` because the
/// latter is **frequently absent under autostart** — the D-Bus activation
/// environment a session launches us in need not carry it. Treating its absence
/// as "X11" previously routed a KDE Wayland session to a window strategy that
/// cannot receive keyboard focus.
///
/// A non-empty socket name is decisive: you cannot have one without a
/// compositor to connect to.
fn detect_session_type(session_type: &str, wayland_display: &str) -> SessionType {
    if session_type.eq_ignore_ascii_case("wayland") || !wayland_display.is_empty() {
        SessionType::Wayland
    } else {
        SessionType::X11
    }
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

/// The session, detected once.
///
/// Cached forever: these are environment variables, which do not change for the
/// life of a process. Capability probes are cached separately and differently —
/// see `capabilities`.
pub fn session() -> &'static SessionInfo {
    static SESSION: OnceLock<SessionInfo> = OnceLock::new();
    SESSION.get_or_init(SessionInfo::detect)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(current: &str) -> Vec<Desktop> {
        current
            .split(':')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(Desktop::from_component)
            .collect()
    }

    /// The spec's rule: colon-separated, considered IN ORDER, exact per
    /// component. The order is the whole point — it is a fallback chain from
    /// the specific identity to the generic one.
    #[test]
    fn the_chain_keeps_its_order() {
        assert_eq!(
            parse("GNOME-Flashback:GNOME"),
            vec![Desktop::GnomeFlashback, Desktop::Gnome]
        );
        assert_eq!(parse("Budgie:GNOME"), vec![Desktop::Budgie, Desktop::Gnome]);
        assert_eq!(
            parse("ubuntu:GNOME"),
            vec![Desktop::Other, Desktop::Gnome],
            "an unregistered vendor prefix must not swallow the real name"
        );
    }

    /// `X-Cinnamon` is what Cinnamon actually ships. A strict registry lookup
    /// rejects it; stripping the `X-` marker is what makes it match.
    #[test]
    fn x_prefixed_names_are_recognised() {
        assert_eq!(parse("X-Cinnamon"), vec![Desktop::Cinnamon]);
    }

    /// GNOME's registered variants are DISTINCT identities, not aliases. Both
    /// are X11-only sessions, so collapsing them into `Gnome` would apply
    /// Mutter-on-Wayland workarounds to sessions that are neither.
    #[test]
    fn gnome_variants_are_distinct_but_still_gnome_family() {
        assert_ne!(Desktop::GnomeFlashback, Desktop::Gnome);
        assert!(Desktop::GnomeFlashback.is_gnome_family());
        assert!(Desktop::GnomeClassic.is_gnome_family());
        assert!(Desktop::Gnome.is_gnome_family());
        assert!(!Desktop::Kde.is_gnome_family());
    }

    /// **The bug this module exists for.**
    ///
    /// `XDG_SESSION_DESKTOP` holds the session FILE NAME, not a desktop name.
    /// This machine ships `plasma.desktop` declaring `DesktopNames=KDE`, so the
    /// two variables read "plasma" and "KDE" for one session. The old
    /// `Compositor` consulted the session-file name first and asked whether it
    /// contained "KDE" — false for "plasma" — landing on `OtherWayland`, which
    /// dispatches window detection to the wlr-foreign-toplevel backend that
    /// KWin does not implement. Silent, total loss of window context on KDE,
    /// invisible on any machine where both variables happen to read "KDE".
    #[test]
    fn session_desktop_is_not_a_desktop_name() {
        for session_file_name in ["plasma", "plasmawayland", "plasma6", "plasmax11"] {
            assert_eq!(
                parse(session_file_name),
                vec![Desktop::Other],
                "{session_file_name} is a session file name, not a desktop name — \
                 it must not be parsed as one"
            );
        }
        // The spec'd variable, for the same session, is what actually names it.
        assert_eq!(parse("KDE"), vec![Desktop::Kde]);
    }

    /// "PLASMA" is not a registered desktop name and nothing sets it. Code
    /// matching it was dead, and read as a deliberate fallback.
    #[test]
    fn plasma_is_not_a_desktop_name() {
        assert_eq!(parse("PLASMA"), vec![Desktop::Other]);
    }

    #[test]
    fn matching_is_case_insensitive_and_trims() {
        assert_eq!(parse("kde"), vec![Desktop::Kde]);
        assert_eq!(parse(" KDE : GNOME "), vec![Desktop::Kde, Desktop::Gnome]);
    }

    #[test]
    fn an_unset_desktop_yields_no_names() {
        assert!(parse("").is_empty());
    }

    /// Autostart: `XDG_SESSION_TYPE` is routinely absent, and treating that as
    /// X11 previously chose a window strategy that cannot take keyboard focus.
    #[test]
    fn a_wayland_socket_alone_means_wayland() {
        assert_eq!(
            detect_session_type("", "wayland-0"),
            SessionType::Wayland,
            "XDG_SESSION_TYPE is often unset under autostart"
        );
        assert_eq!(detect_session_type("wayland", ""), SessionType::Wayland);
        assert_eq!(detect_session_type("x11", ""), SessionType::X11);
        assert_eq!(detect_session_type("", ""), SessionType::X11);
    }

    /// An empty `WAYLAND_DISPLAY` is not a session — some environments export
    /// the name with no value.
    #[test]
    fn an_empty_socket_name_is_not_a_wayland_session() {
        assert_eq!(detect_session_type("", ""), SessionType::X11);
        assert_eq!(detect_session_type("x11", ""), SessionType::X11);
    }

    /// A live socket beats a stale label: nested compositors and
    /// XWayland-adjacent setups can leave `XDG_SESSION_TYPE=x11` set.
    #[test]
    fn a_live_socket_beats_a_stale_label() {
        assert_eq!(
            detect_session_type("x11", "wayland-1"),
            SessionType::Wayland
        );
    }
}
