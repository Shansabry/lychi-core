//! What this desktop can actually do, established by asking rather than guessing.
//!
//! The distinction from [`super::session`] is the point of the split. A desktop
//! *name* tells you which quirks to expect; it cannot tell you whether a feature
//! is present. GNOME's GlobalShortcuts portal version varies by release, KDE and
//! GNOME portal backends can both be installed at once (they are, on the dev
//! machine), and distros spell the same session differently. Every capability
//! this app branches on is directly probeable, so it is probed.
//!
//! Cost: one D-Bus `Introspect` returns every portal interface in a single call,
//! measured at ~7-10ms including process spawn. Cheap enough to answer honestly
//! instead of assuming.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// How long to wait for a D-Bus reply before treating the capability as absent.
///
/// Short on purpose: this runs on paths a user is waiting for, and "the bus did
/// not answer promptly" is operationally the same as "not available".
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// A portal interface, named as it appears on the bus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Portal {
    Screenshot,
    GlobalShortcuts,
    Settings,
    OpenUri,
    Notification,
}

impl Portal {
    fn iface(self) -> &'static str {
        match self {
            Self::Screenshot => "org.freedesktop.portal.Screenshot",
            Self::GlobalShortcuts => "org.freedesktop.portal.GlobalShortcuts",
            Self::Settings => "org.freedesktop.portal.Settings",
            Self::OpenUri => "org.freedesktop.portal.OpenURI",
            Self::Notification => "org.freedesktop.portal.Notification",
        }
    }

    pub fn name(self) -> &'static str {
        self.iface()
    }
}

/// Is `bus_name` currently owned by anything on the session bus?
///
/// The cheap, prompt-free way to ask "is this compositor's scripting interface
/// here?". `NameHasOwner` is answered by the bus daemon itself, so it costs no
/// round trip to the service being asked about — and it is true regardless of
/// how the distro spells the session, which is exactly what name matching gets
/// wrong.
pub fn dbus_name_present(bus_name: &str) -> bool {
    let Ok(conn) = dbus::blocking::Connection::new_session() else {
        return false;
    };
    let proxy = conn.with_proxy(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        PROBE_TIMEOUT,
    );
    proxy
        .method_call::<(bool,), _, _, _>(
            "org.freedesktop.DBus",
            "NameHasOwner",
            (bus_name.to_string(),),
        )
        .map(|(owned,)| owned)
        .unwrap_or(false)
}

/// The portal interfaces this desktop implements, as one introspection.
///
/// Returns `None` when the portal frontend is not reachable at all, which is
/// meaningfully different from "reachable but offers nothing" — the former is
/// usually a portal that has not been D-Bus-activated yet.
fn introspect_portals() -> Option<Vec<String>> {
    let conn = dbus::blocking::Connection::new_session().ok()?;
    let proxy = conn.with_proxy(
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        PROBE_TIMEOUT,
    );
    let (xml,): (String,) = proxy
        .method_call("org.freedesktop.DBus.Introspectable", "Introspect", ())
        .ok()?;
    // Parsing the interface names out of the introspection XML rather than
    // pulling in an XML crate: the shape is fixed and machine-generated, and a
    // dependency for one attribute would not earn itself.
    Some(
        xml.split("<interface name=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .map(str::to_string)
            .collect(),
    )
}

/// Cached portal list. `None` means "not probed yet or probe failed".
static PORTALS: OnceLock<Mutex<Option<Vec<String>>>> = OnceLock::new();

fn portals_cell() -> &'static Mutex<Option<Vec<String>>> {
    PORTALS.get_or_init(|| Mutex::new(None))
}

/// Is `portal` available?
///
/// **Positive results are cached; negative ones are not.** That asymmetry is
/// deliberate. `xdg-desktop-portal` is D-Bus-activated and its backend can lose
/// a startup race — precisely the autostart scenario where this app's window
/// strategy went wrong. Caching "absent" would make a transient miss permanent
/// for the life of the process, so a failed probe is simply retried next time.
pub fn has_portal(portal: Portal) -> bool {
    let mut cell = portals_cell().lock().unwrap_or_else(|e| e.into_inner());
    if cell.is_none() {
        *cell = introspect_portals();
    }
    cell.as_ref()
        .is_some_and(|list| list.iter().any(|i| i == portal.iface()))
}

/// Everything probed, for diagnostics.
///
/// Deliberately re-probes rather than reading the cache: a `doctor` command
/// exists to report what is true now, and a stale "absent" is the answer most
/// likely to send someone down the wrong path.
pub struct Capabilities {
    pub portals: Vec<String>,
    pub kwin_scripting: bool,
    pub gnome_shell: bool,
}

pub fn probe_all() -> Capabilities {
    // Refresh the cache while we are here, so a doctor run also repairs a
    // negative left behind by an early failed probe.
    let portals = introspect_portals();
    if let Some(ref list) = portals {
        *portals_cell().lock().unwrap_or_else(|e| e.into_inner()) = Some(list.clone());
    }
    Capabilities {
        portals: portals.unwrap_or_default(),
        kwin_scripting: dbus_name_present("org.kde.KWin"),
        gnome_shell: dbus_name_present("org.gnome.Shell"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The interface names must match what the bus actually publishes. A typo
    /// here fails open — the capability reads as absent and the feature
    /// silently degrades, with nothing to notice.
    #[test]
    fn portal_names_are_fully_qualified() {
        for p in [
            Portal::Screenshot,
            Portal::GlobalShortcuts,
            Portal::Settings,
            Portal::OpenUri,
            Portal::Notification,
        ] {
            let n = p.name();
            assert!(
                n.starts_with("org.freedesktop.portal."),
                "{n} is not a portal interface name"
            );
            assert!(!n.ends_with('.'), "{n} is truncated");
        }
    }

    /// OpenURI is spelled with a capital I — the one portal name whose casing
    /// does not follow from the enum variant.
    #[test]
    fn openuri_keeps_its_spec_casing() {
        assert_eq!(Portal::OpenUri.name(), "org.freedesktop.portal.OpenURI");
    }

    /// Probing must never panic or hang, whatever the bus is doing. These run
    /// in CI with no session bus at all, which is the degenerate case.
    #[test]
    fn probes_are_safe_without_a_session_bus() {
        let _ = dbus_name_present("org.example.NothingIsHere");
        let _ = has_portal(Portal::Screenshot);
        let _ = probe_all();
    }

    /// The introspection parser, against the shape the bus actually returns.
    #[test]
    fn interface_names_are_extracted_from_introspection_xml() {
        let xml = r#"<node>
  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect"/>
  </interface>
  <interface name="org.freedesktop.portal.Screenshot">
    <property name="version" type="u" access="read"/>
  </interface>
  <interface name="org.freedesktop.portal.GlobalShortcuts"/>
</node>"#;
        let found: Vec<String> = xml
            .split("<interface name=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .map(str::to_string)
            .collect();
        assert_eq!(
            found,
            vec![
                "org.freedesktop.DBus.Introspectable",
                "org.freedesktop.portal.Screenshot",
                "org.freedesktop.portal.GlobalShortcuts",
            ]
        );
    }
}
