//! The single owner of "is the launcher open?".
//!
//! # Why this exists
//!
//! Nothing used to own that question. Three subsystems each answered it by
//! polling GTK — `toggle_window` via `gtk_window.is_visible()`, the focus-out
//! handler via `w.is_visible()`/`is_active()`, and `hide_launcher` by assuming
//! it was open. `is_visible()` is a widget flag, not launcher state: on Wayland
//! mapping is a compositor round-trip, so the flag is transiently wrong *by
//! design*. Three deciders polling a racy flag disagree, and they did —
//! measured microseconds apart in one session:
//!
//! ```text
//! [toggle]  decision on GTK thread: visible=true
//! [dismiss] focus-out ... visible=false
//! ```
//!
//! The user-visible bug was "the launcher vanishes while I type", reported on
//! GNOME and then reproduced locally on KDE at `keys=6`. Its cause is a
//! focus-out event that GTK delivers *while still reporting the window as
//! focused*:
//!
//! ```text
//! focus-out → DISMISS  keys=6 is_active=true toplevel_focus=true visible=true
//! ```
//!
//! The event is not evidence that focus was lost.
//!
//! # Why not just ignore those events
//!
//! That was tried: "ignore focus-out when `is_active()` is still true". It
//! broke summoning in one session — with focus-out always ignored the window
//! never reached a hidden state, so the next hotkey press *hid* it instead of
//! showing it. It is also the wrong shape: a heuristic that re-derives
//! information the system already had, layered on `dismiss_armed`, which was
//! itself a patch over this missing state machine. `dismiss_armed` never made
//! focus-out trustworthy — it only postponed the first spurious dismiss until
//! after the first keystroke, which is exactly why the bug looked like "it
//! closes when I type".
//!
//! # The model
//!
//! Events are INPUTS, not decisions. Two things have to be true together, and
//! neither works alone:
//!
//! 1. **This machine owns the state.** Nothing else polls GTK to decide whether
//!    the launcher is open, so `toggle` and the dismiss path can no longer
//!    disagree.
//! 2. **The focus-out event carries whether focus was actually lost.** The
//!    caller samples `is_active()`/`has_toplevel_focus()` at event time and
//!    passes it in as `FocusOut { focus_lost }`.
//!
//! Point 2 is not inferable from state, and an earlier draft that tried was
//! wrong: measurement shows focus-in reliably arrives ~111ms BEFORE the
//! spurious focus-out, so those events land in `Visible`, not `Showing`. See
//! [`Event::FocusOut`] for the trace.
//!
//! Point 1 is what the standalone predicate lacked. Ignoring spurious
//! focus-outs without owning the state left the window stuck in a "visible"
//! state that `toggle` then inverted — the hotkey hid an already-hidden window
//! and the launcher stopped summoning entirely.

use std::fmt;

/// Where the launcher window is in its show/hide lifecycle.
///
/// The in-flight states are the point: a bool cannot distinguish "mapped and
/// focused" from "asked to map, still settling", and that distinction is
/// precisely what makes a focus-out interpretable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherState {
    /// Not mapped.
    Hidden,
    /// Show requested; mapping and focus still settling. Focus-out here is
    /// expected compositor churn, never a dismiss.
    Showing,
    /// Mapped AND focus confirmed. The only state in which focus-out means the
    /// user actually left.
    Visible,
    /// Hide requested; unmap in flight.
    Hiding,
}

impl fmt::Display for LauncherState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Hidden => "Hidden",
            Self::Showing => "Showing",
            Self::Visible => "Visible",
            Self::Hiding => "Hiding",
        })
    }
}

