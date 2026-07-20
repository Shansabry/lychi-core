//! wlroots foreign-toplevel backend — window enumeration + focus/close for
//! wlroots-family compositors (Sway, Hyprland, niri, Wayfire, COSMIC), where
//! neither the KWin scripting backend (KDE-only) nor X11 EWMH apply.
//!
//! This speaks `wlr-foreign-toplevel-management-unstable-v1`, the standard
//! Wayland protocol a taskbar/dock uses to list open windows and act on them.
//! We chose it over `ext-foreign-toplevel-list-v1` because the ext protocol
//! only *lists* (title/app_id) — the wlr one also gives per-window `state`
//! (which window is `activated`/focused) and the `activate`/`close` requests
//! we need for the `win` command. The wlr protocol is implemented by every
//! wlroots compositor plus KWin; GNOME/Mutter implements neither (by design),
//! so GNOME stays unsupported — consistent with the rest of the codebase.
//!
//! Unlike the GTK-bound plasma-shell code in `src-tauri`, this lives in
//! `lychi-core` (which must not import GTK), so we open our **own** Wayland
//! connection via `connect_to_env()` and drive a private event queue to
//! completion with a `roundtrip`. The protocol is stateful (title/app_id/state
//! arrive as separate events per toplevel, terminated by a `done`), so we
//! implement real `Dispatch` handlers that accumulate into a `State` rather
//! than the fire-and-forget `delegate_noop!` the taskbar code can use.
//!
//! Constraints the protocol imposes, reflected here:
//! - foreign-toplevel exposes **no PID** — `TopLevel::pid` is always 0. Callers
//!   that key on pid must tolerate 0 (we key on the handle identity instead).
//! - it exposes **no stacking/Z-order** — enumeration order is compositor
//!   creation order, not recency. "Which is focused" comes from the `activated`
//!   state flag, which is what the nearest-terminal logic actually needs.

#[cfg(target_os = "linux")]
pub use imp::{activate, close, list_toplevels};

/// A window seen through the foreign-toplevel protocol.
///
/// The protocol assigns each toplevel a connection-local object id that is *not*
/// stable across connections — so we can't hand back an id and resolve it later
/// in a fresh connection. Instead `activate`/`close` take the `app_id` + `title`
/// and re-match within a single connection where the live handle is valid. Two
/// windows of the same app with the same title are practically indistinguishable
/// through this protocol; we act on the first match, which is the best the
/// protocol allows.
#[derive(Debug, Clone)]
pub struct ToplevelWindow {
    pub title: String,
    pub app_id: String,
    pub activated: bool,
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashMap;

    use wayland_client::backend::ObjectId;
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{wl_registry, wl_seat::WlSeat};
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wayland_protocols_wlr::foreign_toplevel::v1::client::{
        zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
        zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
    };

    pub use super::ToplevelWindow;

    /// The `activated` bit in the toplevel `state` array (protocol enum value).
    const STATE_ACTIVATED: u32 = 2;

    /// Accumulator for the event stream. Each toplevel's title/app_id/state
    /// arrive as separate events, flushed by `done`; we collect them keyed by
    /// the handle's object id.
    #[derive(Default)]
    struct State {
        toplevels: HashMap<ObjectId, Entry>,
        seat: Option<WlSeat>,
    }

    #[derive(Default, Clone)]
    struct Entry {
        title: String,
        app_id: String,
        activated: bool,
        /// Set on `closed`; such entries are dropped from the final list.
        closed: bool,
    }

