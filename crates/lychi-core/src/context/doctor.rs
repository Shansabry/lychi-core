//! `lychi doctor` — what this machine looks like to Lychi.
//!
//! Exists because a tester reporting "the hotkey doesn't work and `win` shows
//! nothing" could not be diagnosed. The app reasoned carefully about
//! environments and then recorded none of its conclusions, so every report
//! needed a round trip asking the person to find a log that did not contain the
//! answer either.
//!
//! Prints **raw values and the verdict derived from them**, following
//! `about:support` and `hyprctl systeminfo`. Raw alone cannot be acted on; a
//! verdict alone cannot be checked. Together they make a pasted report
//! diagnosable without a reproduction.

use super::capabilities::{self, Capabilities, Portal};
use super::session::{Desktop, SessionType, session};

/// One portal line in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalStatus {
    pub name: &'static str,
    pub present: bool,
}

/// Everything `doctor` knows, as data.
///
/// Split out from the text so the Setup tab can consume the same conclusions the
/// CLI prints. The **Consequences** list in particular is a decider — it is what
/// turns "no GlobalShortcuts portal" into "your hotkey cannot be registered" —
/// and re-deriving those sentences in the frontend would be a second answer to a
/// question already answered here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticReport {
    pub version: &'static str,
    /// Environment variables verbatim, in display order, unset shown as empty.
    pub raw_env: Vec<(&'static str, String)>,
    pub desktop_chain: Vec<String>,
    pub session_type: &'static str,
    pub portals: Vec<PortalStatus>,
    pub kwin_scripting: bool,
    pub gnome_shell: bool,
    /// True when the portal frontend did not answer at all — which means "not
    /// started yet" as often as "missing", since it is D-Bus-activated.
    pub portals_unanswered: bool,
    /// What the facts above cost the user, in plain sentences.
    pub consequences: Vec<String>,
}

/// Probe the machine and decide what it means.
///
/// Does real work: a D-Bus introspect plus two name lookups. Keep it off any hot
/// path — it belongs behind an explicitly-opened screen or a CLI invocation.
pub fn collect() -> DiagnosticReport {
    let s = session();
    let caps = capabilities::probe_all();

    let portals = [
        Portal::Screenshot,
        Portal::GlobalShortcuts,
        Portal::Settings,
        Portal::OpenUri,
        Portal::Notification,
    ]
    .into_iter()
    .map(|p| PortalStatus {
        name: p.name(),
        present: caps.portals.iter().any(|i| i == p.name()),
    })
    .collect();

    DiagnosticReport {
        version: env!("CARGO_PKG_VERSION"),
        raw_env: vec![
            ("XDG_CURRENT_DESKTOP", s.raw_current_desktop.clone()),
            ("XDG_SESSION_DESKTOP", s.raw_session_desktop.clone()),
            ("XDG_SESSION_TYPE", s.raw_session_type.clone()),
            ("WAYLAND_DISPLAY", s.raw_wayland_display.clone()),
        ],
        desktop_chain: s.desktops.iter().map(|d| format!("{d:?}")).collect(),
        session_type: match s.session_type {
            SessionType::Wayland => "Wayland",
            SessionType::X11 => "X11",
        },
        portals,
        kwin_scripting: caps.kwin_scripting,
        gnome_shell: caps.gnome_shell,
        portals_unanswered: caps.portals.is_empty(),
        consequences: consequences(&caps, s.is_wayland(), s.primary().is_gnome_family()),
    }
}

/// What the probed facts cost, said plainly.
///
/// A capability table is only useful if the reader knows which of their symptoms
/// it explains. Pure over its inputs so every combination is testable — including
/// the ones this machine cannot produce.
fn consequences(caps: &Capabilities, is_wayland: bool, is_gnome_family: bool) -> Vec<String> {
    let mut notes: Vec<String> = Vec::new();
    let has = |p: Portal| caps.portals.iter().any(|i| i == p.name());

    // The definitive verdict, from the same decider the handlers use, so
    // `doctor` and `win` can never disagree about whether windows work here.
    let support = capabilities::WindowSupport::detect();
    if !support.is_available() {
        notes.push(support.explain().into());
    }
    if !caps.kwin_scripting && !caps.gnome_shell && is_wayland {
        notes.push(
            "No compositor scripting interface. Window listing and focus rely on \
             wlr-foreign-toplevel; if this compositor lacks it, `win`/`focus` will \
             find nothing."
                .into(),
        );
    }
    if is_wayland && is_gnome_family {
        notes.push(
            "GNOME Wayland: Mutter implements neither wlr-foreign-toplevel nor a \
             scripting interface, so window context and window commands are \
             unavailable. This is a compositor limitation, not a misconfiguration."
                .into(),
        );
    }
    if !has(Portal::GlobalShortcuts) && is_wayland {
        notes.push(
            "No GlobalShortcuts portal on a Wayland session: the global hotkey \
             cannot be registered. Bind a shortcut to `lychi toggle` in your \
             desktop settings instead."
                .into(),
        );
    }
    if !is_wayland {
        // The X11 counterpart to the portal note above. An XGrabKey cannot
        // override a combination the window manager already owns: the grab
        // returns Ok, the log says "registered", and the key never arrives.
        // Doctor has to say this out loud, because every other signal the user
        // can see reports success.
        notes.push(
            "X11 session: a global-shortcut grab reports success even when the \
             window manager keeps the key for itself (KDE's Super+Space → \
             KRunner is the common case). If the hotkey does nothing, it is \
             taken — pick another combination, or bind `lychi toggle` in your \
             desktop's keyboard settings."
                .into(),
        );
    }
    if !has(Portal::Screenshot) {
        notes.push(
            "No Screenshot portal: screenshots fall back to whichever CLI tool is \
             installed (spectacle, grim, gnome-screenshot, maim, scrot)."
                .into(),
        );
    }
    notes
}

