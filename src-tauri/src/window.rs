use std::sync::atomic::Ordering;

use tauri::{Emitter, Manager, WebviewWindow};

use crate::state::AppState;

/// Short thread identifier for debug logs.
fn tid() -> String {
    std::thread::current().name().unwrap_or("?").to_string()
}

/// Minimum gap between accepted toggle requests. Absorbs double-delivery of
/// one physical keypress (DE-bound `lychi --toggle` + the X11 shortcut plugin
/// both firing over XWayland windows, duplicate IPC lines, key autorepeat).
const TOGGLE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);

/// Toggle the launcher window — the single toggle authority.
///
/// All entry points (hotkey, IPC socket, tray, single-instance) route here.
/// Two correctness properties:
/// - **Debounce**: duplicate deliveries of one keypress are dropped.
/// - **Atomic decide+act**: the visible/hidden decision runs ON the GTK main
///   thread against the live widget state, and a hide executes inline there.
///   The old pattern (`is_visible()` round-trip from an arbitrary thread,
///   then a separately queued hide/show) interleaved under concurrency and
///   caused the "press the hotkey twice" bug.
pub fn toggle_window(window: &WebviewWindow) {
    static LAST_TOGGLE: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    {
        let mut last = match LAST_TOGGLE.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let now = std::time::Instant::now();
        if let Some(prev) = *last
            && now.duration_since(prev) < TOGGLE_DEBOUNCE
        {
            tracing::debug!("[toggle] debounced duplicate request");
            return;
        }
        *last = Some(now);
    }

    let win = window.clone();
    glib::MainContext::default().invoke(move || {
        // Ask the state machine, do not poll GTK. `gtk_window.is_visible()` is a
        // widget flag that changes asynchronously with the compositor, so it is
        // transiently wrong on Wayland — and when the dismiss path polled it
        // too, the two disagreed microseconds apart:
        //
        //   [toggle]  decision on GTK thread: visible=true
        //   [dismiss] focus-out ... visible=false
        //
        // Decide and act under ONE lock on the GTK thread, which is the
        // property the original inline hide was protecting (see the "press the
        // hotkey twice" note above); the state machine preserves it rather than
        // reintroducing a read-then-act round-trip.
        let state = win.app_handle().state::<AppState>();
        let action = state
            .launcher
            .apply(crate::launcher_state::Event::ToggleRequested, "toggle");
        if matches!(action, crate::launcher_state::Action::Hide) {
            state.dismiss_armed.store(false, Ordering::SeqCst);
            // On the GTK thread this executes inline — the widget flag is
            // updated synchronously, so a follow-up toggle decides correctly.
            let _ = win.hide();
            state
                .launcher
                .apply(crate::launcher_state::Event::HideCompleted, "toggle-hide");
        } else {
            // show_window does blocking context work (KWin D-Bus snapshot) —
            // it must not run on the GTK thread. Hand it to a worker; its own
            // GTK closure does the actual map.
            std::thread::spawn(move || show_window(&win));
        }
    });
}

