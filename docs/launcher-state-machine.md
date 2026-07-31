# Launcher visibility: one owner, event-driven

Status: proposed (2026-07-31). Replaces the `dismiss_armed` / `armed_seq` /
`summon_seq` trio and the ad-hoc `is_visible()` polling.

## The bug this exists to fix

A tester on GNOME/Wayland reported the launcher "crashing" when he typed. It had
not crashed — it dismissed itself. Then it reproduced locally on KDE with
`keys=6`, so it is not one compositor misbehaving.

The measured cause, from the instrumented log:

```
[dismiss] seq=4 focus-out (armed, current cycle) → DISMISS
          blur#4 keys=6 up=37401ms is_active=true toplevel_focus=true visible=true
```

GTK delivered a **focus-out event while simultaneously reporting the window as
focused** (`is_active=true toplevel_focus=true`). The event is not evidence that
focus was lost.

## Why the obvious patch is wrong

The tempting fix — "ignore focus-out when `is_active()` is still true" — was
tried and broke summoning within one session:

```
[toggle]  decision on GTK thread: visible=true      ← toggle polls GTK
[dismiss] focus-out ... visible=false               ← same window, same instant
```

With focus-out always ignored, the window never reached a hidden state, so the
next hotkey press *hid* it instead of showing it. The launcher stopped
summoning.

That patch is also the wrong shape. It is a heuristic that re-derives
information the system already had — the tell that a decision was discarded
upstream. It papers over the actual defect.

## The actual defect: three deciders, no owner

Nothing owns the answer to "is the launcher open?". Three subsystems each
compute it independently by polling GTK:

| Site | Asks |
|---|---|
| `window.rs` toggle | `gtk_window.is_visible()` |
| `platform/linux.rs` focus-out | `w.is_visible()` / `is_active()` |
| `commands/config.rs` hide | assumes it is open |

`is_visible()` is a widget flag, not launcher state. On Wayland, mapping is a
compositor round-trip, so the flag is *transiently wrong* by design. Three
deciders polling a racy flag will disagree — and the log shows them disagreeing
microseconds apart.

`dismiss_armed` was already a patch over this: it does not make focus-out
trustworthy, it only delays the first spurious dismiss until after the first
keystroke. Which is precisely why the bug presents as *"it closes when I type."*

## Design: one state machine, events as inputs

```rust
enum LauncherState {
    Hidden,   // not mapped
    Showing,  // show requested; mapping + focus still settling
    Visible,  // mapped AND focus confirmed
    Hiding,   // hide requested; unmap in flight
}
```

One `Arc<Mutex<LauncherState>>` in `AppState` is the single decider. No other
site calls `is_visible()` to make a decision.

### Transitions

| From | Event | To | Note |
|---|---|---|---|
| `Hidden` | toggle / hotkey | `Showing` | spawn `show_window` |
| `Showing` | `focus-in` | `Visible` | focus established |
| `Showing` | `focus-out` | `Showing` | **ignored** — focus never established |
| `Visible` | `focus-out` | `Hiding` | the only dismissing transition |
| `Visible` | toggle | `Hiding` | user asked to close |
| `Hiding` | `unmap` / hide done | `Hidden` | |
| `Hidden` | `focus-out` | `Hidden` | ignored |

The spurious events become structurally unreadable as dismissals: they arrive
while the state is `Showing`, where focus-out means "focus has not settled
yet", not "the user left". That is a *definition*, not a heuristic — no
`is_active()` guess required.

### Why this fixes what the patch broke

Toggle stops polling GTK and reads the state machine:

- `Visible | Showing` → hide
- `Hidden | Hiding` → show

It can no longer disagree with the dismiss path, because both read the same
value. The `Showing`/`Hiding` states exist precisely to represent "in flight",
which a boolean cannot.

## Constraint that must be preserved

`toggle_window` currently decides **and acts on the GTK main thread**, inline.
Its doc comment records why:

> The old pattern (`is_visible()` round-trip from an arbitrary thread, then a
> separately queued hide/show) interleaved under concurrency and caused the
> "press the hotkey twice" bug.

The state machine must not reintroduce that. Rule: **the decision and the
transition happen under one lock, on the GTK thread.** Reading the state from
another thread to decide, then acting later, recreates the original race with
extra steps.

The 250ms toggle debounce stays — it absorbs double-delivery of one physical
keypress, which is a different problem.

## What gets deleted

- `dismiss_armed: Arc<AtomicBool>`
- `armed_seq: Arc<AtomicU64>`
- `summon_seq: Arc<AtomicU64>` (as a dismiss guard; may remain for log
  correlation)

All three exist only to approximate the state machine.

## Prior art

fuzzel hit this class of bug and concluded it could not be fixed universally,
shipping `exit-on-keyboard-focus-loss` as an escape hatch. We already have that
lever in `general.hide_on_blur`, so no new config key is needed — but it stays,
because a compositor can always be wrong in a way no state machine catches.

## Verification

Not "it compiles". The reproduction is known and must be re-run:

1. Type ≥6 characters — the window must stay open (the reported bug).
2. Click away / Alt-Tab — it must still dismiss (proves dismiss is not simply
   disabled, which is how the first patch "passed").
3. Hotkey summon → hotkey again → hides. Repeat 5× — no "press twice" drift.
4. Both KDE (local) and GNOME (tester) — the entire point is compositor
   independence.

Step 2 is the one that catches a fix that only appears to work.