/// Build the report. Returns plain text — a `doctor` that needed a UI to read
/// would not survive the case it exists for, which is an app that will not
/// start.
pub fn report() -> String {
    render(&collect())
}

/// Format a [`DiagnosticReport`]. Formatting only: every judgement was already
/// made in [`collect`].
pub fn render(r: &DiagnosticReport) -> String {
    let mut out = String::new();

    out.push_str("Lychi doctor\n");
    out.push_str(&format!("  version           {}\n", r.version));

    out.push_str("\nEnvironment (verbatim)\n");
    for (k, v) in &r.raw_env {
        let shown = if v.is_empty() { "(unset)" } else { v.as_str() };
        out.push_str(&format!("  {k:<20}{shown}\n"));
    }

    out.push_str("\nDetected\n");
    let chain = if r.desktop_chain.is_empty() {
        "(none — XDG_CURRENT_DESKTOP is unset)".to_string()
    } else {
        r.desktop_chain.join(" -> ")
    };
    out.push_str(&format!("  desktop chain     {chain}\n"));
    out.push_str(&format!("  session type      {}\n", r.session_type));

    out.push_str("\nCapabilities (probed)\n");
    for p in &r.portals {
        out.push_str(&format!("  {:<40}{}\n", p.name, yes_no(p.present)));
    }
    out.push_str(&format!(
        "  {:<40}{}\n",
        "org.kde.KWin (scripting)",
        yes_no(r.kwin_scripting)
    ));
    out.push_str(&format!(
        "  {:<40}{}\n",
        "org.gnome.Shell",
        yes_no(r.gnome_shell)
    ));
    if r.portals_unanswered {
        out.push_str(
            "  NOTE: the portal frontend did not answer. It is D-Bus-activated,\n\
             \x20       so this can mean it has not started yet rather than that it\n\
             \x20       is missing. Re-run to retry.\n",
        );
    }

    out.push_str("\nConsequences\n");
    if r.consequences.is_empty() {
        out.push_str("  Nothing notable — every capability Lychi uses is present.\n");
    } else {
        for n in &r.consequences {
            out.push_str(&format!("  - {n}\n"));
        }
    }

    out
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "NO" }
}

