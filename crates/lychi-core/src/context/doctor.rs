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

use super::capabilities::{self, Portal};
use super::session::{Desktop, SessionType, session};

/// Build the report. Returns plain text — a `doctor` that needed a UI to read
/// would not survive the case it exists for, which is an app that will not
/// start.
pub fn report() -> String {
    let mut out = String::new();
    let s = session();

    out.push_str("Lychi doctor\n");
    out.push_str(&format!(
        "  version           {}\n",
        env!("CARGO_PKG_VERSION")
    ));

    out.push_str("\nEnvironment (verbatim)\n");
    for (k, v) in [
        ("XDG_CURRENT_DESKTOP", &s.raw_current_desktop),
        ("XDG_SESSION_DESKTOP", &s.raw_session_desktop),
        ("XDG_SESSION_TYPE", &s.raw_session_type),
        ("WAYLAND_DISPLAY", &s.raw_wayland_display),
    ] {
        let shown = if v.is_empty() { "(unset)" } else { v.as_str() };
        out.push_str(&format!("  {k:<20}{shown}\n"));
    }

    out.push_str("\nDetected\n");
    let chain = if s.desktops.is_empty() {
        "(none — XDG_CURRENT_DESKTOP is unset)".to_string()
    } else {
        s.desktops
            .iter()
            .map(|d| format!("{d:?}"))
            .collect::<Vec<_>>()
            .join(" -> ")
    };
    out.push_str(&format!("  desktop chain     {chain}\n"));
    out.push_str(&format!(
        "  session type      {}\n",
        match s.session_type {
            SessionType::Wayland => "Wayland",
            SessionType::X11 => "X11",
        }
    ));

    // Probed, not inferred. This is the section that answers "why is this
    // feature missing" without anyone having to guess from the desktop name.
    let caps = capabilities::probe_all();
    out.push_str("\nCapabilities (probed)\n");
    for p in [
        Portal::Screenshot,
        Portal::GlobalShortcuts,
        Portal::Settings,
        Portal::OpenUri,
        Portal::Notification,
    ] {
        let present = caps.portals.iter().any(|i| i == p.name());
        out.push_str(&format!(
            "  {:<40}{}\n",
            p.name(),
            if present { "yes" } else { "NO" }
        ));
    }
    out.push_str(&format!(
        "  {:<40}{}\n",
        "org.kde.KWin (scripting)",
        yes_no(caps.kwin_scripting)
    ));
    out.push_str(&format!(
        "  {:<40}{}\n",
        "org.gnome.Shell",
        yes_no(caps.gnome_shell)
    ));
    if caps.portals.is_empty() {
        out.push_str(
            "  NOTE: the portal frontend did not answer. It is D-Bus-activated,\n\
             \x20       so this can mean it has not started yet rather than that it\n\
             \x20       is missing. Re-run to retry.\n",
        );
    }

    // What those facts cost, said plainly. A capability table is only useful if
    // the reader knows which of their symptoms it explains.
    out.push_str("\nConsequences\n");
    let mut notes: Vec<String> = Vec::new();
    if !caps.kwin_scripting && !caps.gnome_shell && s.is_wayland() {
        notes.push(
            "No compositor scripting interface. Window listing and focus rely on \
             wlr-foreign-toplevel; if this compositor lacks it, `win`/`focus` will \
             find nothing."
                .into(),
        );
    }
    if s.is_wayland() && s.primary().is_gnome_family() {
        notes.push(
            "GNOME Wayland: Mutter implements neither wlr-foreign-toplevel nor a \
             scripting interface, so window context and window commands are \
             unavailable. This is a compositor limitation, not a misconfiguration."
                .into(),
        );
    }
    if !caps
        .portals
        .iter()
        .any(|i| i == Portal::GlobalShortcuts.name())
        && s.is_wayland()
    {
        notes.push(
            "No GlobalShortcuts portal on a Wayland session: the global hotkey \
             cannot be registered. Bind a shortcut to `lychi toggle` in your \
             desktop settings instead."
                .into(),
        );
    }
    if !s.is_wayland() {
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
    if !caps.portals.iter().any(|i| i == Portal::Screenshot.name()) {
        notes.push(
            "No Screenshot portal: screenshots fall back to whichever CLI tool is \
             installed (spectacle, grim, gnome-screenshot, maim, scrot)."
                .into(),
        );
    }
    if notes.is_empty() {
        out.push_str("  Nothing notable — every capability Lychi uses is present.\n");
    } else {
        for n in notes {
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
}