/// Show the window, reposition to the correct monitor, focus it, and notify
/// the frontend. Public so lib.rs can call it at startup too.
pub fn show_window(window: &WebviewWindow) {
    // This function must NEVER run on a tokio async worker: the blocking_read()
    // below panics in an async execution context. (A comment here used to
    // claim the exact opposite — "safe because callers are always on Tokio
    // tasks" — and that inverted rule shipped the `--ai` IPC path calling
    // this from its async handler, where the panic silently killed the task
    // and the launcher never appeared.) Async callers hop first, the way
    // toggle_window and the IPC server do.
    lychi_core::events::debug_assert_blocking_legal("show_window");

    // Read monitor_mode from config.
    let monitor_mode = {
        let state = window.app_handle().state::<AppState>();
        state.config.blocking_read().general.monitor_mode.clone()
    };

    // Increment summon sequence — makes stale focus events from previous
    // summon cycles harmless (focus handlers compare against current seq).
    let seq = {
        let state = window.app_handle().state::<AppState>();
        state.summon_seq.fetch_add(1, Ordering::SeqCst) + 1
    };
    tracing::info!("[show] === show_window BEGIN (seq={seq}, t={}) ===", tid());
    // Record the show even when it did not come from a toggle (startup, tray,
    // `--show`), so the machine is never left in Hidden while a window is up.
    // Idempotent for the toggle path, which has already moved to Showing.
    {
        let state = window.app_handle().state::<AppState>();
        state.launcher.apply(
            crate::launcher_state::Event::ShowRequested,
            &format!("show_window seq={seq}"),
        );
    }

    // Reset dismiss armed — will be re-armed by user interaction (key/click).
    {
        let state = window.app_handle().state::<AppState>();
        state.dismiss_armed.store(false, Ordering::SeqCst);
    }
    tracing::info!("[show] seq={seq} armed=false");

    // Snapshot the active window BEFORE we show Lychi (otherwise we'd detect ourselves).
    let pre_window = lychi_core::context::snapshot_active_window();

    // Seed the focus ring with the pre-summon window if it's a terminal.
    // This handles cold-start: if the user summoned Lychi from a terminal,
    // the ring is immediately non-empty and the next gather() hits it.
    if let Some(ref w) = pre_window
        && w.is_terminal
    {
        lychi_core::context::window_stack::push_focus_entry_pre_summon(w.clone());
    }

    // Clear frontend state before the window becomes visible so there is no
    // stale-completions flash. The WebView is already loaded (just hidden) and
    // can process the event before the compositor paints the first frame.
    tracing::info!("[show] seq={seq} emitting lychi://summon (pre-show)");
    let _ = window.emit("lychi://summon", ());

    // Reposition + show + focus all run on the GLib main thread in one invoke.
    {
        let window_clone = window.clone();
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        glib::MainContext::default().invoke(move || {
            let t = std::thread::current().name().unwrap_or("?").to_string();

            tracing::info!("[show] seq={seq} t={t} reposition BEGIN");
            if let Some(monitor) = crate::platform::get_monitor_for_mode(&monitor_mode) {
                crate::platform::reposition_to_monitor(&window_clone, &monitor);
            }
            tracing::info!("[show] seq={seq} t={t} reposition END");

            tracing::info!("[show] seq={seq} t={t} window.show()");
            if let Err(e) = window_clone.show() {
                tracing::error!("Failed to show window: {e}");
                let _ = tx.send(false);
                return;
            }

            tracing::info!("[show] seq={seq} t={t} focus_window()");
            crate::platform::focus_window(&window_clone);

            tracing::info!("[show] seq={seq} t={t} set_focus()");
            if let Err(e) = window_clone.set_focus() {
                tracing::error!("Failed to focus window: {e}");
            }

            // Ready signal: the surface is mapped — tell the frontend to drop
            // `.not-ready` (opacity 0 + pointer-events none). Without this, a
            // lost pre-show summon leaves an INVISIBLE monitor-covering
            // surface that eats every desktop click. Re-emit once as a
            // watchdog: `lychi://shown` is idempotent (only sets ready=true),
            // unlike summon which clears input state.
            let _ = window_clone.emit("lychi://shown", ());
            let watchdog = window_clone.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                let _ = watchdog.emit("lychi://shown", ());
            });

            let _ = tx.send(true);
        });
        let ok = rx.recv().unwrap_or(false);
        if !ok {
            return;
        }
    }

    tracing::info!("[show] === show_window END (seq={seq}) ===");

    // Fast path: emit last-known context immediately so suggestions appear
    // before the fresh gather completes (typically saves 50-200ms).
    // Skip if the active window changed — stale context causes a suggestion flash.
    {
        let state = window.app_handle().state::<AppState>();
        if let Ok(executor) = state.executor.try_read()
            && let Some(ref cached_ctx) = executor.context
        {
            let same_window = match (&pre_window, &cached_ctx.active_window) {
                (Some(pre), Some(cached)) => {
                    // Use window_id (UUID) when available — distinguishes two windows
                    // of the same app (e.g. VS Code with different projects open).
                    match (&pre.window_id, &cached.window_id) {
                        (Some(a), Some(b)) => a == b,
                        _ => pre.wm_class == cached.wm_class,
                    }
                }
                (None, None) => true,
                _ => false,
            };
            if same_window {
                tracing::debug!(
                    "Fast path: emitting cached context ({}ms old)",
                    cached_ctx.gather_ms
                );
                let _ = window.emit("lychi://context-ready", cached_ctx);
            } else {
                tracing::debug!("Fast path: skipped (window changed)");
            }
        }
    }

    // Gather fresh context asynchronously (never blocks summon).
    // Result replaces the cached context and is re-emitted to frontend.
    // Latest-wins: if a newer summon started while we were gathering, discard.
    let ctx_handle = window.app_handle().clone();
    let gather_seq = seq;
    tauri::async_runtime::spawn_blocking(move || {
        let ctx = lychi_core::context::gather(pre_window);

        // Latest-wins: discard if a newer summon has started
        let current_seq = {
            let state = ctx_handle.state::<AppState>();
            state.summon_seq.load(Ordering::SeqCst)
        };
        if gather_seq < current_seq {
            tracing::debug!(
                "Context gathered in {}ms but discarded (seq={}, current={})",
                ctx.gather_ms,
                gather_seq,
                current_seq
            );
            return;
        }

        tracing::info!(
            "Context gathered in {}ms: window={}, cwd={}, terminal_cwd={}, git={}, project={}, docker={}",
            ctx.gather_ms,
            ctx.active_window
                .as_ref()
                .map(|w| w.wm_class.as_str())
                .unwrap_or("none"),
            ctx.cwd.as_deref().unwrap_or("none"),
            ctx.terminal_cwd.as_deref().unwrap_or("none"),
            ctx.git
                .as_ref()
                .map(|g| g.branch.as_str())
                .unwrap_or("none"),
            ctx.project
                .as_ref()
                .map(|p| format!("{:?}", p.kind))
                .unwrap_or_else(|| "none".into()),
            ctx.docker
                .as_ref()
                .map(|d| d.containers.len().to_string())
                .unwrap_or_else(|| "none".into()),
        );

        // Store in executor
        let state = ctx_handle.state::<AppState>();
        if let Ok(mut executor) = state.executor.try_write() {
            executor.context = Some(ctx.clone());
        }

        // Notify frontend that context is ready
        let _ = ctx_handle.emit("lychi://context-ready", &ctx);
    });
}