/// What happened. Named for the event, not the intended outcome — deciding the
/// outcome is this module's job, and naming an input after a result is how
/// callers start making the decision themselves again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Hotkey, tray, IPC, or single-instance activation.
    ToggleRequested,
    /// An explicit show that is not a toggle (startup, `--show`).
    ShowRequested,
    /// An explicit hide (Escape, frontend request, `hide_launcher`).
    ///
    /// Currently `hide_launcher` reports [`Self::HideCompleted`] directly
    /// because the hide is synchronous there. This variant stays because the
    /// transition table must still define what a *requested* hide does from
    /// every state — deleting it would silently make that undefined the moment
    /// an async hide path appears.
    #[cfg_attr(not(test), allow(dead_code))]
    HideRequested,
    /// GTK `focus-in-event`.
    FocusIn,
    /// GTK `focus-out-event`, carrying whether the window ACTUALLY lost focus.
    ///
    /// The flag is the whole fix, and it is here rather than inferred from the
    /// state because of a measurement. The event order on a real session is
    /// invariably:
    ///
    /// ```text
    /// show_window BEGIN (seq=8)
    ///   +111ms  focus-IN #9      <- focus IS established
    ///   +258ms  focus-out        <- the spurious event arrives AFTER
    /// ```
    ///
    /// An earlier draft assumed spurious events land while still `Showing` and
    /// could be ignored by state alone. They do not: focus-in reliably precedes
    /// them, so that design would have dismissed on every one. Nine unit tests
    /// passed against that wrong premise — an internally coherent model proves
    /// nothing about the compositor.
    ///
    /// The caller derives this from `GdkWindowState::FOCUSED`, which GDK sets
    /// from the Wayland `wl_keyboard` enter/leave events — the protocol's own
    /// notion of keyboard focus. Two GTK-level properties were tried first and
    /// are both wrong here; measured on KDE Wayland they are exactly inverted:
    ///
    /// ```text
    /// FOCUS-IN   is_active=false toplevel_focus=false  FOCUSED=true
    /// FOCUS-OUT  is_active=true  toplevel_focus=true   FOCUSED=false
    /// ```
    ///
    /// `is_active()`/`has_toplevel_focus()` lag by one event on Wayland, so at
    /// focus-out time they still describe the previous state — a predicate on
    /// them reads `true` on every focus-out including a genuine click-away, and
    /// silently disables dismiss-on-blur instead of fixing anything.
    ///
    /// `interacted` is whether the user has typed or clicked in the window this
    /// summon cycle. `focus_lost` alone is not enough: it distinguishes
    /// protocol-real focus loss from GTK noise, but not the user leaving from
    /// focus being TAKEN before the user ever touched the window. Measured on
    /// GNOME/Mutter (tester log, 2026-08-10): show → focus-in at +87ms →
    /// focus-out with `FOCUSED` genuinely cleared at +560ms, `keys=0` — and the
    /// launcher dismissed itself before it could be used. What took focus is
    /// unidentified (NOT the shortcut-approval dialog: the bind response had
    /// already returned ~340ms earlier, far too fast for a human to have had a
    /// dialog open). The gate deliberately does not depend on knowing the
    /// thief: a focus loss the user had no hand in must leave the window up,
    /// whoever won it — the user can see who did and click back.
    FocusOut { focus_lost: bool, interacted: bool },
    /// The hide completed — the window is unmapped.
    HideCompleted,
}

/// What the caller should DO. Returned rather than performed, so the state
/// machine stays free of GTK and testable without a display server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Map, position, and focus the window.
    Show,
    /// Unmap the window.
    Hide,
    /// Tell the frontend the launcher was dismissed by focus loss.
    EmitDismiss,
    /// Deliberately do nothing.
    Nothing,
}

