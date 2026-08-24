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

/// The input-method landscape, as facts. CJK input lives or dies on these,
/// and every failure mode looks identical from the launcher ("typing gives
/// ascii") — only the combination of facts distinguishes "daemon not running"
/// from "GTK module not installed" from "a stale AppImage hook is overriding
/// the module path".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImeFacts {
    /// GTK_IM_MODULE verbatim ("" = unset; unset is normal — GTK then asks
    /// XSETTINGS or the immodules cache).
    pub gtk_im_module: String,
    /// XMODIFIERS verbatim (the XIM fallback route, e.g. "@im=fcitx").
    pub xmodifiers: String,
    /// GTK_IM_MODULE_FILE verbatim. Lychi's own AppImage must never set this:
    /// it pins GTK to a bundled, IM-less module cache and silently kills CJK
    /// input (issue #48). Set here = a stale hook or another AppImage leaked.
    pub gtk_im_module_file: String,
    /// GDK_BACKEND verbatim — Lychi defaults to x11 (XWayland), which is the
    /// battle-tested IME path; a user override to wayland changes the story.
    pub gdk_backend: String,
    /// org.fcitx.Fcitx5 has a bus owner.
    pub fcitx_daemon: bool,
    /// org.freedesktop.IBus has a bus owner.
    pub ibus_daemon: bool,
    /// Whether the HOST GTK3 immodules cache lists an fcitx module.
    /// None = no host cache was found to inspect.
    pub host_cache_has_fcitx: Option<bool>,
    /// Same, for ibus.
    pub host_cache_has_ibus: Option<bool>,
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
    pub ime: ImeFacts,
    /// What the facts above cost the user, in plain sentences.
    pub consequences: Vec<String>,
}

/// Probe the input-method landscape. Two bus-daemon name lookups plus one
/// file read — cheap, but still doctor-only, off any hot path.
fn collect_ime() -> ImeFacts {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    // Distro layouts for the host GTK3 module cache: Debian/Ubuntu multiarch,
    // Fedora lib64, Arch plain lib. First one that exists speaks for the host.
    let cache = [
        "/usr/lib/x86_64-linux-gnu/gtk-3.0/3.0.0/immodules.cache",
        "/usr/lib64/gtk-3.0/3.0.0/immodules.cache",
        "/usr/lib/gtk-3.0/3.0.0/immodules.cache",
    ]
    .iter()
    .find_map(|p| std::fs::read_to_string(p).ok());

    ImeFacts {
        gtk_im_module: env("GTK_IM_MODULE"),
        xmodifiers: env("XMODIFIERS"),
        gtk_im_module_file: env("GTK_IM_MODULE_FILE"),
        gdk_backend: env("GDK_BACKEND"),
        fcitx_daemon: capabilities::dbus_name_present("org.fcitx.Fcitx5"),
        ibus_daemon: capabilities::dbus_name_present("org.freedesktop.IBus"),
        host_cache_has_fcitx: cache.as_deref().map(|c| c.contains("fcitx")),
        host_cache_has_ibus: cache.as_deref().map(|c| c.contains("ibus")),
    }
}

