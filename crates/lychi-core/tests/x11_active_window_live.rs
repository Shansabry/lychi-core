//! Exercises the X11 EWMH window detector against a real X server.
//!
//! `detect_x11` is the **most widely used** of the three window backends — it
//! serves every X11 session on every desktop — and it had no test at all. It
//! reads `_NET_ACTIVE_WINDOW`, `_NET_WM_NAME`, `WM_CLASS` and `_NET_WM_PID`
//! from the root and focused windows, so it needs both an X server *and* an
//! EWMH-compliant window manager to set those properties. Xvfb + openbox
//! provides both headlessly.
//!
//! What this catches that a unit test cannot: an atom name typo, reading the
//! wrong property type, a `value32()` misparse, or the `WM_CLASS` NUL-split
//! being wrong. Every one of those compiles fine and returns `None` at
//! runtime — indistinguishable from "no window focused", which is a legitimate
//! result. Silent degradation, exactly the failure mode B2 is about.
//!
//! `#[ignore]`d: needs `DISPLAY` pointing at an X server with a running WM.
//! Use `scripts/test-x11.sh`; CI runs it on every PR.

/// Is there an X server with an EWMH window manager and a focused window?
///
/// Without a WM nothing sets `_NET_ACTIVE_WINDOW`, so `detect_x11` correctly
/// returns `None` and the test would pass while asserting nothing.
fn ewmh_active_window_present() -> bool {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};

    let Ok((conn, screen_num)) = x11rb::rust_connection::RustConnection::connect(None) else {
        return false;
    };
    let root = conn.setup().roots[screen_num].root;
    let Ok(atom) = conn.intern_atom(false, b"_NET_ACTIVE_WINDOW") else {
        return false;
    };
    let Ok(atom) = atom.reply() else { return false };
    let Ok(cookie) = conn.get_property(false, root, atom.atom, AtomEnum::WINDOW, 0, 1) else {
        return false;
    };
    let Ok(reply) = cookie.reply() else {
        return false;
    };
    reply.value32().and_then(|mut v| v.next()).unwrap_or(0) != 0
}

#[test]
#[ignore = "needs Xvfb + an EWMH window manager (see scripts/test-x11.sh)"]
fn detects_the_focused_x11_window() {
    assert!(
        ewmh_active_window_present(),
        "no EWMH _NET_ACTIVE_WINDOW on DISPLAY={:?} — without a window manager \
         and a focused window this test asserts nothing",
        std::env::var("DISPLAY").unwrap_or_default()
    );

    let w = lychi_core::context::active_window::detect_x11_for_test()
        .expect("detect_x11 returned None while a window is demonstrably focused");

    println!(
        "detected: wm_class={:?} title={:?} pid={}",
        w.wm_class, w.title, w.pid
    );

    // Each of these is read from a different X property with its own atom and
    // decoding, so they fail independently — assert them independently.
    assert!(!w.wm_class.is_empty(), "WM_CLASS was not read");
    assert!(!w.title.is_empty(), "_NET_WM_NAME / WM_NAME was not read");
    assert!(w.pid > 0, "_NET_WM_PID was not read");

    // The pid must name a live process; a misparse typically yields a plausible
    // but wrong number, which an `> 0` check alone would accept.
    assert!(
        std::path::Path::new(&format!("/proc/{}", w.pid)).exists(),
        "pid {} does not exist — _NET_WM_PID was misparsed",
        w.pid
    );
}

#[test]
#[ignore = "needs Xvfb + an EWMH window manager (see scripts/test-x11.sh)"]
fn repeated_detection_is_stable() {
    assert!(ewmh_active_window_present(), "no EWMH window manager");
    // Each call opens a fresh X connection. A leaked connection or an atom
    // interned per-call against a closed connection would show up here rather
    // than on the first call.
    let first = lychi_core::context::active_window::detect_x11_for_test();
    for _ in 0..5 {
        let again = lychi_core::context::active_window::detect_x11_for_test();
        assert_eq!(
            first.as_ref().map(|w| &w.wm_class),
            again.as_ref().map(|w| &w.wm_class),
            "detection is not stable across connections"
        );
    }
}