/// Applies an event to the current state, returning the new state and the
/// action to take.
///
/// Pure: no locks, no GTK, no I/O — so every transition below is unit-testable,
/// including the ones that only occur under a compositor race.
pub fn transition(state: LauncherState, event: Event) -> (LauncherState, Action) {
    use Action::*;
    use Event::*;
    use LauncherState::*;

    match (state, event) {
        // ---- Showing -------------------------------------------------------
        (Hidden | Hiding, ToggleRequested | ShowRequested) => (Showing, Show),

        // Focus arrived: the window is genuinely up. Only now can a focus-out
        // mean the user left.
        (Showing, FocusIn) => (Visible, Nothing),

        // Focus was never established in this state, so it cannot have been
        // lost — regardless of what the flag says.
        (Showing, FocusOut { .. }) => (Showing, Nothing),

        // ---- Visible -------------------------------------------------------
        // THE FIX, and the only dismissing transition in the machine. Both
        // flags must hold; each covers the other's blind spot.
        //
        // `focus_lost` filters GTK noise — a focus-out where GTK still reports
        // the window focused is the compositor's churn, not the user leaving.
        // Measured on the real dismiss that closed the launcher at keys=6:
        //
        //   focus-out → DISMISS  is_active=true toplevel_focus=true visible=true
        //
        // `interacted` filters focus THEFT — a protocol-real focus loss the
        // user had no hand in. Measured on GNOME/Mutter: something (still
        // unidentified) took focus 560ms after show at keys=0, and the
        // launcher dismissed itself on arrival. See [`Event::FocusOut`].
        //
        // Ignoring events here is necessary but NOT sufficient on its own:
        // doing it without owning the state left the window stuck in a
        // "visible" state that toggle then inverted, so the hotkey hid an
        // already-hidden window and the launcher stopped summoning. The
        // predicate needs this machine; this machine needs the predicate.
        (
            Visible,
            FocusOut {
                focus_lost: true,
                interacted: true,
            },
        ) => (Hiding, EmitDismiss),
        (Visible, FocusOut { .. }) => (Visible, Nothing),

        // A second toggle while open means "close it".
        (Visible | Showing, ToggleRequested) => (Hiding, Hide),
        (Visible | Showing, HideRequested) => (Hiding, Hide),

        // Redundant focus-in (re-focus without an intervening focus-out).
        (Visible, FocusIn) => (Visible, Nothing),

        // ---- Hiding --------------------------------------------------------
        (Hiding, HideCompleted) => (Hidden, Nothing),

        // Focus events during unmap are noise from a window on its way out.
        (Hiding, FocusIn | FocusOut { .. }) => (Hiding, Nothing),

        // ---- Hidden --------------------------------------------------------
        // Stale focus events for an unmapped window.
        (Hidden, FocusIn | FocusOut { .. }) => (Hidden, Nothing),
        (Hidden, HideRequested | HideCompleted) => (Hidden, Nothing),

        // ---- Remaining pairs, each decided rather than defaulted -----------
        // The compiler required these three explicitly, which is the point of
        // modelling the transition as a total function: a wildcard arm would
        // have silently picked a behaviour for cases nobody thought about.

        // Show requested for a window already up or on its way up: idempotent.
        // Re-showing would re-run positioning and re-trigger the focus churn
        // this machine exists to absorb.
        (Showing | Visible, ShowRequested) => (state, Nothing),

        // Hide requested while a hide is already in flight.
        (Hiding, HideRequested) => (Hiding, Nothing),

        // A hide that completes from a non-Hiding state (compositor unmapped
        // us, or a hide raced a show) — trust the fact and record Hidden.
        (Showing, HideCompleted) => (Hidden, Nothing),
        (Visible, HideCompleted) => (Hidden, Nothing),
    }
}

/// Shared, log-on-every-transition wrapper.
///
/// Every transition is traced, including the ones that change nothing: a
/// focus-out that was correctly ignored is exactly as diagnostic as one that
/// dismissed, and the whole reason this bug took so long to pin down was that
/// the ignored ones were invisible.
#[derive(Debug)]
pub struct LauncherStateMachine {
    state: std::sync::Mutex<LauncherState>,
}

impl Default for LauncherStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl LauncherStateMachine {
    pub fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(LauncherState::Hidden),
        }
    }

    /// Current state. Read-only — callers must go through [`Self::apply`] to
    /// change it, which is what keeps this the single decider.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn get(&self) -> LauncherState {
        match self.state.lock() {
            Ok(g) => *g,
            Err(p) => *p.into_inner(),
        }
    }

    /// Apply an event and return the action to take.
    ///
    /// Decide and transition happen under ONE lock. `toggle_window`'s doc
    /// records why that matters: reading the state on one thread and acting on
    /// it later is the pattern that caused the "press the hotkey twice" bug.
    /// Callers get an `Action` back precisely so they cannot re-derive the
    /// decision themselves.
    pub fn apply(&self, event: Event, ctx: &str) -> Action {
        let mut guard = match self.state.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let before = *guard;
        let (after, action) = transition(before, event);
        *guard = after;
        drop(guard);

        if before == after && matches!(action, Action::Nothing) {
            tracing::debug!("[state] {before} --{event:?}--> (no change)  {ctx}");
        } else {
            tracing::info!("[state] {before} --{event:?}--> {after} : {action:?}  {ctx}");
        }
        action
    }
}