/// Whether a desktop is one Lychi has been exercised on, for the report's
/// benefit. Deliberately not used to gate behaviour — an unfamiliar desktop
/// should still get every capability it can actually provide.
pub fn is_well_tested(d: Desktop) -> bool {
    matches!(d, Desktop::Kde | Desktop::Gnome | Desktop::Xfce)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The report must render whatever the machine looks like, including the
    /// degenerate cases — a doctor that panics on an unusual session is useless
    /// exactly where it is needed.
    #[test]
    fn the_report_renders() {
        let r = report();
        assert!(r.contains("Lychi doctor"));
        assert!(r.contains("Environment (verbatim)"));
        assert!(r.contains("Detected"));
        assert!(r.contains("Capabilities (probed)"));
        assert!(r.contains("Consequences"));
    }

    /// Raw values are printed unparsed. A report that only showed our
    /// interpretation would hide the case where the interpretation is the bug —
    /// which is precisely how the session-name-vs-desktop-name confusion
    /// survived.
    #[test]
    fn raw_environment_is_shown_even_when_unset() {
        let r = report();
        for k in [
            "XDG_CURRENT_DESKTOP",
            "XDG_SESSION_DESKTOP",
            "XDG_SESSION_TYPE",
            "WAYLAND_DISPLAY",
        ] {
            assert!(r.contains(k), "{k} missing from the report");
        }
    }

    /// Every capability line resolves to a definite yes or NO. "Unknown" would
    /// put the reader back where they started.
    #[test]
    fn capabilities_are_reported_definitively() {
        let r = report();
        let caps = r
            .split("Capabilities (probed)")
            .nth(1)
            .expect("capabilities section");
        let lines: Vec<&str> = caps.lines().filter(|l| l.starts_with("  org.")).collect();
        assert!(!lines.is_empty(), "no capability lines");
        for l in lines {
            assert!(
                l.ends_with("yes") || l.ends_with("NO"),
                "indefinite capability line: {l:?}"
            );
        }
    }

    // ---- `consequences`, now that the split makes it reachable ----
    //
    // These are the sentences the Setup tab will show. Before the split they
    // could only be exercised by whatever session the test machine happened to
    // be running, so the Wayland notes were unverifiable on an X11 box and vice
    // versa.

    fn caps_with(portals: &[Portal]) -> Capabilities {
        Capabilities {
            portals: portals.iter().map(|p| p.name().to_string()).collect(),
            kwin_scripting: false,
            gnome_shell: false,
        }
    }

    /// The note that matters most: on Wayland without the portal there is no
    /// hotkey at all, and the user needs to be told what to do instead.
    #[test]
    fn a_wayland_session_without_the_shortcuts_portal_is_told_to_bind_the_cli() {
        let caps = caps_with(&[Portal::Screenshot]);
        let notes = consequences(&caps, true, false);
        assert!(
            notes
                .iter()
                .any(|n| n.contains("GlobalShortcuts portal") && n.contains("lychi toggle")),
            "expected the bind-the-CLI note, got {notes:?}"
        );
    }

    /// With the portal present, that note must not appear — otherwise every KDE
    /// user is told to work around a problem they do not have.
    #[test]
    fn a_wayland_session_with_the_portal_is_not_warned_about_shortcuts() {
        let caps = caps_with(&[Portal::GlobalShortcuts, Portal::Screenshot]);
        let notes = consequences(&caps, true, false);
        assert!(
            !notes.iter().any(|n| n.contains("GlobalShortcuts portal")),
            "warned about a portal that is present: {notes:?}"
        );
    }

    /// X11 always gets the stolen-grab warning, because every other signal the
    /// user can see reports success. This is the B6 failure written down.
    #[test]
    fn an_x11_session_is_always_warned_that_a_grab_can_be_silently_stolen() {
        let caps = caps_with(&[Portal::GlobalShortcuts, Portal::Screenshot]);
        let notes = consequences(&caps, false, false);
        assert!(
            notes.iter().any(|n| n.contains("X11 session")),
            "expected the stolen-grab warning, got {notes:?}"
        );
    }

    /// ...and Wayland never gets it, since it describes an X11 mechanism.
    #[test]
    fn a_wayland_session_is_not_warned_about_x11_grabs() {
        let caps = caps_with(&[Portal::GlobalShortcuts, Portal::Screenshot]);
        let notes = consequences(&caps, true, false);
        assert!(!notes.iter().any(|n| n.contains("X11 session")));
    }

    #[test]
    fn a_missing_screenshot_portal_names_the_fallback_tools() {
        let caps = caps_with(&[Portal::GlobalShortcuts]);
        let notes = consequences(&caps, true, false);
        assert!(
            notes
                .iter()
                .any(|n| n.contains("Screenshot portal") && n.contains("grim")),
            "expected the screenshot fallback note, got {notes:?}"
        );
    }

    /// GNOME Wayland's window limitation is a compositor fact, and saying so
    /// stops it being reported as a Lychi bug.
    #[test]
    fn gnome_wayland_is_told_the_limitation_is_the_compositors() {
        let caps = caps_with(&[Portal::GlobalShortcuts, Portal::Screenshot]);
        let notes = consequences(&caps, true, true);
        assert!(
            notes
                .iter()
                .any(|n| n.contains("GNOME Wayland") && n.contains("compositor limitation")),
            "expected the GNOME note, got {notes:?}"
        );
    }

    #[test]
    fn a_non_gnome_session_does_not_get_the_gnome_note() {
        let caps = caps_with(&[Portal::GlobalShortcuts, Portal::Screenshot]);
        let notes = consequences(&caps, true, false);
        assert!(!notes.iter().any(|n| n.contains("GNOME Wayland")));
    }

    /// `render` formats; it decides nothing. Given an empty consequence list it
    /// must say so rather than inventing a verdict.
    #[test]
    fn render_reports_a_clean_machine_when_there_are_no_consequences() {
        let r = DiagnosticReport {
            version: "0.0.0",
            raw_env: vec![("XDG_CURRENT_DESKTOP", "KDE".into())],
            desktop_chain: vec!["Kde".into()],
            session_type: "Wayland",
            portals: vec![PortalStatus {
                name: "org.freedesktop.portal.Screenshot",
                present: true,
            }],
            kwin_scripting: true,
            gnome_shell: false,
            portals_unanswered: false,
            consequences: vec![],
        };
        let text = render(&r);
        assert!(text.contains("Nothing notable"));
        assert!(text.contains("0.0.0"));
    }

    /// An unset variable prints as `(unset)`, not as an empty column — the
    /// difference between "we looked and it was empty" and "we forgot to look".
    #[test]
    fn render_marks_unset_environment_variables() {
        let r = DiagnosticReport {
            version: "0.0.0",
            raw_env: vec![("WAYLAND_DISPLAY", String::new())],
            desktop_chain: vec![],
            session_type: "X11",
            portals: vec![],
            kwin_scripting: false,
            gnome_shell: false,
            portals_unanswered: true,
            consequences: vec!["something".into()],
        };
        let text = render(&r);
        assert!(text.contains("(unset)"));
        assert!(text.contains("XDG_CURRENT_DESKTOP is unset"));
        assert!(text.contains("did not answer"));
    }
}
