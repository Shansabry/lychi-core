//! Hide the launcher from KDE's taskbar and Alt-Tab switcher on Wayland.
//!
//! `set_skip_taskbar_hint()` only sets X11 atoms, which Wayland ignores
//! (tauri#9829 / docs/issues.md I-009). KWin's own mechanism is the
//! `org_kde_plasma_shell` protocol: create a plasma surface for our
//! wl_surface and flag it skip-taskbar/skip-switcher — the same thing
//! KDE's LayerShell-Qt and plasma-integration do for Qt apps.
//!
//! We piggyback on GDK's already-open Wayland connection (foreign display)
//! with a private event queue, so GTK's own event handling is untouched.
//!
//! Protocol constraints that shape this module (a violation is a fatal
//! protocol error that kills the whole shared connection):
//! - org_kde_plasma_shell may only be bound ONCE per connection
//!   → bind once per process, cache it.
//! - only ONE plasma surface may exist per wl_surface
//!   → satisfied by GTK3 itself: gdk_wayland_window_hide_surface() calls
//!   wl_surface_destroy() on hide, so every map has a FRESH wl_surface
//!   (and the old plasma surface died with the old wl_surface). Apply on
//!   every map, no dedup — pointer-based dedup would misfire when the
//!   allocator reuses an address for a genuinely new surface.

use std::cell::RefCell;

use glib::translate::ToGlibPtr;
use gtk::prelude::*;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols_plasma::plasma_shell::client::org_kde_plasma_shell::OrgKdePlasmaShell;
use wayland_protocols_plasma::plasma_shell::client::org_kde_plasma_surface::OrgKdePlasmaSurface;

// GDK-Wayland accessors — exported by libgdk-3 (already linked) but not
// wrapped by the gdk crate.
unsafe extern "C" {
    fn gdk_wayland_display_get_wl_display(
        display: *mut gdk::ffi::GdkDisplay,
    ) -> *mut std::ffi::c_void;
    fn gdk_wayland_window_get_wl_surface(window: *mut gdk::ffi::GdkWindow)
    -> *mut std::ffi::c_void;
}

struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

wayland_client::delegate_noop!(State: ignore OrgKdePlasmaShell);
wayland_client::delegate_noop!(State: ignore OrgKdePlasmaSurface);

struct PlasmaCtx {
    conn: Connection,
    queue: EventQueue<State>,
    shell: OrgKdePlasmaShell,
}

thread_local! {
    // GTK main thread only (map-event handler), so thread_local is enough.
    static PLASMA: RefCell<Option<PlasmaCtx>> = const { RefCell::new(None) };
    // True once the current map/unmap cycle's surface has been flagged.
    // GTK can emit map-event several times for one mapped surface (observed:
    // show() + present() → two maps ~1.5ms apart); a second get_surface on
    // the same wl_surface is a fatal protocol error. Reset by mark_unmapped().
    static FLAGGED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Call from the window's unmap-event: the wl_surface is being destroyed
/// (GTK3 destroys it on hide), so the next map needs a fresh plasma surface.
pub fn mark_unmapped() {
    FLAGGED.with(|f| f.set(false));
}

fn init_ctx(wl_display_ptr: *mut std::ffi::c_void) -> Result<PlasmaCtx, String> {
    // Wrap GDK's connection without taking ownership (foreign display).
    let backend =
        unsafe { wayland_backend::sys::client::Backend::from_foreign_display(wl_display_ptr as _) };
    let conn = Connection::from_backend(backend);

    let (globals, queue) =
        registry_queue_init::<State>(&conn).map_err(|e| format!("registry: {e}"))?;
    let qh = queue.handle();

    // set_skip_taskbar needs interface v2, set_skip_switcher v5.
    let shell: OrgKdePlasmaShell = globals
        .bind(&qh, 2..=8, ())
        .map_err(|e| format!("org_kde_plasma_shell not offered (not KWin?): {e}"))?;

    Ok(PlasmaCtx { conn, queue, shell })
}

/// Flag the window's Wayland surface as skip-taskbar + skip-switcher via
/// org_kde_plasma_shell. Call on every map: the shell is bound once per
/// process, and each map carries a fresh wl_surface (GTK3 destroys the old
/// one on hide) that needs its own plasma surface.
pub fn hide_from_taskbar(gtk_win: &gtk::ApplicationWindow) -> Result<(), String> {
    if FLAGGED.with(|f| f.get()) {
        // Repeat map-event for a surface we already flagged this cycle.
        return Ok(());
    }
    let gdk_window = gtk_win.window().ok_or("window not realized")?;
    let display = gdk_window.display();
    if display.type_().name() != "GdkWaylandDisplay" {
        return Err("not a Wayland display".into());
    }

    let (wl_display_ptr, wl_surface_ptr) = unsafe {
        let display_ptr: *mut gdk::ffi::GdkDisplay = display.to_glib_none().0;
        let window_ptr: *mut gdk::ffi::GdkWindow = gdk_window.to_glib_none().0;
        (
            gdk_wayland_display_get_wl_display(display_ptr),
            gdk_wayland_window_get_wl_surface(window_ptr),
        )
    };
    if wl_display_ptr.is_null() || wl_surface_ptr.is_null() {
        return Err("null wl_display/wl_surface from GDK".into());
    }

    PLASMA.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(init_ctx(wl_display_ptr)?);
        }
        let ctx = slot.as_mut().expect("just initialized");

        let surface_id = unsafe {
            wayland_client::backend::ObjectId::from_ptr(WlSurface::interface(), wl_surface_ptr as _)
        }
        .map_err(|e| format!("invalid wl_surface: {e}"))?;
        let wl_surface =
            WlSurface::from_id(&ctx.conn, surface_id).map_err(|e| format!("wl_surface: {e}"))?;

        let qh = ctx.queue.handle();
        let plasma_surface = ctx.shell.get_surface(&wl_surface, &qh, ());
        plasma_surface.set_skip_taskbar(1);
        if plasma_surface.version() >= 5 {
            plasma_surface.set_skip_switcher(1);
        }

        let mut state = State;
        ctx.queue
            .roundtrip(&mut state)
            .map_err(|e| format!("roundtrip: {e}"))?;

        FLAGGED.with(|f| f.set(true));

        // Dropping the plasma_surface proxy handle does not send a destroy
        // request — the flags stay active for the lifetime of the wl_surface,
        // which is exactly what we want.
        Ok(())
    })
}