#[cfg(test)]
mod tests {
    use super::{Action::*, Event::*, LauncherState::*, *};

    /// THE REPORTED BUG, in the order it actually happens.
    ///
    /// This is the regression test that an earlier design would have failed.
    /// That design assumed the spurious focus-out arrives while `Showing`; the
    /// log says otherwise — focus-in lands ~111ms after show, and the spurious
    /// focus-out ~258ms after that, i.e. in `Visible`. So the sequence below is
    /// the real one, and only the `focus_lost` flag saves it.
    #[test]
    fn spurious_focus_out_after_focus_in_does_not_dismiss() {
        let (s, _) = transition(Hidden, ToggleRequested);
        let (s, _) = transition(s, FocusIn); // ~111ms later — now Visible
        assert_eq!(s, Visible);
        // GTK says focus-out but still reports the window focused.
        let (s, a) = transition(
            s,
            FocusOut {
                focus_lost: false,
                interacted: true,
            },
        );
        assert_eq!(
            (s, a),
            (Visible, Nothing),
            "spurious focus-out in Visible must not dismiss — this is the bug"
        );
    }

    /// The guard against "fixing" the bug by disabling dismiss entirely, which
    /// is how the first attempt passed a casual check and still shipped broken.
    #[test]
    fn genuine_focus_loss_while_visible_still_dismisses() {
        let (s, a) = transition(
            Visible,
            FocusOut {
                focus_lost: true,
                interacted: true,
            },
        );
        assert_eq!((s, a), (Hiding, EmitDismiss));
    }

    /// THE GNOME FOCUS-THEFT BUG (tester report, 2026-08-10), in the order the
    /// log records it: show → focus-in → a protocol-real focus-out at keys=0,
    /// 560ms in — something stole focus before the user ever touched the
    /// window (what, exactly, is unidentified — see [`Event::FocusOut`]). The
    /// launcher must stay up: dismissing on a focus loss the user had no hand
    /// in is what made every summon flash and vanish.
    #[test]
    fn focus_theft_before_any_interaction_does_not_dismiss() {
        let (s, _) = transition(Hidden, ShowRequested);
        let (s, _) = transition(s, FocusIn);
        assert_eq!(s, Visible);
        let (s, a) = transition(
            s,
            FocusOut {
                focus_lost: true,
                interacted: false,
            },
        );
        assert_eq!(
            (s, a),
            (Visible, Nothing),
            "unarmed focus theft must not dismiss — this is the GNOME bug"
        );
        // The user notices, clicks the launcher back, types, then leaves.
        let (s, _) = transition(s, FocusIn);
        let (s, a) = transition(
            s,
            FocusOut {
                focus_lost: true,
                interacted: true,
            },
        );
        assert_eq!(
            (s, a),
            (Hiding, EmitDismiss),
            "interacted dismiss still works"
        );
    }

    /// Summon → focus settles → the user genuinely leaves.
    #[test]
    fn full_show_focus_dismiss_cycle() {
        let (s, a) = transition(Hidden, ToggleRequested);
        assert_eq!((s, a), (Showing, Show));
        let (s, a) = transition(s, FocusIn);
        assert_eq!((s, a), (Visible, Nothing));
        let (s, a) = transition(
            s,
            FocusOut {
                focus_lost: true,
                interacted: true,
            },
        );
        assert_eq!((s, a), (Hiding, EmitDismiss));
        let (s, a) = transition(s, HideCompleted);
        assert_eq!((s, a), (Hidden, Nothing));
    }

