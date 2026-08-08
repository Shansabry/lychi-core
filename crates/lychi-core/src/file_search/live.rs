//! The single-slot holder that makes "latest wins" the only possible outcome.
//!
//! Search-as-you-type has one hard rule: the newest query owns the screen. The
//! classic failure is a slow earlier query finishing after a fast later one and
//! overwriting it — the standard fix being a generation counter with stale-result
//! rejection.
//!
//! The reason that rule lives *here*, in one type, rather than at each call site:
//! there are three consumers of file search (the `/` stream, nucleo's redraw
//! callback, and the one-shot `@` completion path), and when each enforced
//! freshness itself they disagreed. This owns the current [`SearchSession`] and
//! the current generation together, so a caller cannot hold one without the
//! other, and cannot act on a session that has been superseded.
//!
//! Replacing the session drops the old one. That *is* the cancellation: a
//! superseded matcher is deallocated rather than flagged, so there is no path by
//! which its results reach a consumer.

use std::sync::{Arc, Mutex};

use super::corpus::{CorpusStore, PathCorpus, now_ms};
use super::session::{Generation, ScopeMatcher, SearchResults};

/// Owns the in-flight search and hands out only fresh results.
pub struct LiveSearch {
    corpora: Arc<CorpusStore>,
    /// The matcher for the scope currently being searched, keyed by scope so a
    /// scope change rebuilds it but a query change does not. One matcher, reused
    /// across keystrokes — Helix's model, and what makes a keystroke a reparse
    /// rather than a 162k-item re-injection.
    matcher: Mutex<Option<(String, ScopeMatcher)>>,
    /// Monotonic, bumped per accepted search. A result carrying anything else is
    /// by definition from a superseded query.
    generation: std::sync::atomic::AtomicU64,
    /// When the matcher was last touched, for [`Self::release_if_idle`].
    ///
    /// Milliseconds on the same monotonic clock the corpus uses, or `None`
    /// before the first search. An `Option` rather than a zero sentinel: the
    /// clock starts near zero, so "never used" and "used at t=0" are genuinely
    /// different states that a sentinel would merge — and it merged them in a
    /// way that refused to release, which is the failure that hides.
    last_used_ms: Mutex<Option<u64>>,
}

/// How long the matcher may sit unused before it is dropped.
///
/// The matcher is the single largest thing this process holds after a search:
/// nucleo keeps its own normalised copy of every injected path (a `Utf32String`
/// per item) plus a worker pool, measured at ~27MB and 12 threads for a
/// 162k-path scope. It is retained between keystrokes on purpose — that is what
/// makes a keystroke a reparse rather than a re-injection — but "between
/// keystrokes" and "for the rest of the session" are different claims, and the
/// code only ever made the first one.
///
/// Five minutes is chosen against what re-acquiring it costs: re-injection was
/// measured at ~18-25ms, once, on the next search. A user who searches again
/// within five minutes keeps the warm path; one who does not gets 25ms back on
/// a keystroke they were about to wait for anyway.
const MATCHER_IDLE_TIMEOUT_MS: u64 = 5 * 60 * 1000;

impl LiveSearch {
    pub fn new(corpora: Arc<CorpusStore>) -> Self {
        Self {
            corpora,
            matcher: Mutex::new(None),
            generation: std::sync::atomic::AtomicU64::new(0),
            last_used_ms: Mutex::new(None),
        }
    }

    /// Drop the matcher if nothing has used it for [`MATCHER_IDLE_TIMEOUT_MS`].
    ///
    /// Returns whether it was dropped. Safe to call at any time from any thread:
    /// the next search rebuilds transparently (`begin` already handles a `None`
    /// matcher — it is the cold-start path), so this can only ever cost latency,
    /// never correctness.
    ///
    /// Deliberately a *poll* rather than a timer armed at the end of each
    /// search. A timer would need cancelling and re-arming on every keystroke,
    /// which is a lifecycle to get wrong for no benefit; releasing 27MB a minute
    /// late costs nothing.
    pub fn release_if_idle(&self) -> bool {
        self.release_if_idle_for(MATCHER_IDLE_TIMEOUT_MS)
    }

