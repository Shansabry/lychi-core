//! One matcher per scope, reused across queries — Helix's picker model.
//!
//! A [`ScopeMatcher`] owns a `Nucleo` and the paths injected into it. Changing the
//! query is a `pattern.reparse`, never a rebuild: the item set is injected once
//! per corpus snapshot and matched against every subsequent query in place. This
//! is how Helix's picker (nucleo's reference consumer) works — a long-lived
//! injector streaming into a matcher that never restarts.
//!
//! Two earlier designs were wrong in opposite directions, and both are worth
//! recording because neither failure was obvious.
//!
//! **One matcher shared by every caller.** The `/` stream, nucleo's redraw
//! callback, and the one-shot `@` path all reparsed the same pattern column, so
//! any of them could change the query under another's feet. nucleo only swaps in
//! a new snapshot once its worker finishes, so a read landing mid-match returned
//! the *previous query's* matches — not fewer results, different ones. Measured
//! against a real home corpus mid-walk: 12 of 15 reads returned the prior query's
//! set. Searching `readme` showed whatever was typed before it.
//!
//! **A fresh matcher per query.** This fixed attribution but re-injected 162k
//! items per keystroke: ~150MB of allocator churn per search, and a read straight
//! after construction saw `candidates=0` because matching had not started.
//! Correct, but wasteful and briefly empty.
//!
//! The fix for attribution is not a private matcher — it is that a caller states
//! which query it is asking about and [`ScopeMatcher::results_for`] refuses to
//! answer for any other. Identity is checked against the query this matcher was
//! last told to match: a fact it holds, not one inferred from nucleo's snapshot.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Nucleo};

use super::corpus::{PathCorpus, Paths, SharedPath};

/// A monotonic search identifier.
///
/// The standard "latest wins" guard for search-as-you-type: every result set
/// carries the generation it was produced for, and a consumer drops anything that
/// is not the generation it currently wants. Without it a slow query typed
/// earlier can land after a fast one typed later and overwrite the screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct Generation(pub u64);

/// Results, tagged with what produced them.
pub struct SearchResults {
    /// Best-first matches, ranked by nucleo.
    pub items: Vec<SharedPath>,
    /// The query these are matches for.
    pub query: String,
    /// The search generation these belong to.
    pub generation: Generation,
    /// Whether matching has finished. `false` means more results may arrive — the
    /// set is incomplete, but never *wrong*: every item matches `query`.
    pub complete: bool,
}

/// The long-lived matcher for one search scope.
pub struct ScopeMatcher {
    nucleo: Nucleo<SharedPath>,
    /// The query currently loaded into the pattern, and the generation that asked
    /// for it. Held here so a read can be refused unless it names this query —
    /// the guard that makes misattribution impossible.
    current: Option<(String, Generation)>,
    /// Corpus generation the injected items came from. A newer corpus means the
    /// item set is stale and must be re-injected (rare: only on a rebuild).
    injected_generation: u64,
    /// Keeps the injected paths alive while they are in the matcher.
    _paths: Arc<Paths>,
    /// How many items were injected. Compared against the snapshot's own item
    /// count to tell whether matching has caught up with injection.
    injected_count: usize,
    /// Keeps the notifier thread running; cleared on drop.
    alive: Arc<AtomicBool>,
    /// The caller's notify hook, kept so a rebuild against a newer corpus
    /// snapshot reuses it rather than inventing one.
    redraw: Arc<dyn Fn() + Send + Sync>,
}

impl Drop for ScopeMatcher {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
    }
}