    /// Typing does not close the launcher: many spurious blurs interleaved with
    /// a genuine one at the end. Mirrors the measured session (keys=6).
    #[test]
    fn typing_survives_repeated_spurious_blurs_then_dismisses_for_real() {
        let (mut s, _) = transition(Hidden, ToggleRequested);
        let (next, _) = transition(s, FocusIn);
        s = next;
        for _ in 0..6 {
            let (next, a) = transition(
                s,
                FocusOut {
                    focus_lost: false,
                    interacted: true,
                },
            );
            assert_eq!(a, Nothing, "a keystroke-adjacent blur must not dismiss");
            s = next;
        }
        assert_eq!(s, Visible, "still open after six spurious blurs");
        let (s, a) = transition(
            s,
            FocusOut {
                focus_lost: true,
                interacted: true,
            },
        );
        assert_eq!((s, a), (Hiding, EmitDismiss), "real focus loss still works");
    }

    /// Toggling twice must land back at Hidden, not drift — the "press the
    /// hotkey twice" regression.
    #[test]
    fn toggle_twice_returns_to_hidden() {
        let (s, _) = transition(Hidden, ToggleRequested);
        let (s, _) = transition(s, FocusIn);
        let (s, a) = transition(s, ToggleRequested);
        assert_eq!((s, a), (Hiding, Hide));
        let (s, _) = transition(s, HideCompleted);
        assert_eq!(s, Hidden);
    }

    /// A toggle arriving before focus settles still closes the window, rather
    /// than being swallowed and leaving it open.
    #[test]
    fn toggle_while_still_showing_hides() {
        let (s, a) = transition(Showing, ToggleRequested);
        assert_eq!((s, a), (Hiding, Hide));
    }

    /// Repeated spurious focus-outs must not accumulate into a dismiss.
    #[test]
    fn many_spurious_focus_outs_never_dismiss() {
        let mut s = Showing;
        for _ in 0..50 {
            let (next, a) = transition(
                s,
                FocusOut {
                    focus_lost: false,
                    interacted: true,
                },
            );
            assert_eq!(a, Nothing);
            s = next;
        }
        assert_eq!(s, Showing);
    }

    /// Stale focus events for an unmapped window are inert.
    #[test]
    fn focus_events_while_hidden_are_inert() {
        assert_eq!(
            transition(
                Hidden,
                FocusOut {
                    focus_lost: true,
                    interacted: true,
                },
            ),
            (Hidden, Nothing)
        );
        assert_eq!(transition(Hidden, FocusIn), (Hidden, Nothing));
    }

    /// Every (state, event) pair is handled — no panic, no silent fallthrough.
    #[test]
    fn transition_is_total() {
        for s in [Hidden, Showing, Visible, Hiding] {
            for e in [
                ToggleRequested,
                ShowRequested,
                HideRequested,
                FocusIn,
                FocusOut {
                    focus_lost: true,
                    interacted: true,
                },
                FocusOut {
                    focus_lost: true,
                    interacted: false,
                },
                FocusOut {
                    focus_lost: false,
                    interacted: true,
                },
                FocusOut {
                    focus_lost: false,
                    interacted: false,
                },
                HideCompleted,
            ] {
                let _ = transition(s, e);
            }
        }
    }

    /// The machine logs and mutates consistently through a realistic sequence.
    #[test]
    fn machine_tracks_state_across_events() {
        let m = LauncherStateMachine::new();
        assert_eq!(m.get(), Hidden);
        assert_eq!(m.apply(ToggleRequested, "test"), Show);
        assert_eq!(m.get(), Showing);
        // The bug: keystrokes + spurious blur while showing.
        assert_eq!(
            m.apply(
                FocusOut {
                    focus_lost: false,
                    interacted: true,
                },
                "test"
            ),
            Nothing
        );
        assert_eq!(m.get(), Showing);
        assert_eq!(m.apply(FocusIn, "test"), Nothing);
        assert_eq!(m.get(), Visible);
        assert_eq!(
            m.apply(
                FocusOut {
                    focus_lost: true,
                    interacted: true,
                },
                "test"
            ),
            EmitDismiss
        );
        assert_eq!(m.get(), Hiding);
    }
}
