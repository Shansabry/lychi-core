//! Domain-event bus — the state-change propagation spine.
//!
//! Lychi's hot path (keystroke → resolve → validate → execute) is a synchronous
//! request/response pipeline and stays that way; events are deliberately NOT used
//! there. This bus is for the *other* axis: when one thing changes and several
//! subsystems must react without knowing about each other.
//!
//! The motivating case (and the first migration): saving settings used to have a
//! command imperatively poke five subsystems (re-register the shell handler,
//! refresh bang keywords, update IDE markers, set the pinned workspace, …). Now
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
    /// order. A poisoned lock is treated as "no subscribers" rather than a panic —
    /// emitting an event must never take down the caller.
    pub fn emit(&self, event: DomainEvent) {
        // Clone the handler list out of the lock so a reactor can itself subscribe
        // (or emit) without deadlocking on the RwLock.
        let handlers = match self.handlers.read() {
            Ok(guard) => guard.clone(),
            Err(_) => return,
        };
        tracing::debug!("[events] emit {event:?} → {} subscriber(s)", handlers.len());
        for handler in &handlers {
            handler.handle(&event);
        }
    }
}

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
}
