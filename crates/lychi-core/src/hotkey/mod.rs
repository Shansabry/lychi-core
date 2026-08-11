//! How confident are we that the global hotkey actually opens the launcher?
//!
//! This exists because the launcher used to answer that question by asking
//! whether *registration returned Ok*, which is not the same thing and is
//! wrong in the one case that stranded a real tester.
//!
//! ## Why "registered" is not "works"
//!
//! On X11 an `XGrabKey` cannot override a combination the window manager
//! already owns. The grab **succeeds** — the plugin returns `Ok(())`, the log
//! says "Global shortcut registered" — and the WM keeps delivering the key to
//! itself, so the launcher never opens. KDE-on-X11 with `Super+Space` bound to
//! KRunner is exactly this, and so was the XFCE report that prompted
//! `hotkey_de`.
//!
//! The old `reliable` flag was `registered && (!wayland || portal_bound)`. On
//! X11 `!wayland` is true, so it collapsed to `registered` — meaning it was
//! *always true on the only platform where the grab silently lies*. The warning
//! never appeared for the users who needed it.
//!
//! ## What actually distinguishes the cases
//!
//! Ownership, not registration:
//!
//! - **Portal binding** (Wayland, GNOME/KDE): the compositor routes the key to
//!   us. Authoritative.
//! - **A binding in the desktop's own settings** (`hotkey_de`): we are the
//!   registered owner, not a competing grabber. Authoritative.
//! - **A bare X11 grab**: works only if nothing else claimed the key, and we
//!   cannot tell from the return value whether that is so. **Unverified.**
//! - **Nothing** — no portal backend and no DE integration (Sway, COSMIC,
//!   river, labwc): there is no hotkey at all.
//!
//! Only a keypress that reaches us proves the third case, so
//! [`Confidence::Unverified`] is a question to ask the user, not a failure to
//! report. See [`Confidence`].

/// How the hotkey was bound, in descending order of how much it proves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    /// Bound through the XDG GlobalShortcuts portal — the compositor routes the
    /// key to us.
    Portal,
    /// Written into the desktop environment's own shortcut settings, so we are
    /// the owner rather than a competing grabber.
    DesktopSettings,
    /// An X11 key-grab returned Ok. Proves the call succeeded, nothing more.
    X11Grab,
    /// A portal binding existed and its session died (portal crash/restart —
    /// routine over a long session: package upgrades, OOM, `systemctl --user
    /// restart`). The key is dead RIGHT NOW while re-registration retries in
    /// the background. Distinct from [`Binding::None`] because the diagnosis
    /// and the user story differ: the portal works here, it just went away.
    PortalLost,
    /// The combination belongs to something else and we declined to steal it.
    Conflict,
    /// Nothing bound the key.
    None,
}

/// What we can honestly say about the hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Something owns the binding on our behalf. Say nothing to the user.
    Reliable,
    /// A grab succeeded but cannot be trusted; only a keypress settles it.
    /// The user should be *asked*, not warned.
    Unverified,
    /// There is no working hotkey. The user needs an alternative.
    Broken,
}

/// The reliability verdict, plus enough context to explain it.
///
/// One decider: computed once where the binding actually happens and stored,
/// rather than re-derived later from weaker inputs. The previous design threw
/// the `hotkey_de` outcome away after logging it, then guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyVerdict {
    pub binding: Binding,
    pub confidence: Confidence,
}

impl HotkeyVerdict {
    /// Decide from how the key was bound and whether this is a Wayland session.
    pub fn assess(binding: Binding, wayland: bool) -> Self {
        let confidence = match binding {
            Binding::Portal => Confidence::Reliable,
            Binding::DesktopSettings => Confidence::Reliable,
            // On Wayland the X11 plugin only sees XWayland windows, so a grab
            // there is not merely unproven — it is known not to work globally.
            Binding::X11Grab if wayland => Confidence::Broken,
            Binding::X11Grab => Confidence::Unverified,
            // Not Unverified: no keypress can arrive on a dead session, so
            // there is no question to ask the user — the key is broken until
            // re-registration succeeds and records a fresh Portal verdict.
            Binding::PortalLost => Confidence::Broken,
            Binding::Conflict => Confidence::Broken,
            Binding::None => Confidence::Broken,
        };
        Self {
            binding,
            confidence,
        }
    }

    /// Should the user be asked to press the key to confirm it works?
    ///
    /// Only when a press would tell us something we do not already know.
    pub fn needs_confirmation(&self) -> bool {
        self.confidence == Confidence::Unverified
    }

    /// A press reached us, so the binding is proven regardless of how it was made.
    pub fn confirmed(self) -> Self {
        Self {
            confidence: Confidence::Reliable,
            ..self
        }
    }