    /// The registry dispatch — we also grab a `wl_seat`, needed for `activate`.
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
        fn event(
            _state: &mut Self,
            _registry: &wl_registry::WlRegistry,
            _event: wl_registry::Event,
            _data: &GlobalListContents,
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
        }
    }

    wayland_client::delegate_noop!(State: ignore WlSeat);

    /// Manager events: a `toplevel` event introduces a new handle (whose own
    /// events then stream in); `finished` ends the manager.
    impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
        fn event(
            state: &mut Self,
            _mgr: &ZwlrForeignToplevelManagerV1,
            event: zwlr_foreign_toplevel_manager_v1::Event,
            _data: &(),
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
                state.toplevels.entry(toplevel.id()).or_default();
            }
        }
    }

    /// Per-toplevel events: title/app_id/state accumulate; `closed` marks the
    /// entry dead. `done` is a no-op for us (we read the accumulated state after
    /// a full roundtrip, not incrementally).
    impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
        fn event(
            state: &mut Self,
            handle: &ZwlrForeignToplevelHandleV1,
            event: zwlr_foreign_toplevel_handle_v1::Event,
            _data: &(),
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            let entry = state.toplevels.entry(handle.id()).or_default();
            use zwlr_foreign_toplevel_handle_v1::Event;
            match event {
                Event::Title { title } => entry.title = title,
                Event::AppId { app_id } => entry.app_id = app_id,
                Event::State { state: bytes } => {
                    // `state` is an array of little-endian u32 enum values.
                    entry.activated = bytes
                        .chunks_exact(4)
                        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                        .any(|v| v == STATE_ACTIVATED);
                }
                Event::Closed => entry.closed = true,
                // Done / OutputEnter / OutputLeave / Parent — not needed here.
                _ => {}
            }
        }
    }

    /// Open a connection, bind the foreign-toplevel manager, and roundtrip until
    /// the compositor has sent the current window list. Returns the connection,
    /// the drained `State`, and the bound manager (kept alive so handle object
    /// ids stay valid while the caller resolves them).
    ///
    /// Returns `None` if this isn't a Wayland session, the compositor doesn't
    /// offer the protocol (not wlroots-family), or the connection fails — every
    /// one of which correctly means "no wlroots window info here", and callers
    /// degrade to empty, never error.
    fn collect() -> Option<(Connection, State, ZwlrForeignToplevelManagerV1)> {
        // Our own connection — lychi-core has no GTK display to borrow.
        let conn = Connection::connect_to_env().ok()?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();

        let mut state = State::default();

        // A seat is required to `activate` a toplevel. Best-effort — if absent,
        // enumeration still works; only focus is unavailable.
        if let Ok(seat) = globals.bind::<WlSeat, _, _>(&qh, 1..=8, ()) {
            state.seat = Some(seat);
        }

        // Bind the manager. Absence = not a wlroots-family compositor → None.
        let manager: ZwlrForeignToplevelManagerV1 = globals.bind(&qh, 1..=3, ()).ok()?;

        // Two roundtrips: the first delivers the `toplevel` events (creating the
        // handles), the second delivers each handle's title/app_id/state before
        // its `done`. One roundtrip can race a handle's details arriving after
        // its introduction; a second flushes them deterministically.
        queue.roundtrip(&mut state).ok()?;
        queue.roundtrip(&mut state).ok()?;

        Some((conn, state, manager))
    }

    /// List all current toplevel windows. Empty on any failure or non-wlroots
    /// session. Ordering is compositor creation order (the protocol exposes no
    /// Z-order); callers needing recency should use the `activated` flag.
    pub fn list_toplevels() -> Vec<ToplevelWindow> {
        let Some((conn, state, _manager)) = collect() else {
            return Vec::new();
        };
        let _ = conn; // held so object ids stay valid through the map below.
        state
            .toplevels
            .values()
            .filter(|e| !e.closed)
            .map(|e| ToplevelWindow {
                title: e.title.clone(),
                app_id: e.app_id.clone(),
                activated: e.activated,
            })
            .collect()
    }

    /// Re-open a connection, find the live handle matching `app_id` + `title`,
    /// and run `action` on it. Done in a single connection because the protocol
    /// object ids are connection-local — an id from an earlier `list_toplevels`
    /// call would be meaningless here. Matching on app_id+title is the only
    /// cross-call-stable key the protocol offers.
    fn with_handle<F>(app_id: &str, title: &str, action: F) -> Result<(), String>
    where
        F: FnOnce(&ZwlrForeignToplevelHandleV1, &State),
    {
        let (conn, state, _manager) =
            collect().ok_or("Foreign-toplevel protocol unavailable on this compositor")?;

        // Find the object id of the first non-closed toplevel matching app+title.
        // app_id is compared case-insensitively because callers hold a
        // normalized (lowercased) wm_class, while the protocol reports the raw
        // app_id.
        let target = state
            .toplevels
            .iter()
            .find(|(_, e)| {
                !e.closed && e.app_id.eq_ignore_ascii_case(app_id) && e.title == title
            })
            .map(|(id, _)| id.clone())
            .ok_or("Window no longer exists")?;

        let handle = ZwlrForeignToplevelHandleV1::from_id(&conn, target)
            .map_err(|e| format!("resolve window handle: {e}"))?;

        action(&handle, &state);

        // Flush the request (activate/close) to the compositor.
        conn.roundtrip().map_err(|e| format!("roundtrip: {e}"))?;
        Ok(())
    }

    /// Focus (activate) the window matching `app_id` + `title`. Needs a seat.
    pub fn activate(app_id: &str, title: &str) -> Result<(), String> {
        let mut seat_missing = false;
        with_handle(app_id, title, |handle, state| match &state.seat {
            Some(seat) => handle.activate(seat),
            None => seat_missing = true,
        })?;
        if seat_missing {
            return Err("No seat available to focus the window".to_string());
        }
        Ok(())
    }

    /// Close the window matching `app_id` + `title`.
    pub fn close(app_id: &str, title: &str) -> Result<(), String> {
        with_handle(app_id, title, |handle, _state| handle.close())
    }
}

#[cfg(not(target_os = "linux"))]
pub fn list_toplevels() -> Vec<ToplevelWindow> {
    Vec::new()
}

#[cfg(not(target_os = "linux"))]
pub fn activate(_app_id: &str, _title: &str) -> Result<(), String> {
    Err("Not supported on this platform".to_string())
}

#[cfg(not(target_os = "linux"))]
pub fn close(_app_id: &str, _title: &str) -> Result<(), String> {
    Err("Not supported on this platform".to_string())
}
