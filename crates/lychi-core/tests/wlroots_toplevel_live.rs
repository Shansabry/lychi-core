//! Exercises the wlroots foreign-toplevel path against a **real** wlroots
//! compositor, headless.
//!
//! This is the test the Sway SIGABRT needed and did not have. `lychi-core`
//! drives a hand-written Wayland `Dispatch` state machine, and any
//! object-creating event without an `event_created_child!` specialization
//! panics inside a C callback — which cannot unwind, so it becomes
//! `fatal runtime error: failed to initiate panic` → SIGABRT. Total startup
//! failure, not a degraded feature.
//!
//! The existing unit test is honestly labelled a *value* check: it asserts the
//! opcode constant is still 0. That cannot catch a second object-creating event
//! being added to the protocol, a wayland-client upgrade changing the
//! specialization requirements, or any other member of the same class. Only
//! running the code against a compositor that actually implements the protocol
//! does — and neither developer machine (KDE) advertises the global, so the
//! whole path is dead code there. That is precisely why the bug shipped.
//!
//! ## Running it
//!
//! `#[ignore]`d: it needs a headless wlroots compositor on `WAYLAND_DISPLAY`.
//! CI (and `scripts/test-wlroots.sh`) provide one:
//!
//! ```text
//! sway -c <headless.conf> &          # WLR_BACKENDS=headless
//! WAYLAND_DISPLAY=wayland-1 \
//!   cargo test -p lychi-core --test wlroots_toplevel_live -- --ignored
//! ```
//!
//! The point is not the assertions — an empty list is a legitimate result. It
//! is that **the process survives**: an abort here fails the run.

/// Is a compositor advertising the wlroots foreign-toplevel protocol reachable?
///
/// Guards against the test silently passing on a KDE/GNOME session, where the
/// global is never advertised, the manager is never bound, and none of the
/// dispatch code under test ever runs. A green run there would be worthless and
/// indistinguishable from a real one.
fn wlroots_protocol_available() -> bool {
    use wayland_client::Connection;
    let Ok(conn) = Connection::connect_to_env() else {
        return false;
    };
    let Ok((globals, _queue)) = wayland_client::globals::registry_queue_init::<Probe>(&conn) else {
        return false;
    };
    globals
        .contents()
        .clone_list()
        .iter()
        .any(|g| g.interface == "zwlr_foreign_toplevel_manager_v1")
}

struct Probe;
impl
    wayland_client::Dispatch<
        wayland_client::protocol::wl_registry::WlRegistry,
        wayland_client::globals::GlobalListContents,
    > for Probe
{
    fn event(
        _: &mut Self,
        _: &wayland_client::protocol::wl_registry::WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &wayland_client::globals::GlobalListContents,
        _: &Connection,
        _: &wayland_client::QueueHandle<Self>,
    ) {
    }
}
use wayland_client::Connection;

#[test]
#[ignore = "needs a headless wlroots compositor (see scripts/test-wlroots.sh)"]
fn listing_toplevels_on_wlroots_does_not_abort() {
    assert!(
        wlroots_protocol_available(),
        "no zwlr_foreign_toplevel_manager_v1 on WAYLAND_DISPLAY={:?} — this \
         test is meaningless without one (KDE/GNOME never advertise it, so \
         every dispatch path under test would be skipped)",
        std::env::var("WAYLAND_DISPLAY").unwrap_or_default()
    );

    // The assertion that matters is reaching the next line at all. The
    // `toplevel` event is what created the child object that aborted.
    let windows = lychi_core::context::wlr_toplevel::list_toplevels();
    println!("wlroots reported {} toplevel(s)", windows.len());
    for w in &windows {
        println!(
            "  app_id={:?} title={:?} activated={}",
            w.app_id, w.title, w.activated
        );
        // A toplevel the compositor announced must arrive fully accumulated:
        // the per-handle events (title/app_id/state) are separate dispatches,
        // and dropping one silently would show up as an empty identity.
        assert!(
            !w.app_id.is_empty() || !w.title.is_empty(),
            "a toplevel with neither app_id nor title means handle events were \
             announced but never dispatched"
        );
    }
}

#[test]
#[ignore = "needs a headless wlroots compositor (see scripts/test-wlroots.sh)"]
fn repeated_connections_do_not_abort() {
    assert!(wlroots_protocol_available(), "no wlroots compositor");
    // Each call opens its own connection and rebinds the manager, so this
    // re-runs the registry + event_created_child path from scratch. A
    // specialization that is missing only on a later bind would show here.
    for i in 0..5 {
        let n = lychi_core::context::wlr_toplevel::list_toplevels().len();
        println!("pass {i}: {n} toplevel(s)");
    }
}