    /// [`Self::release_if_idle`] with an explicit timeout.
    ///
    /// Exists because `now_ms()` counts from process start: in a test binary it
    /// is a few tens of milliseconds, so no arithmetic on the stamp can fake
    /// five minutes having passed. Passing the threshold in tests the real rule
    /// (elapsed vs threshold) against a clock that has genuinely advanced,
    /// rather than testing a mocked clock.
    fn release_if_idle_for(&self, timeout_ms: u64) -> bool {
        // Matcher lock first, then the stamp — the same order `begin` takes, so
        // the two can never deadlock against each other.
        let mut guard = self.matcher.lock().unwrap();
        if guard.is_none() {
            return false;
        }
        let mut last = self.last_used_ms.lock().unwrap();
        let Some(used_at) = *last else {
            // A matcher with no recorded use should not happen, but treating it
            // as "release it" would race a search that is mid-`begin`.
            return false;
        };
        if now_ms().saturating_sub(used_at) < timeout_ms {
            return false;
        }
        *guard = None;
        *last = None;
        tracing::debug!("[search] matcher released after idle timeout");
        true
    }

    /// Drop path corpora for scopes nobody has searched recently.
    ///
    /// Exposed here because `LiveSearch` owns the store; the caller is the
    /// upkeep tick, which should not need to know the store exists.
    pub fn evict_idle_scopes(&self) -> usize {
        self.corpora.evict_idle()
    }

    /// True when a matcher is currently held. For tests and diagnostics.
    pub fn has_matcher(&self) -> bool {
        self.matcher.lock().unwrap().is_some()
    }

    /// The corpus for `scope`, building it on first use.
    ///
    /// On first build this also subscribes the live search to the corpus, so an
    /// in-flight query is re-seeded whenever more paths arrive. Subscribing here
    /// (once per corpus) rather than per search is deliberate: `PathCorpus`
    /// publishes its paths *before* it flips `is_complete`, so a per-search
    /// "subscribe only if incomplete" check races that publish and can miss the
    /// only notification it needed.
    pub fn corpus(self: &Arc<Self>, scope: &str) -> Arc<PathCorpus> {
        let (corpus, is_new) = self.corpora.get_or_build_tracked(scope);
        if is_new {
            // Subscribe BEFORE the walk starts — see the note above.
            self.reseed_on_corpus_change(&corpus);
            self.corpora.start(&corpus);
        }
        corpus
    }

    /// Begin a search, replacing (and cancelling) any search in flight.
    ///
    /// Returns the generation assigned to it. The caller passes that generation
    /// back to [`Self::results`]; anything older is silently dropped, so a late
    /// callback from a previous keystroke cannot repaint the list.
    pub fn begin(
        self: &Arc<Self>,
        scope: &str,
        query: &str,
        redraw: Arc<dyn Fn() + Send + Sync>,
    ) -> Generation {
        // Via `corpus()`, not the store directly, so the re-seed subscription is
        // guaranteed to exist before any search can observe an empty corpus.
        let corpus = self.corpus(scope);
        let generation = Generation(
            self.generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                + 1,
        );

        let mut guard = self.matcher.lock().unwrap();
        // Stamped on every search, including plain keystrokes, so "idle" means
        // no search activity rather than no matcher rebuild. Taken while
        // holding the matcher lock so the reaper cannot observe a matcher
        // without its refreshed stamp.
        *self.last_used_ms.lock().unwrap() = Some(now_ms());
        // Rebuild only when the scope changed or the corpus was re-walked.
        // A plain keystroke hits neither branch — it is just a reparse.
        let needs_build = match guard.as_ref() {
            Some((held_scope, m)) => held_scope != scope || m.is_stale(&corpus),
            None => true,
        };
        if needs_build {
            *guard = Some((scope.to_string(), ScopeMatcher::new(&corpus, redraw)));
        }
        if let Some((_, m)) = guard.as_mut() {
            m.set_query(query, generation);
        }
        generation
    }

