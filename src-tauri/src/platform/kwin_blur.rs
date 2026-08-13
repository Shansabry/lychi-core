//! Frosted-glass background blur via the KWin `org_kde_kwin_blur` Wayland
//! protocol — the ONLY way to blur the DESKTOP behind a translucent launcher.
//!
//! CSS `backdrop-filter` cannot do this: it samples only content within the
//! webview's own document, never past the surface to the compositor. Real
//! desktop blur is the compositor's job, opted into per-surface via this
//! protocol (the same path GTK terminals use for KWin blur).
//!
//! We blur the WHOLE surface (`set_region(None)`) rather than a rect: the card
//! fills the window and the transparent margins have nothing behind them to
//! blur, so a region buys only resize bookkeeping for no visible gain. The blur
//! STRENGTH is a global KWin setting the client can't set — we only toggle blur
//! on/off, which is why the UI exposes a "frosted glass" toggle, not a radius.
//!
//! Mirrors `kde_taskbar.rs`: reach the `wl_surface` through GDK, bind the plasma
//! blur manager once per process, and (re)apply on every map — GTK3 destroys the
//! wl_surface on hide, so each map carries a fresh one that needs blur set again.

use std::cell::RefCell;

use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur::OrgKdeKwinBlur;
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur_manager::OrgKdeKwinBlurManager;

// GDK-Wayland accessors — exported by libgdk-3 (already linked) but not wrapped
// by the gdk crate.
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

wayland_client::delegate_noop!(State: ignore OrgKdeKwinBlurManager);
wayland_client::delegate_noop!(State: ignore OrgKdeKwinBlur);

struct BlurCtx {
    conn: Connection,
    queue: EventQueue<State>,
    manager: OrgKdeKwinBlurManager,
}

thread_local! {
    // GTK main thread only (map-event handler), so thread_local is enough.
    static BLUR: RefCell<Option<BlurCtx>> = const { RefCell::new(None) };
    // Whether blur is enabled (the user's toggle). Applied on every map.
    static ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    // True once this map/unmap cycle's surface has been handled — GTK can emit
    // map-event twice for one surface (show()+present()); a second create on the
    // same surface is a protocol error. Reset by mark_unmapped().
    static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The window's wl_surface is being destroyed (GTK3 destroys it on hide), so the
/// next map needs blur re-applied. Call from the window's unmap-event.
pub fn mark_unmapped() {
    DONE.with(|f| f.set(false));
}

/// Set whether the launcher should be blurred. Persisted by the caller; the
/// actual protocol call happens on the next `apply_on_map`. If the window is
/// already mapped, we re-apply immediately via `gtk_win`.
pub fn set_enabled(enabled: bool, gtk_win: &gtk::ApplicationWindow) {
    ENABLED.with(|e| e.set(enabled));
    // Force a re-apply against the current surface: clear the once-guard and run.
    DONE.with(|f| f.set(false));
    if let Err(e) = apply_on_map(gtk_win) {
        tracing::debug!("[blur] set_enabled apply skipped: {e}");
    }
}

fn init_ctx(wl_display_ptr: *mut std::ffi::c_void) -> Result<BlurCtx, String> {
    let backend =
        unsafe { wayland_backend::sys::client::Backend::from_foreign_display(wl_display_ptr as _) };
    let conn = Connection::from_backend(backend);
    let (globals, queue) =
        registry_queue_init::<State>(&conn).map_err(|e| format!("registry: {e}"))?;
    let qh = queue.handle();
    let manager: OrgKdeKwinBlurManager = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| format!("org_kde_kwin_blur_manager not offered (blur unsupported): {e}"))?;
    Ok(BlurCtx {
        conn,
        queue,
        manager,
    })
}

/// Apply (or clear) blur on the window's current Wayland surface. Idempotent per
/// map cycle. Returns Err when blur can't be applied (not Wayland/KWin, surface
/// not realized, manager not offered) — the caller treats that as "no real blur,
/// the CSS fallback stands in".
pub fn apply_on_map(gtk_win: &gtk::ApplicationWindow) -> Result<(), String> {
    if DONE.with(|f| f.get()) {
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

    BLUR.with(|cell| {
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
        if ENABLED.with(|e| e.get()) {
            // Blur the whole surface: no region = the entire surface is blurred.
            let blur = ctx.manager.create(&wl_surface, &qh, ());
            blur.set_region(None);
            blur.commit();
        } else {
            // Remove any blur previously set on this surface.
            ctx.manager.unset(&wl_surface);
        }
        // A wl_surface.commit is needed for the compositor to pick up the change.
        wl_surface.commit();

        let mut state = State;
        ctx.queue
            .roundtrip(&mut state)
            .map_err(|e| format!("roundtrip: {e}"))?;

        DONE.with(|f| f.set(true));
        Ok(())
    })
}
