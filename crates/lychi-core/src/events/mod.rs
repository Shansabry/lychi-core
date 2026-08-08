//! Domain-event bus — the state-change propagation spine.
//!
//! Lychi's hot path (keystroke → resolve → validate → execute) is a synchronous
//! request/response pipeline and stays that way; events are deliberately NOT used
//! there. This bus is for the *other* axis: when one thing changes and several
//! subsystems must react without knowing about each other.
//!
//! The motivating case (and the first migration): saving settings used to have a
//! command imperatively poke five subsystems (re-register the shell handler,
//! refresh quicklink keywords, update IDE markers, set the pinned workspace, …). Now
//! the command emits a single `ConfigChanged` and each subsystem reacts to its
//! own concern. The command no longer knows those subsystems exist.
//!
//! Design choices for v1 (kept deliberately boring):
//!   - **Synchronous dispatch.** Reactions are cheap (flip a cache, re-register a
//!     handler). No async runtime, no channels, no external broker. A reactor that
//!     ever needs slow work spawns its own thread inside `handle`.
//!   - **Events carry *what changed*, not the changed data.** `ConfigChanged`
//!     names a section; reactors read the live config themselves. This keeps the
//!     event vocabulary decoupled from the config structs.
//!   - **Core defines the vocabulary; the app layer wires reactors.** Reactions
//!     that touch app-owned state (the executor/registry, which live in the Tauri
//!     crate) subscribe from there — so `lychi-core` stays Tauri-free.

use std::sync::{Arc, RwLock};

/// Which slice of configuration changed. Reactors match on this to decide whether
/// a change concerns them, then read the current config for the details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSection {
    /// `[commands]` — shell, terminal, search-engine shortcuts, routing.
    Commands,
    /// `[projects]` — project directories, IDE markers, pinned workspace.
    Projects,
    /// `[ai]` — provider/model/mode.
    Ai,
    /// `[general]` — window/theme/hotkey and other app-shell settings.
    General,
}

/// A past-tense fact about something that already happened. Never a command or a
/// query — emitters don't care who (if anyone) is listening.
#[derive(Debug, Clone)]
pub enum DomainEvent {
    /// A section of the persisted config was saved. Reactors re-derive their state
    /// from the live config for the named section.
    ConfigChanged { section: ConfigSection },
    /// The desktop-app index finished (re)building.
    AppIndexRebuilt { count: usize },
    /// The active code workspace changed (e.g. focus moved to a different repo).
    WorkspaceSwitched { root: Option<String> },
}

/// A subscriber that reacts to domain events. Implementations must be cheap and
/// non-blocking; offload slow work to a spawned thread.
pub trait EventHandler: Send + Sync {
    fn handle(&self, event: &DomainEvent);
}

/// Maximum re-entrant `emit` depth. A reactor is allowed to emit further events
/// (that's why handlers are cloned out of the lock), but an unbounded cascade —
/// or an accidental A→B→A loop — must not blow the stack. Beyond this depth the
/// event is dropped with a warning. Real reactor chains are 1-2 deep.
const MAX_EMIT_DEPTH: usize = 8;