    /// One line explaining the verdict, for `lychi doctor` and the settings panel.
    pub fn explain(&self) -> &'static str {
        match (self.binding, self.confidence) {
            (_, Confidence::Reliable) => "the hotkey is bound and works",
            (Binding::X11Grab, Confidence::Unverified) => {
                "an X11 grab succeeded, but a window manager that already owns \
                 this key would silently keep it — press the hotkey to confirm"
            }
            (Binding::X11Grab, Confidence::Broken) => {
                "the X11 grab only covers XWayland windows on this Wayland session"
            }
            (Binding::PortalLost, _) => {
                "the desktop portal closed our shortcut session (it likely \
                 restarted) — rebinding in the background; restart Lychi if \
                 the hotkey stays dead"
            }
            (Binding::Conflict, _) => {
                "the combination is already bound to something else — pick another"
            }
            (Binding::None, _) => "nothing could bind the hotkey",
            _ => "the hotkey may not work",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_x11_grab_alone_is_never_called_reliable() {
        // This is the whole bug: the old flag was `registered && !wayland`, so
        // an X11 grab was reported reliable and the warning never showed for
        // the KDE/XFCE users who needed it.
        let v = HotkeyVerdict::assess(Binding::X11Grab, false);
        assert_ne!(v.confidence, Confidence::Reliable);
        assert!(v.needs_confirmation(), "only a keypress can settle this");
    }

    #[test]
    fn ownership_is_reliable_without_asking() {
        for b in [Binding::Portal, Binding::DesktopSettings] {
            let v = HotkeyVerdict::assess(b, false);
            assert_eq!(v.confidence, Confidence::Reliable, "{b:?}");
            assert!(!v.needs_confirmation(), "{b:?} needs no confirmation");
        }
    }

    #[test]
    fn a_portal_binding_is_reliable_on_wayland() {
        let v = HotkeyVerdict::assess(Binding::Portal, true);
        assert_eq!(v.confidence, Confidence::Reliable);
    }

    #[test]
    fn an_x11_grab_on_wayland_is_broken_not_merely_unverified() {
        // XWayland-only coverage is a known limitation, not an open question —
        // asking the user to test it would waste their time.
        let v = HotkeyVerdict::assess(Binding::X11Grab, true);
        assert_eq!(v.confidence, Confidence::Broken);
        assert!(!v.needs_confirmation());
    }

    #[test]
    fn a_conflict_is_broken_and_not_worth_asking_about() {
        let v = HotkeyVerdict::assess(Binding::Conflict, false);
        assert_eq!(v.confidence, Confidence::Broken);
        assert!(!v.needs_confirmation());
    }

    #[test]
    fn a_lost_portal_session_is_broken_not_a_question() {
        // The B6 lesson, applied to the portal path: a verdict must describe
        // the key's state NOW, not the registration's past success. A dead
        // session cannot deliver the confirming keypress, so asking the user
        // to press it would be the old lie in a new place.
        for wayland in [true, false] {
            let v = HotkeyVerdict::assess(Binding::PortalLost, wayland);
            assert_eq!(v.confidence, Confidence::Broken);
            assert!(!v.needs_confirmation());
        }
    }

    #[test]
    fn nothing_bound_is_broken() {
        assert_eq!(
            HotkeyVerdict::assess(Binding::None, false).confidence,
            Confidence::Broken
        );
        assert_eq!(
            HotkeyVerdict::assess(Binding::None, true).confidence,
            Confidence::Broken
        );
    }

    #[test]
    fn a_press_promotes_an_unverified_grab_to_reliable() {
        let v = HotkeyVerdict::assess(Binding::X11Grab, false).confirmed();
        assert_eq!(v.confidence, Confidence::Reliable);
        assert!(!v.needs_confirmation());
        // How it was bound is preserved — diagnostics still need to say the
        // press is what proved it.
        assert_eq!(v.binding, Binding::X11Grab);
    }

    #[test]
    fn every_unreliable_verdict_explains_itself_distinctly() {
        // A generic "may not work" for a case we can describe precisely is the
        // failure this whole item is about.
        let cases = [
            HotkeyVerdict::assess(Binding::X11Grab, false),
            HotkeyVerdict::assess(Binding::X11Grab, true),
            HotkeyVerdict::assess(Binding::PortalLost, false),
            HotkeyVerdict::assess(Binding::Conflict, false),
            HotkeyVerdict::assess(Binding::None, false),
        ];
        let mut seen = std::collections::HashSet::new();
        for c in cases {
            assert!(
                seen.insert(c.explain()),
                "duplicate explanation for {c:?}: {}",
                c.explain()
            );
            assert_ne!(c.explain(), "the hotkey may not work", "{c:?} fell through");
        }
    }
}