    /// Rebuild the scope's matcher when the corpus publishes new paths, keeping
    /// the query and generation it was already answering.
    ///
    /// Without this a query typed before the walk finished stays empty forever: a
    /// matcher holds the path snapshot it was injected with and cannot learn that
    /// more arrived. That is the common case, not an edge one — the launcher opens
    /// and the user types immediately.
    ///
    /// Re-injection here is deliberate and rare. A keystroke never reaches this
    /// path (it is a reparse on the existing matcher); only a corpus publish does,
    /// which is once at startup plus once per debounced filesystem change. Keeping
    /// the same generation means an in-flight emit stays valid — this is the same
    /// query answered over more paths, not a new search.
    fn reseed_on_corpus_change(self: &Arc<Self>, corpus: &Arc<PathCorpus>) {
        // Weak, so a subscription never keeps the app's search state alive.
        let weak = Arc::downgrade(self);
        // Weak for the corpus too — and this one is load-bearing in the other
        // direction: the closure is STORED IN that corpus's own `on_change`
        // list, so a strong capture here is a self-referential Arc cycle. With
        // it, an evicted corpus could never deallocate — the reaper released
        // the watcher thread and inotify watches while the arena (15-39MB per
        // scope) leaked for the process lifetime, silently defeating the
        // memory half of the eviction fix this module exists to serve.
        // (`begin_shutdown` also clears `on_change` at eviction — deliberate
        // redundancy so a future strongly-capturing subscriber elsewhere
        // cannot re-create the leak.)
        let weak_corpus = Arc::downgrade(corpus);
        corpus.subscribe(Arc::new(move || {
            let Some(live) = weak.upgrade() else {
                return; // app shutting down
            };
            let Some(corpus_for_cb) = weak_corpus.upgrade() else {
                return; // corpus evicted; a fresh scope re-subscribes
            };
            let redraw = {
                let mut guard = live.matcher.lock().unwrap();
                let Some((held_scope, m)) = guard.as_mut() else {
                    return; // nothing being searched
                };
                if held_scope != corpus_for_cb.scope() || !m.is_stale(&corpus_for_cb) {
                    return; // different scope, or already on the newest paths
                }
                let Some((query, generation)) = m.current_query() else {
                    // Warm matcher with no active query: let the next `begin`
                    // rebuild it rather than paying for injection now.
                    return;
                };
                let redraw = m.redraw();
                let mut rebuilt = ScopeMatcher::new(&corpus_for_cb, redraw.clone());
                rebuilt.set_query(&query, generation);
                *guard = Some((held_scope.clone(), rebuilt));
                redraw
            };
            // Outside the lock: the hook reads results, which takes this same
            // lock, so firing it while held would deadlock.
            redraw();
        }));
    }

    /// Whether `generation` is still the one the user is waiting for.
    pub fn is_current(&self, generation: Generation) -> bool {
        Generation(self.generation.load(std::sync::atomic::Ordering::Acquire)) == generation
    }

    /// Results for `generation`, or `None` if it has been superseded.
    ///
    /// The generation check is the whole contract: a caller literally cannot
    /// obtain results for a query that is no longer current, so it cannot emit
    /// them by mistake.
    pub fn results(&self, generation: Generation, limit: usize) -> Option<SearchResults> {
        if !self.is_current(generation) {
            return None;
        }
        let mut guard = self.matcher.lock().unwrap();
        let (_, m) = guard.as_mut()?;
        m.results_for(generation, limit)
    }

    /// Drop the in-flight search. Later results for it are rejected by the
    /// generation bump, so a cancel cannot be undone by an in-flight callback.
    pub fn cancel(&self) {
        // Only the generation moves. The matcher stays warm: its injected items
        // are still valid, and dropping them would make the next search pay for a
        // full re-injection. Attribution is safe because every outstanding
        // generation is now stale.
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TempTree(std::path::PathBuf);
    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tree(files: &[&str]) -> TempTree {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lychi-live-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        for f in files {
            let p = dir.join(f);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("mkdir parent");
            }
            std::fs::write(&p, b"x").expect("write");
        }
        TempTree(dir)
    }