/// What the IME facts cost, said plainly. Pure over its input so every
/// combination is testable, most of which this machine cannot produce.
///
/// Deliberately silent when no IME framework is present at all — most users
/// have none, and doctor must not manufacture noise for them.
fn ime_consequences(ime: &ImeFacts) -> Vec<String> {
    let mut notes: Vec<String> = Vec::new();

    if !ime.gtk_im_module_file.is_empty() {
        notes.push(format!(
            "GTK_IM_MODULE_FILE is set ({}) — something is pinning GTK to a \
             fixed input-module cache. If that cache lacks your input method's \
             module, CJK input dies silently. Lychi's own AppImage never sets \
             this; unset it (a stale launcher script or another AppImage's \
             hook is the usual source).",
            ime.gtk_im_module_file
        ));
    }
    if ime.fcitx_daemon && ime.host_cache_has_fcitx == Some(false) {
        notes.push(
            "fcitx5 is running but the host GTK3 module cache has no fcitx \
             module — install your distro's fcitx5 GTK module (fcitx5-gtk / \
             fcitx5-gtk3) or GTK apps, Lychi included, cannot reach it."
                .into(),
        );
    }
    if ime.ibus_daemon && ime.host_cache_has_ibus == Some(false) {
        notes.push(
            "ibus is running but the host GTK3 module cache has no ibus \
             module — install ibus-gtk3 or GTK apps, Lychi included, cannot \
             reach it."
                .into(),
        );
    }
    if ime.fcitx_daemon && !ime.ibus_daemon && ime.gtk_im_module == "ibus" {
        notes.push(
            "GTK_IM_MODULE=ibus but the running input daemon is fcitx5 — GTK \
             apps will try to talk to a daemon that isn't there. Set \
             GTK_IM_MODULE=fcitx (and XMODIFIERS=@im=fcitx) for this session."
                .into(),
        );
    }
    if ime.ibus_daemon && !ime.fcitx_daemon && ime.gtk_im_module.starts_with("fcitx") {
        notes.push(
            "GTK_IM_MODULE names fcitx but the running input daemon is ibus — \
             GTK apps will try to talk to a daemon that isn't there. Set \
             GTK_IM_MODULE=ibus for this session."
                .into(),
        );
    }
    if !ime.fcitx_daemon
        && !ime.ibus_daemon
        && (ime.gtk_im_module.starts_with("fcitx") || ime.gtk_im_module == "ibus")
    {
        notes.push(format!(
            "GTK_IM_MODULE={} but no input-method daemon is on the bus — the \
             module will connect to nothing and input stays plain. Start the \
             daemon (or remove the variable if you no longer use an IME).",
            ime.gtk_im_module
        ));
    }
    notes
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

    let ime = collect_ime();

    DiagnosticReport {
        version: env!("CARGO_PKG_VERSION"),
        raw_env: vec![
            ("XDG_CURRENT_DESKTOP", s.raw_current_desktop.clone()),
            ("XDG_SESSION_DESKTOP", s.raw_session_desktop.clone()),
            ("XDG_SESSION_TYPE", s.raw_session_type.clone()),
            ("WAYLAND_DISPLAY", s.raw_wayland_display.clone()),
            ("GDK_BACKEND", ime.gdk_backend.clone()),
            ("GTK_IM_MODULE", ime.gtk_im_module.clone()),
            ("XMODIFIERS", ime.xmodifiers.clone()),
            ("GTK_IM_MODULE_FILE", ime.gtk_im_module_file.clone()),
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
        consequences: {
            let mut all = consequences(&caps, s.is_wayland(), s.primary().is_gnome_family());
            all.extend(ime_consequences(&ime));
            all
        },
        ime,
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
    out.push_str(&format!(
        "  {:<40}{}\n",
        "org.fcitx.Fcitx5 (input method)",
        yes_no(r.ime.fcitx_daemon)
    ));
    out.push_str(&format!(
        "  {:<40}{}\n",
        "org.freedesktop.IBus (input method)",
        yes_no(r.ime.ibus_daemon)
    ));
    let cache_line = |v: Option<bool>| match v {
        None => "no host cache found",
        Some(true) => "yes",
        Some(false) => "NO",
    };
    out.push_str(&format!(
        "  {:<40}{}\n",
        "host GTK cache: fcitx module",
        cache_line(r.ime.host_cache_has_fcitx)
    ));
    out.push_str(&format!(
        "  {:<40}{}\n",
        "host GTK cache: ibus module",
        cache_line(r.ime.host_cache_has_ibus)
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
            ime: ImeFacts::default(),
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
            ime: ImeFacts::default(),
            consequences: vec!["something".into()],
        };
        let text = render(&r);
        assert!(text.contains("(unset)"));
        assert!(text.contains("XDG_CURRENT_DESKTOP is unset"));
        assert!(text.contains("did not answer"));
    }

    #[test]
    fn a_machine_without_any_ime_gets_no_ime_notes() {
        // The overwhelmingly common case: no daemon, no env vars. Doctor must
        // say nothing rather than teach users about a subsystem they lack.
        assert!(ime_consequences(&ImeFacts::default()).is_empty());
    }

    #[test]
    fn a_healthy_fcitx_setup_gets_no_notes() {
        let ime = ImeFacts {
            fcitx_daemon: true,
            gtk_im_module: "fcitx".into(),
            xmodifiers: "@im=fcitx".into(),
            host_cache_has_fcitx: Some(true),
            host_cache_has_ibus: Some(false),
            ..Default::default()
        };
        assert!(ime_consequences(&ime).is_empty());
    }

    #[test]
    fn fcitx_running_without_its_gtk_module_names_the_missing_package() {
        // The classic "fcitx5 installed without fcitx5-gtk" failure: daemon
        // alive, GTK structurally unable to reach it.
        let ime = ImeFacts {
            fcitx_daemon: true,
            host_cache_has_fcitx: Some(false),
            ..Default::default()
        };
        let notes = ime_consequences(&ime);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("fcitx5-gtk"), "{notes:?}");
    }

    #[test]
    fn a_pinned_module_cache_is_flagged_loudly() {
        // The issue-48 mechanism: GTK_IM_MODULE_FILE overrides everything and
        // fails silently. It must be called out even when nothing else is wrong.
        let ime = ImeFacts {
            gtk_im_module_file: "/tmp/other.AppDir/immodules.cache".into(),
            ..Default::default()
        };
        let notes = ime_consequences(&ime);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("GTK_IM_MODULE_FILE"), "{notes:?}");
    }

    #[test]
    fn a_daemon_and_module_mismatch_is_called_out_both_ways() {
        let fcitx_daemon_ibus_module = ImeFacts {
            fcitx_daemon: true,
            gtk_im_module: "ibus".into(),
            host_cache_has_fcitx: Some(true),
            host_cache_has_ibus: Some(true),
            ..Default::default()
        };
        assert!(
            ime_consequences(&fcitx_daemon_ibus_module)
                .iter()
                .any(|n| n.contains("running input daemon is fcitx5"))
        );

        let ibus_daemon_fcitx_module = ImeFacts {
            ibus_daemon: true,
            gtk_im_module: "fcitx".into(),
            host_cache_has_fcitx: Some(true),
            host_cache_has_ibus: Some(true),
            ..Default::default()
        };
        assert!(
            ime_consequences(&ibus_daemon_fcitx_module)
                .iter()
                .any(|n| n.contains("running input daemon is ibus"))
        );
    }

    #[test]
    fn a_module_configured_with_no_daemon_running_is_called_out() {
        let ime = ImeFacts {
            gtk_im_module: "fcitx".into(),
            ..Default::default()
        };
        let notes = ime_consequences(&ime);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("no input-method daemon"), "{notes:?}");
    }

    #[test]
    fn the_report_shows_the_ime_probe_lines() {
        let r = DiagnosticReport {
            version: "0.0.0",
            raw_env: vec![],
            desktop_chain: vec![],
            session_type: "X11",
            portals: vec![],
            kwin_scripting: false,
            gnome_shell: false,
            portals_unanswered: false,
            ime: ImeFacts {
                fcitx_daemon: true,
                host_cache_has_fcitx: Some(true),
                host_cache_has_ibus: None,
                ..Default::default()
            },
            consequences: vec![],
        };
        let text = render(&r);
        assert!(text.contains("org.fcitx.Fcitx5"), "{text}");
        assert!(text.contains("host GTK cache: fcitx module"), "{text}");
        assert!(text.contains("no host cache found"), "{text}");
    }
}