thread_local! {
    /// Current re-entrant emit depth on this thread. Emit is synchronous and
    /// same-thread, so a thread-local is the correct scope for the cap.
    static EMIT_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Synchronous in-process fan-out. Owned by `AppState` and injected like the
/// database is — never reached through a global.
#[derive(Default)]
pub struct EventBus {
    handlers: RwLock<Vec<Arc<dyn EventHandler>>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            handlers: RwLock::new(Vec::new()),
        }
    }

    /// Register a reactor. Typically done once at startup.
    pub fn subscribe(&self, handler: Arc<dyn EventHandler>) {
        if let Ok(mut handlers) = self.handlers.write() {
            handlers.push(handler);
        }
    }

    /// Fan an event out to every subscriber, synchronously and in subscription
    /// order.
    ///
    /// Failure boundaries (G3):
    /// - A poisoned lock is treated as "no subscribers" — emitting must never
    ///   take down the caller.
    /// - Each subscriber runs inside `catch_unwind`, so one panicking reactor
    ///   does NOT prevent the others from running (the panic is logged).
    /// - Re-entrant emits are capped at `MAX_EMIT_DEPTH` so a reactor that emits
    ///   further events can't cause an unbounded cascade or an A→B→A stack blow.
    ///
    /// Contract for callers: emit *after* your own persist+commit and *after*
    /// releasing any DB/state locks (reactors re-acquire them with `blocking_*`).
    pub fn emit(&self, event: DomainEvent) {
        // Reactors take their locks with `blocking_*`, which tokio panics on
        // when the calling thread is driving async tasks — and the per-handler
        // `catch_unwind` below would swallow that panic into one log line, i.e.
        // the reactor silently never runs. That is not hypothetical: the
        // Commands/Projects saves shipped in exactly that state (persisted to
        // disk, never hot-applied) while only the AI save had the hop. Fail
        // loudly in dev instead; async callers use `emit_from_async`.
        debug_assert_blocking_legal("EventBus::emit — use emit_from_async");

        let depth = EMIT_DEPTH.with(|d| d.get());
        if depth >= MAX_EMIT_DEPTH {
            tracing::warn!(
                "[events] dropping {event:?} — re-entrant emit depth {depth} exceeded cap {MAX_EMIT_DEPTH} (possible reactor loop)"
            );
            return;
        }

        // Clone the handler list out of the lock so a reactor can itself subscribe
        // (or emit) without deadlocking on the RwLock.
        let handlers = match self.handlers.read() {
            Ok(guard) => guard.clone(),
            Err(_) => return,
        };
        tracing::debug!("[events] emit {event:?} → {} subscriber(s)", handlers.len());

        EMIT_DEPTH.with(|d| d.set(depth + 1));
        // Restore depth even if a subscriber unwinds past the catch (shouldn't,
        // but be defensive) — a guard makes the decrement panic-safe.
        struct DepthGuard;
        impl Drop for DepthGuard {
            fn drop(&mut self) {
                EMIT_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
            }
        }
        let _guard = DepthGuard;

        for handler in &handlers {
            // Isolate each subscriber: a panic in one reactor is logged and the
            // remaining reactors still run. `AssertUnwindSafe` is sound here —
            // `&event` is shared-immutable and handlers are `Send + Sync`.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handler.handle(&event);
            }));
            if result.is_err() {
                tracing::error!("[events] a reactor panicked handling {event:?} — continuing");
            }
        }
    }

    /// Emit from an async execution context (a tokio worker task).
    ///
    /// [`emit`](Self::emit) runs every reactor synchronously on the calling
    /// thread, and reactors acquire their locks with `blocking_*` — documented
    /// by tokio to panic inside an async execution context. This wrapper hops
    /// to a blocking thread first, so the reactors run on a thread where
    /// `blocking_*` is legal. Synchronous callers (watcher threads, the main
    /// thread, code already inside `spawn_blocking`) keep calling `emit`.
    pub async fn emit_from_async(self: &Arc<Self>, event: DomainEvent) {
        let bus = Arc::clone(self);
        // A JoinError here means the blocking task itself panicked. emit
        // already isolates reactor panics, so this is out-of-memory-grade
        // unlikely — but an escaped panic must not take the caller down.
        if tokio::task::spawn_blocking(move || bus.emit(event))
            .await
            .is_err()
        {
            tracing::error!("[events] emit_from_async: the emitting task panicked");
        }
    }
}

/// Dev-only guard: panic if a `blocking_*` lock acquisition would panic on the
/// current thread — i.e. this thread is driving async tasks.
///
/// The probe IS the operation. `Handle::try_current()` cannot be the predicate:
/// it reports the runtime context as present both on async workers (blocking
/// illegal) and inside `spawn_blocking` closures (blocking legal — measured,
/// not assumed). So ask tokio directly, by performing a trivially uncontended
/// `blocking_read` and catching the context panic it raises on async workers.
/// Costs nanoseconds, runs only in debug builds, and cannot drift from tokio's
/// real rule because it exercises it.
#[cfg(debug_assertions)]
pub fn debug_assert_blocking_legal(who: &str) {
    let legal =
        std::panic::catch_unwind(|| drop(tokio::sync::RwLock::new(0u8).blocking_read())).is_ok();
    assert!(
        legal,
        "{who}: called from an async execution context. blocking_* locks panic \
         here, and in release the failure is silent (catch_unwind eats it) — \
         hop via spawn_blocking or a plain thread first."
    );
}