    /// Build a `LiveSearch` whose corpus has finished walking.
    fn live(t: &TempTree) -> (Arc<LiveSearch>, String) {
        let scope = t.0.to_string_lossy().into_owned();
        let live = Arc::new(LiveSearch::new(Arc::new(CorpusStore::default())));
        let corpus = live.corpus(&scope);
        for _ in 0..200 {
            if corpus.is_complete() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(corpus.is_complete(), "corpus never finished");
        (live, scope)
    }

    fn noop() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    /// Release as if the idle timeout had elapsed.
    ///
    /// A zero threshold rather than a backdated stamp: `now_ms` counts from
    /// process start, so in a test binary it is tens of milliseconds and
    /// `stamp - 5min` saturates to 0 — which reads as "used 40ms ago", not
    /// "used 5 minutes ago". This exercises the real comparison instead.
    fn release_now(live: &LiveSearch) -> bool {
        live.release_if_idle_for(0)
    }

    /// The matcher must survive an idle period shorter than the timeout — this
    /// is the property that makes a keystroke a reparse rather than a 162k-item
    /// re-injection, and releasing eagerly would destroy it.
    #[test]
    fn an_active_matcher_is_not_released() {
        let t = tree(&["a/readme.md"]);
        let (live, scope) = live(&t);
        live.begin(&scope, "readme", noop());
        assert!(live.has_matcher());

        assert!(!live.release_if_idle(), "released while recently used");
        assert!(live.has_matcher(), "matcher must survive a recent search");
    }

    /// The fix: after the idle timeout, nucleo's per-path copy and worker pool
    /// are dropped rather than held for the process lifetime.
    #[test]
    fn an_idle_matcher_is_released() {
        let t = tree(&["a/readme.md"]);
        let (live, scope) = live(&t);
        live.begin(&scope, "readme", noop());
        assert!(live.has_matcher());

        // Backdate the last-use stamp past the timeout.
        assert!(release_now(&live), "should have released");
        assert!(!live.has_matcher());
    }

    /// Releasing must be invisible to the user: the next search rebuilds. If it
    /// were not, this would trade memory for broken search.
    #[test]
    fn a_search_after_release_still_returns_results() {
        let t = tree(&["a/readme.md", "b/other.rs"]);
        let (live, scope) = live(&t);

        let g1 = live.begin(&scope, "readme", noop());
        let before = live.results(g1, 10).map(|r| r.items.len()).unwrap_or(0);
        assert!(before > 0, "baseline search found nothing");

        assert!(release_now(&live));

        let g2 = live.begin(&scope, "readme", noop());
        let after = live.results(g2, 10).map(|r| r.items.len()).unwrap_or(0);
        assert_eq!(
            after, before,
            "a rebuilt matcher must find what the warm one found"
        );
    }

    /// Nothing to release before the first search — and no panic for trying.
    #[test]
    fn releasing_without_a_matcher_is_a_no_op() {
        let live = Arc::new(LiveSearch::new(Arc::new(CorpusStore::default())));
        assert!(!live.release_if_idle());
        assert!(!live.has_matcher());
    }

    /// A keystroke stamps use even though it does not rebuild the matcher, so
    /// "idle" means no searching rather than no rebuilding. Stamping only on
    /// rebuild would release the matcher out from under an active typist.
    #[test]
    fn a_plain_keystroke_counts_as_use() {
        let t = tree(&["a/readme.md"]);
        let (live, scope) = live(&t);
        live.begin(&scope, "read", noop());
        let first = live.last_used_ms.lock().unwrap().expect("stamped");

        // Type another character. Same scope and a live corpus, so this is a
        // reparse on the existing matcher — it does NOT rebuild.
        std::thread::sleep(std::time::Duration::from_millis(5));
        live.begin(&scope, "readm", noop());
        let second = live.last_used_ms.lock().unwrap().expect("stamped");

        assert!(
            second > first,
            "a keystroke must refresh the idle clock ({first} -> {second}); \
             stamping only on matcher REBUILD would let the reaper release the \
             matcher out from under someone who is actively typing"
        );
    }

    /// The rule this type exists for: once a newer search starts, the older one
    /// can no longer produce results. This is what stops a slow earlier query
    /// from repainting the list after a faster later one.
    #[test]
    fn a_superseded_search_cannot_produce_results() {
        let t = tree(&["a/readme.md", "b/main.rs"]);
        let (live, scope) = live(&t);

        let old = live.begin(&scope, "readme", noop());
        assert!(
            live.results(old, 10).is_some(),
            "control: newest is current"
        );

        let new = live.begin(&scope, "main", noop());
        assert!(
            live.results(old, 10).is_none(),
            "superseded generation still returned results"
        );
        assert!(live.results(new, 10).is_some(), "newest must work");
        assert!(!live.is_current(old));
        assert!(live.is_current(new));
    }

    /// Results always describe the query they came from.
    #[test]
    fn results_carry_their_own_query() {
        let t = tree(&["a/readme.md", "b/main.rs"]);
        let (live, scope) = live(&t);

        let g = live.begin(&scope, "readme", noop());
        let r = live.results(g, 10).expect("current");
        assert_eq!(r.query, "readme");
        assert_eq!(r.generation, g);
        assert!(
            r.items.iter().all(|p| p.file_name().contains("readme")),
            "leaked another query's items: {:?}",
            r.items.iter().map(|p| p.file_name()).collect::<Vec<_>>()
        );
    }

    /// The cold-start case, which is the COMMON case: the user types immediately
    /// after the launcher opens, while the corpus walk is still running.
    ///
    /// A session snapshots the corpus at construction, so without re-seeding it
    /// would match an empty path list and stay empty forever. Regression test for
    /// exactly that — it stayed at 0 results because the initial empty snapshot
    /// and the first walked one both carried generation 0, so the staleness check
    /// concluded it was already current.
    #[test]
    fn a_search_started_before_the_walk_finishes_fills_in() {
        let t = tree(&["a/readme.md", "b/readme.txt", "c/other.bin"]);
        let scope = t.0.to_string_lossy().into_owned();
        let live = Arc::new(LiveSearch::new(Arc::new(CorpusStore::default())));

        // Deliberately do NOT wait for the corpus — search straight away.
        let hits = Arc::new(AtomicUsize::new(0));
        let redraw: Arc<dyn Fn() + Send + Sync> = {
            let hits = hits.clone();
            Arc::new(move || {
                hits.fetch_add(1, Ordering::Relaxed);
            })
        };
        let g = live.begin(&scope, "readme", redraw);

        // Poll for the re-seed to land.
        let mut found = 0;
        for _ in 0..200 {
            found = live.results(g, 10).map(|r| r.items.len()).unwrap_or(0);
            if found > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(found, 2, "cold search never picked up the walked paths");

        let r = live.results(g, 10).expect("still current");
        assert_eq!(r.generation, g, "re-seed must keep the same generation");
        assert_eq!(r.query, "readme");
        assert!(
            r.items.iter().all(|p| p.file_name().contains("readme")),
            "{:?}",
            r.items.iter().map(|p| p.file_name()).collect::<Vec<_>>()
        );

        // Injection must not notify per item: nucleo calls the hook once per
        // pushed path, and unguarded that is one IPC emit per path (measured at
        // 162,292 for a real home corpus). A small upper bound is the assertion —
        // the lower bound is deliberately not checked, because a walk that
        // finishes before the first `results` call legitimately needs no
        // notification at all, and asserting otherwise makes this test a race.
        let fired = hits.load(Ordering::Relaxed);
        assert!(
            fired < 50,
            "redraw fired {fired} times — injection is not gated"
        );
    }

    /// Generations increase, so ordering is well-defined even under rapid typing.
    #[test]
    fn generations_are_monotonic() {
        let t = tree(&["a/x.txt"]);
        let (live, scope) = live(&t);
        let mut last = Generation(0);
        for q in ["a", "ab", "abc", "abcd"] {
            let g = live.begin(&scope, q, noop());
            assert!(g > last, "generation went backwards: {g:?} after {last:?}");
            last = g;
        }
    }

    /// Cancel makes every outstanding generation stale, so an in-flight callback
    /// firing after the user dismissed cannot repaint.
    #[test]
    fn cancel_rejects_in_flight_results() {
        let t = tree(&["a/readme.md"]);
        let (live, scope) = live(&t);

        let g = live.begin(&scope, "readme", noop());
        assert!(live.results(g, 10).is_some());

        live.cancel();
        assert!(live.results(g, 10).is_none(), "results survived a cancel");
        assert!(!live.is_current(g));
    }

    /// An evicted corpus must actually DEALLOCATE, not merely stop.
    ///
    /// The reseed subscription is stored inside the corpus's own `on_change`
    /// list; when it captured a strong `Arc<PathCorpus>` that was a
    /// self-referential cycle, and every evicted scope leaked its whole arena
    /// (15-39MB) for the process lifetime. The eviction test in corpus.rs
    /// asserts the shutdown flag and map removal — this one asserts the only
    /// thing that frees memory: the refcount reaching zero.
    ///
    /// Two mechanisms both break the cycle (the Weak capture in
    /// `reseed_on_corpus_change`, and `begin_shutdown` clearing `on_change`);
    /// they are deliberately redundant, so this test fails only when BOTH are
    /// lost — that is the invariant, the mechanisms are implementation.
    #[test]
    fn evicted_corpus_deallocates_despite_reseed_subscription() {
        let t = tree(&["a/x.txt"]);
        let (live, scope) = live(&t);

        // Wires reseed_on_corpus_change — the subscription under test.
        let corpus = live.corpus(&scope);
        let weak = Arc::downgrade(&corpus);
        drop(corpus);

        assert_eq!(live.corpora.evict_idle_for(0), 1);
        // The watcher thread holds its own Arc until it notices the shutdown
        // (eviction drops the notify watcher, disconnecting the thread's
        // channel). That release is prompt but asynchronous — poll for it. A
        // cycle, by contrast, never releases: this loop times out.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while weak.upgrade().is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            weak.upgrade().is_none(),
            "evicted corpus is still strongly referenced — the reseed \
             subscription (or another cycle) is pinning the arena after evict"
        );
    }
}