impl ScopeMatcher {
    /// Build a matcher for `corpus` and inject its current paths.
    ///
    /// `redraw` fires when matching makes progress. It is coalesced: nucleo
    /// notifies once per injected item, which for a 162k-path corpus is 162k calls
    /// — one IPC emit each if the hook is the UI's. Coalescing rather than
    /// suppressing matters, because the "matching finished" notification travels
    /// the same path; dropping those left the launcher stuck on "Searching..."
    /// with results that had been computed but were never delivered.
    pub fn new(corpus: &PathCorpus, redraw: Arc<dyn Fn() + Send + Sync>) -> Self {
        // fzf "path scheme": characters right after a `/` score higher, so
        // tail/filename hits rank above mid-path noise. Plus prefix preference.
        let config = {
            let mut c = Config::DEFAULT.match_paths();
            c.prefer_prefix = true;
            c
        };

        let dirty = Arc::new(AtomicBool::new(false));
        let coalesced: Arc<dyn Fn() + Send + Sync> = {
            let dirty = dirty.clone();
            Arc::new(move || dirty.store(true, Ordering::Release))
        };
        let nucleo = Nucleo::<SharedPath>::new(config, coalesced, None, 1);

        let paths = corpus.paths();
        let injector = nucleo.injector();
        for item in &paths.items {
            // Refcount bump, not a deep copy — see `SharedPath`.
            injector.push(item.clone(), |d, cols| {
                cols[0] = d.rel_path.as_str().into();
            });
        }

        let alive = Arc::new(AtomicBool::new(true));
        {
            let alive = alive.clone();
            let dirty = dirty.clone();
            let redraw = redraw.clone();
            std::thread::Builder::new()
                .name("lychi-search-notify".into())
                .spawn(move || {
                    while alive.load(Ordering::Acquire) {
                        std::thread::sleep(std::time::Duration::from_millis(30));
                        if dirty.swap(false, Ordering::AcqRel) {
                            redraw();
                        }
                    }
                })
                .ok();
        }

        Self {
            nucleo,
            current: None,
            injected_generation: paths.generation,
            injected_count: paths.items.len(),
            _paths: paths,
            alive,
            redraw,
        }
    }

    /// Whether the injected items came from an older corpus snapshot.
    pub fn is_stale(&self, corpus: &PathCorpus) -> bool {
        self.injected_generation < corpus.paths().generation
    }

    /// Point the matcher at `query`. Cheap — a reparse, never a re-injection.
    ///
    /// `is_append` tells nucleo the new pattern extends the old one (typing
    /// `Down` after `Dow`), letting it narrow the existing match set instead of
    /// rescoring every item. Helix computes it the same way; it is the difference
    /// between per-keystroke work proportional to the *current matches* and to the
    /// whole corpus.
    pub fn set_query(&mut self, query: &str, generation: Generation) {
        let is_append = self
            .current
            .as_ref()
            .is_some_and(|(prev, _)| !prev.is_empty() && query.starts_with(prev.as_str()));
        self.nucleo.pattern.reparse(
            0,
            query,
            CaseMatching::Ignore,
            Normalization::Smart,
            is_append,
        );
        self.current = Some((query.to_string(), generation));
        self.tick();
    }

    /// Advance matching one slice. 10ms is Helix's per-frame budget: enough to
    /// make progress, short enough never to be felt.
    pub fn tick(&mut self) -> bool {
        self.nucleo.tick(10).changed
    }

    /// Results for `generation`, or `None` if this matcher is answering a
    /// different query.
    ///
    /// The generation check is the whole attribution guarantee: a caller cannot
    /// obtain results for a superseded query, so it cannot emit them by mistake.
    pub fn results_for(&mut self, generation: Generation, limit: usize) -> Option<SearchResults> {
        let (query, current_gen) = self.current.clone()?;
        if current_gen != generation {
            return None;
        }
        self.tick();
        let snap = self.nucleo.snapshot();
        // "Have all injected items been matched against this query?"
        //
        // Deliberately NOT `Status::running`. That flag is
        // `items.count() > worker.item_count()` — it describes how far *injection*
        // has been consumed, not whether the query finished. Injecting 162k paths
        // in one burst keeps it true almost permanently, so deriving `complete`
        // from it reported `false` on 22 of 23 finished searches and left the UI
        // spinner up forever over results that had fully arrived.
        //
        // The snapshot's own `item_count` is the honest measure: it counts the
        // items this snapshot was built from, so once it reaches the injected
        // total, every path has been scored against the current pattern.
        let complete = snap.item_count() as usize >= self.injected_count;
        let n = snap.matched_item_count().min(limit as u32);
        Some(SearchResults {
            items: snap.matched_items(0..n).map(|i| i.data.clone()).collect(),
            query,
            generation,
            complete,
        })
    }

