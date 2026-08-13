use std::sync::atomic::Ordering;

use tauri::{Emitter, Manager, WebviewWindow};

use crate::state::AppState;

/// Short thread identifier for debug logs.
fn tid() -> String {
    std::thread::current().name().unwrap_or("?").to_string()
}

/// Update the tray's toggle item so its label matches the window: "Hide" while
/// the launcher is open, "Show" while it's closed. The item always TOGGLED
/// correctly; only its text was static ("Show" even when open). Called from the
/// show and hide paths. A no-op when the tray failed to build (item is `None`)
/// or its text can't be set — the tray is best-effort, never load-bearing.
pub fn update_tray_label(app: &tauri::AppHandle) {
    use crate::launcher_state::LauncherState;
    let state = app.state::<AppState>();
    let open = matches!(
        state.launcher.get(),
        LauncherState::Visible | LauncherState::Showing
    );
    let label = if open { "Hide" } else { "Show" };
    if let Ok(guard) = state.tray_toggle_item.lock()
        && let Some(item) = guard.as_ref()
    {
        let _ = item.set_text(label);
    }
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
            // Route the hide through the FRONTEND so it can blank + let one paint
            // land before the surface unmaps (the re-summon-flash fix — see
            // `blankThenHide` in +page.svelte). The frontend then calls the
            // `hide_launcher` command, which does the real `win.hide()` and fires
            // `HideCompleted`. We still flip the state machine to Hiding here (it
            // already did on ToggleRequested), so a rapid follow-up toggle can't
            // race a half-applied hide. A watchdog fallback hides directly if the
            // frontend doesn't ack within a short budget, so a wedged webview can
            // never leave the launcher stuck open.
            let _ = win.emit("lychi://request-hide", ());
            let watchdog = win.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
                use crate::launcher_state::LauncherState;
                let st = watchdog.app_handle().state::<AppState>();
                // Only fire if the frontend hasn't already completed the hide.
                if matches!(st.launcher.get(), LauncherState::Hiding) {
                    tracing::warn!("[toggle] frontend hide not acked in 120ms — hiding directly");
                    let _ = watchdog.hide();
                    st.launcher.apply(
                        crate::launcher_state::Event::HideCompleted,
                        "toggle-hide-watchdog",
                    );
                    update_tray_label(watchdog.app_handle());
                }
            });
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
    update_tray_label(window.app_handle());

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

        // Store in executor. A blocking acquire, not `try_write`: the summon
        // that triggered this gather also fires an immediate `get_completions`,
        // whose read lock is usually still held when the gather lands — with
        // `try_write` the fresh context was silently dropped exactly when it
        // mattered, and the launcher kept suggesting from the PREVIOUS
        // summon's world. We are on a blocking thread; waiting is what it's
        // for, and tokio's write-preferring lock bounds the wait to the
        // in-flight readers.
        let state = ctx_handle.state::<AppState>();
        let stored = {
            let mut executor = tauri::async_runtime::block_on(state.executor.write());
            // Re-check AFTER acquiring: a newer summon may have started while
            // we waited, and its gather must win (latest-wins).
            if state.summon_seq.load(Ordering::SeqCst) == gather_seq {
                executor.context = Some(ctx.clone());
                true
            } else {
                false
            }
        };

        // Notify frontend that context is ready — only when the executor
        // actually holds it. Emitting on a discarded store made the frontend
        // re-render "fresh" suggestions from context the backend didn't have.
        if stored {
            let _ = ctx_handle.emit("lychi://context-ready", &ctx);
        } else {
            tracing::debug!("context store skipped (seq={gather_seq}, newer summon in flight)");
        }
    });
}