#[cfg(not(debug_assertions))]
#[inline]
pub fn debug_assert_blocking_legal(_who: &str) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counter(Arc<AtomicUsize>);
    impl EventHandler for Counter {
        fn handle(&self, event: &DomainEvent) {
            if matches!(
                event,
                DomainEvent::ConfigChanged {
                    section: ConfigSection::Commands
                }
            ) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[test]
    fn emit_reaches_all_subscribers() {
        let bus = EventBus::new();
        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));
        bus.subscribe(Arc::new(Counter(a.clone())));
        bus.subscribe(Arc::new(Counter(b.clone())));

        bus.emit(DomainEvent::ConfigChanged {
            section: ConfigSection::Commands,
        });

        assert_eq!(a.load(Ordering::SeqCst), 1);
        assert_eq!(b.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn reactors_filter_by_section() {
        let bus = EventBus::new();
        let hits = Arc::new(AtomicUsize::new(0));
        bus.subscribe(Arc::new(Counter(hits.clone())));

        // Only the Commands section increments the counter.
        bus.emit(DomainEvent::ConfigChanged {
            section: ConfigSection::Projects,
        });
        bus.emit(DomainEvent::ConfigChanged {
            section: ConfigSection::Commands,
        });
        bus.emit(DomainEvent::AppIndexRebuilt { count: 5 });

        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn emit_with_no_subscribers_is_a_noop() {
        let bus = EventBus::new();
        // Must not panic.
        bus.emit(DomainEvent::WorkspaceSwitched { root: None });
    }

    // --- PLAT-3: blocking reactors vs async emit contexts ---

    /// A reactor shaped like the real ones: takes a tokio lock with
    /// `blocking_read`, which panics on a thread that is driving async tasks.
    struct BlockingReactor {
        lock: Arc<tokio::sync::RwLock<u32>>,
        ran: Arc<AtomicUsize>,
    }
    impl EventHandler for BlockingReactor {
        fn handle(&self, _event: &DomainEvent) {
            let _guard = self.lock.blocking_read();
            self.ran.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// The regression that shipped: Commands/Projects saves emitted straight
    /// from an async command, the reactor's `blocking_read` panicked, and
    /// `catch_unwind` reduced "settings never hot-apply" to one log line.
    /// `emit_from_async` must actually reach a blocking-lock reactor.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn emit_from_async_reaches_blocking_reactors() {
        let bus = Arc::new(EventBus::new());
        let ran = Arc::new(AtomicUsize::new(0));
        bus.subscribe(Arc::new(BlockingReactor {
            lock: Arc::new(tokio::sync::RwLock::new(0)),
            ran: ran.clone(),
        }));

        bus.emit_from_async(DomainEvent::ConfigChanged {
            section: ConfigSection::Commands,
        })
        .await;

        assert_eq!(
            ran.load(Ordering::SeqCst),
            1,
            "a blocking_* reactor must run when the emit originates on a tokio worker"
        );
    }

    /// The other half of the contract: a direct `emit` from an async context
    /// is a bug, and must fail loudly in dev builds instead of unwinding into
    /// catch_unwind where nobody sees it.
    #[tokio::test]
    #[should_panic(expected = "emit_from_async")]
    async fn emitting_directly_from_async_context_fails_loudly() {
        let bus = EventBus::new();
        bus.emit(DomainEvent::WorkspaceSwitched { root: None });
    }

    // --- G3: failure boundaries ---

    struct Panicker;
    impl EventHandler for Panicker {
        fn handle(&self, _event: &DomainEvent) {
            panic!("reactor boom");
        }
    }

    #[test]
    fn one_panicking_reactor_does_not_stop_others() {
        // A panic in an earlier subscriber must not prevent a later one from
        // running. Silence the default panic hook so the test output stays clean.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let bus = EventBus::new();
        let after = Arc::new(AtomicUsize::new(0));
        bus.subscribe(Arc::new(Panicker));
        bus.subscribe(Arc::new(Counter(after.clone())));

        bus.emit(DomainEvent::ConfigChanged {
            section: ConfigSection::Commands,
        });

        std::panic::set_hook(prev);
        assert_eq!(after.load(Ordering::SeqCst), 1, "later reactor still ran");
    }

    struct ReEmitter {
        bus: *const EventBus,
        seen: Arc<AtomicUsize>,
    }
    // Test-only: the bus outlives the handler within the test scope.
    unsafe impl Send for ReEmitter {}
    unsafe impl Sync for ReEmitter {}
    impl EventHandler for ReEmitter {
        fn handle(&self, _event: &DomainEvent) {
            self.seen.fetch_add(1, Ordering::SeqCst);
            // Re-emit unconditionally — without the depth cap this recurses forever.
            let bus = unsafe { &*self.bus };
            bus.emit(DomainEvent::ConfigChanged {
                section: ConfigSection::Commands,
            });
        }
    }

    #[test]
    fn reentrant_emit_is_depth_capped() {
        let bus = Box::new(EventBus::new());
        let bus_ptr: *const EventBus = &*bus;
        let seen = Arc::new(AtomicUsize::new(0));
        bus.subscribe(Arc::new(ReEmitter {
            bus: bus_ptr,
            seen: seen.clone(),
        }));

        // Terminates (does not stack-overflow) thanks to MAX_EMIT_DEPTH.
        bus.emit(DomainEvent::ConfigChanged {
            section: ConfigSection::Commands,
        });

        // The initial emit is depth 0; the reactor re-emits until the cap. So the
        // handler runs exactly MAX_EMIT_DEPTH times (once per allowed level).
        assert_eq!(seen.load(Ordering::SeqCst), MAX_EMIT_DEPTH);
    }
}