    /// The query and generation this matcher is currently answering, if any.
    pub fn current_query(&self) -> Option<(String, Generation)> {
        self.current.clone()
    }

    /// This matcher's notify hook, for handing to a replacement built against a
    /// newer corpus snapshot.
    pub fn redraw(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.redraw.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct TempTree(std::path::PathBuf);
    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tree(files: &[&str]) -> TempTree {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lychi-matcher-test-{}-{}",
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

    fn corpus(t: &TempTree) -> Arc<PathCorpus> {
        let c = PathCorpus::new(&t.0.to_string_lossy());
        for _ in 0..200 {
            if c.is_complete() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(c.is_complete(), "corpus walk never finished");
        c
    }

    fn settled(m: &mut ScopeMatcher, g: Generation, limit: usize) -> SearchResults {
        for _ in 0..200 {
            if m.results_for(g, limit).is_some_and(|r| r.complete) {
                break;
            }
            m.tick();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        m.results_for(g, limit).expect("current generation")
    }

    fn names(r: &SearchResults) -> Vec<String> {
        r.items.iter().map(|p| p.file_name.clone()).collect()
    }

    /// Changing the query must not re-inject: the same matcher answers a second,
    /// unrelated query correctly.
    #[test]
    fn one_matcher_answers_successive_queries() {
        let t = tree(&["a/readme.md", "b/main.rs", "c/logo.png"]);
        let c = corpus(&t);
        let mut m = ScopeMatcher::new(&c, Arc::new(|| {}));

        m.set_query("readme", Generation(1));
        let r = settled(&mut m, Generation(1), 10);
        assert_eq!(r.query, "readme");
        assert!(
            names(&r).iter().all(|n| n.contains("readme")),
            "{:?}",
            names(&r)
        );

        m.set_query("main", Generation(2));
        let r = settled(&mut m, Generation(2), 10);
        assert_eq!(r.query, "main");
        assert!(
            names(&r).iter().all(|n| n.contains("main")),
            "{:?}",
            names(&r)
        );
    }

    /// A finished search must report `complete: true`.
    ///
    /// Two earlier versions of this got it wrong, both silently: the UI reads
    /// `complete` as `done` and keeps a spinner up while it is false, so a search
    /// that had fully returned still looked like it was working. Neither bug
    /// affected the results themselves, which is exactly why a test is needed.
    ///
    /// The wrong signals were `Status::running` (which reports how far *injection*
    /// has been consumed, not whether the query matched — true almost permanently
    /// after a bulk inject) and a follow-up `tick(0)` probe (a 0ms `try_lock` that
    /// returns `running: true` having checked nothing).
    #[test]
    fn a_finished_search_reports_complete() {
        let t = tree(&["a/readme.md", "b/readme.txt", "c/other.bin"]);
        let c = corpus(&t);
        let mut m = ScopeMatcher::new(&c, Arc::new(|| {}));
        m.set_query("readme", Generation(1));

        let mut done = false;
        for _ in 0..200 {
            if m.results_for(Generation(1), 10).is_some_and(|r| r.complete) {
                done = true;
                break;
            }
            m.tick();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(done, "a settled search never reported complete=true");

        // And it must still be complete for a second, narrower query.
        m.set_query("readme.md", Generation(2));
        let mut done2 = false;
        for _ in 0..200 {
            if m.results_for(Generation(2), 10).is_some_and(|r| r.complete) {
                done2 = true;
                break;
            }
            m.tick();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(done2, "a narrowed search never reported complete=true");
    }

    /// Attribution: a superseded generation gets `None`, never another query's
    /// results. This is what a shared matcher used to get wrong.
    #[test]
    fn a_superseded_generation_gets_nothing() {
        let t = tree(&["a/readme.md", "b/main.rs"]);
        let c = corpus(&t);
        let mut m = ScopeMatcher::new(&c, Arc::new(|| {}));

        m.set_query("readme", Generation(1));
        assert!(m.results_for(Generation(1), 10).is_some());

        m.set_query("main", Generation(2));
        assert!(
            m.results_for(Generation(1), 10).is_none(),
            "superseded generation still got results"
        );
        assert!(m.results_for(Generation(2), 10).is_some());
    }

    /// A query matching nothing resolves to empty, never to leftovers.
    #[test]
    fn a_non_matching_query_is_empty() {
        let t = tree(&["a/readme.md"]);
        let c = corpus(&t);
        let mut m = ScopeMatcher::new(&c, Arc::new(|| {}));

        m.set_query("readme", Generation(1));
        assert!(
            !settled(&mut m, Generation(1), 10).items.is_empty(),
            "control"
        );

        m.set_query("qqqqqqqzzz", Generation(2));
        let r = settled(&mut m, Generation(2), 10);
        assert!(r.items.is_empty(), "stale results leaked: {:?}", names(&r));
    }

    /// `is_append` is an optimization, not a behaviour change: narrowing a query
    /// must give the same answer as asking it cold.
    #[test]
    fn appending_narrows_to_the_same_answer() {
        let t = tree(&["a/readme.md", "b/read-me-too.txt", "c/other.bin"]);
        let c = corpus(&t);

        // Typed incrementally: "read" then "readme" (is_append true).
        let mut typed = ScopeMatcher::new(&c, Arc::new(|| {}));
        typed.set_query("read", Generation(1));
        settled(&mut typed, Generation(1), 10);
        typed.set_query("readme", Generation(2));
        let mut incremental = names(&settled(&mut typed, Generation(2), 10));

        // Asked cold (is_append false).
        let mut fresh = ScopeMatcher::new(&c, Arc::new(|| {}));
        fresh.set_query("readme", Generation(1));
        let mut cold = names(&settled(&mut fresh, Generation(1), 10));

        incremental.sort();
        cold.sort();
        assert_eq!(incremental, cold, "append path diverged from a cold query");
    }

    /// Matching completion must NOTIFY. The UI is push-driven — it emits once on
    /// begin and repaints only on this hook — so a swallowed completion leaves it
    /// stuck on "Searching..." with results computed but never sent.
    #[test]
    fn matching_completion_notifies_the_caller() {
        let t = tree(&["a/readme.md", "b/readme.txt"]);
        let c = corpus(&t);
        let hits = Arc::new(AtomicUsize::new(0));
        let redraw: Arc<dyn Fn() + Send + Sync> = {
            let hits = hits.clone();
            Arc::new(move || {
                hits.fetch_add(1, Ordering::Relaxed);
            })
        };
        let mut m = ScopeMatcher::new(&c, redraw);
        m.set_query("readme", Generation(1));

        let mut fired = 0;
        for _ in 0..100 {
            fired = hits.load(Ordering::Relaxed);
            if fired > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(fired > 0, "matching never notified the caller");
        // Coalesced: nucleo notifies once per injected item.
        assert!(fired < 20, "notified {fired} times — not coalesced");
    }

    /// Dropping the matcher stops its notifier thread.
    #[test]
    fn a_dropped_matcher_stops_notifying() {
        let t = tree(&["a/readme.md"]);
        let c = corpus(&t);
        let hits = Arc::new(AtomicUsize::new(0));
        let redraw: Arc<dyn Fn() + Send + Sync> = {
            let hits = hits.clone();
            Arc::new(move || {
                hits.fetch_add(1, Ordering::Relaxed);
            })
        };
        {
            let mut m = ScopeMatcher::new(&c, redraw);
            m.set_query("readme", Generation(1));
            settled(&mut m, Generation(1), 10);
        } // dropped
        std::thread::sleep(std::time::Duration::from_millis(80));
        let after = hits.load(Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert_eq!(
            hits.load(Ordering::Relaxed),
            after,
            "a dropped matcher kept notifying"
        );
    }

    #[test]
    fn generations_are_comparable() {
        assert!(Generation(1) < Generation(2));
    }
}
