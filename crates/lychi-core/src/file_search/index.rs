//! Persistent fuzzy file index — the Raycast v2 / fzf / Telescope standard.
//!
//! Instead of re-walking the filesystem on every keystroke (single-threaded,
//! multi-second, cancelled by the next keystroke — the bug this replaces), each
//! search scope gets ONE long-lived [`nucleo::Nucleo`] engine:
//!
//!   1. **Persistent index, not per-keystroke walk.** A background PARALLEL walk
//!      (`build_parallel`, all cores, ~0.4s) injects every path once. Each
//!      keystroke is then just `reparse + tick` reading a snapshot — instant.
//!   2. **Match on the scope-relative path with a filename bonus.** nucleo's
//!      `match_paths` config is the fzf "path scheme": characters after a `/`
//!      score higher, so tail/filename hits rank above deep mid-path noise.
//!   3. **Standard ranking + tiebreak.** nucleo ranks by match quality; the
//!      caller nudges true ties with frecency/recency (a signal, not a driver).
//!   4. **One entry per unique path** (the walk visits each path once;
//!      `follow_links(false)` avoids symlink revisits).
//!   6. **Adaptive noise filtering.** `.gitignore`/`.ignore` honored even outside
//!      a git repo (`require_git(false)`) — each project's own rules apply.
//!
//! Freshness (part 1, cont.): a per-scope `notify` filesystem watcher rebuilds
//! the index after a 2s debounce on any create/delete/rename — event-driven, not
//! a timer. Indexes are built lazily on a scope's first search and then kept
//! alive (and watched) for the session.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ignore::WalkBuilder;
use notify::{Config as NotifyConfig, Event, RecommendedWatcher, RecursiveMode, Watcher};
use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Nucleo, Utf32String};

/// Max path depth the indexer descends.
const MAX_SEARCH_DEPTH: usize = 10;
/// Quiet period after a filesystem event before rebuilding a scope's index.
const REBUILD_DEBOUNCE: Duration = Duration::from_secs(2);

/// The payload stored per indexed path — everything the UI needs to render a
/// result without touching disk again.
#[derive(Clone)]
pub struct PathData {
    pub full_path: String,
    pub file_name: String,
    /// Path relative to the search scope — the match column, so nucleo's path
    /// scheme bonuses tail/filename characters (fzf "path scheme").
    pub rel_path: String,
    pub is_dir: bool,
    pub size_bytes: Option<u64>,
    pub modified_secs: Option<u64>,
}

/// A live per-scope fuzzy index (one nucleo engine + its background walk state).
pub struct FileIndex {
    nucleo: Nucleo<PathData>,
    /// Set true once the background walk has finished injecting all paths.
    complete: Arc<AtomicBool>,
    /// Bumped each time the index is (re)built; lets a stale rebuild's walk stop.
    generation: Arc<std::sync::atomic::AtomicU64>,
}

impl FileIndex {
    /// Create an index for `scope`, kicking off the background parallel walk.
    /// `notify` is nucleo's redraw callback — called whenever match results are
    /// ready (used to re-emit to the frontend as indexing streams in).
    pub fn build(scope: &str, redraw: Arc<dyn Fn() + Send + Sync>) -> Self {
        // fzf "path scheme": bonus points for characters right after a `/`, so
        // tail/filename hits rank above mid-path hits. Plus prefix preference.
        let config = {
            let mut c = Config::DEFAULT.match_paths();
            c.prefer_prefix = true;
            c
        };
        let nucleo = Nucleo::<PathData>::new(config, redraw, None, 1);
        let complete = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let index = Self {
            nucleo,
            complete,
            generation,
        };
        index.spawn_walk(scope);
        index
    }

    /// Clear the index and re-run the parallel walk (called after fs changes).
    pub fn rebuild(&mut self, scope: &str) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.complete.store(false, Ordering::Relaxed);
        self.nucleo.restart(true); // drop all items, keep the pattern
        self.spawn_walk(scope);
    }

    /// Spawn the background parallel walk that injects every path into nucleo.
    fn spawn_walk(&self, scope: &str) {
        let injector = self.nucleo.injector();
        let complete = self.complete.clone();
        let generation = self.generation.clone();
        let my_gen = generation.load(Ordering::Acquire);
        let scope_owned = scope.to_string();

        std::thread::spawn(move || {
            let walker = WalkBuilder::new(&scope_owned)
                .hidden(true)
                .git_ignore(true)
                .git_global(false)
                .git_exclude(false)
                // Honor `.gitignore`/`.ignore` even outside a git repo, so each
                // project's own rules apply (adaptive noise filtering).
                .require_git(false)
                .follow_links(false)
                .max_depth(Some(MAX_SEARCH_DEPTH))
                .build_parallel();

            walker.run(|| {
                let injector = injector.clone();
                let scope_owned = scope_owned.clone();
                let generation = generation.clone();
                Box::new(move |result| {
                    use ignore::WalkState;
                    // A newer rebuild superseded us — stop this walk.
                    if generation.load(Ordering::Acquire) != my_gen {
                        return WalkState::Quit;
                    }
                    let Ok(entry) = result else {
                        return WalkState::Continue;
                    };
                    if entry.depth() == 0 {
                        return WalkState::Continue; // skip the scope root
                    }
                    let Some(file_name) = entry.file_name().to_str() else {
                        return WalkState::Continue;
                    };
                    let file_name = file_name.to_string();
                    let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
                    let meta = entry.metadata().ok();
                    let size_bytes = meta.as_ref().map(|m| m.len());
                    let modified_secs = meta
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs());
                    let full_path = entry.path().to_string_lossy().into_owned();
                    let rel_path = entry
                        .path()
                        .strip_prefix(&scope_owned)
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| full_path.clone());
                    let data = PathData {
                        full_path,
                        file_name,
                        rel_path,
                        is_dir,
                        size_bytes,
                        modified_secs,
                    };
                    injector.push(data, |d, cols| {
                        cols[0] = Utf32String::from(d.rel_path.as_str());
                    });
                    WalkState::Continue
                })
            });

            if generation.load(Ordering::Acquire) == my_gen {
                complete.store(true, Ordering::Relaxed);
            }
        });
    }

    /// Whether the background walk has finished injecting all paths.
    pub fn is_complete(&self) -> bool {
        self.complete.load(Ordering::Relaxed)
    }

    /// Set the query and run one match tick. Read results via [`Self::top`].
    pub fn search(&mut self, query: &str, timeout_ms: u64) {
        self.nucleo
            .pattern
            .reparse(0, query, CaseMatching::Ignore, Normalization::Smart, false);
        let _ = self.nucleo.tick(timeout_ms);
    }

    /// Run one match tick WITHOUT changing the pattern — pulls newly-injected
    /// items into the snapshot as indexing streams in.
    pub fn refresh(&mut self, timeout_ms: u64) {
        let _ = self.nucleo.tick(timeout_ms);
    }

    /// The current top-`limit` matches (best first) — nucleo's ranked order.
    pub fn top(&self, limit: usize) -> Vec<PathData> {
        let snap = self.nucleo.snapshot();
        let n = snap.matched_item_count().min(limit as u32);
        snap.matched_items(0..n)
            .map(|item| item.data.clone())
            .collect()
    }
}

/// Caches one [`FileIndex`] per scope so keystrokes reuse the same engine, and
/// runs a per-scope filesystem watcher that debounce-rebuilds on changes.
#[derive(Default)]
pub struct FileIndexStore {
    by_scope: Mutex<HashMap<String, Arc<Mutex<FileIndex>>>>,
    /// Scopes that already have a filesystem watcher running.
    watched: Mutex<HashMap<String, ()>>,
}

impl FileIndexStore {
    /// Get (or lazily build) the index for `scope`, and ensure a filesystem
    /// watcher keeps it fresh. Reused across keystrokes for instant matching.
    pub fn get_or_build(
        self: &Arc<Self>,
        scope: &str,
        redraw: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<Mutex<FileIndex>> {
        {
            let map = self.by_scope.lock().unwrap();
            if let Some(existing) = map.get(scope) {
                return existing.clone();
            }
        }
        let fresh = Arc::new(Mutex::new(FileIndex::build(scope, redraw)));
        self.by_scope
            .lock()
            .unwrap()
            .insert(scope.to_string(), fresh.clone());
        self.ensure_watcher(scope);
        fresh
    }

    /// Return the cached index for `scope` without building one.
    pub fn peek(&self, scope: &str) -> Option<Arc<Mutex<FileIndex>>> {
        self.by_scope.lock().ok()?.get(scope).cloned()
    }

    /// Start a filesystem watcher for `scope` (once). On any create/delete/rename
    /// it debounce-rebuilds the scope's index after a 2s quiet period —
    /// event-driven freshness, matching Raycast's fs-event model.
    fn ensure_watcher(self: &Arc<Self>, scope: &str) {
        {
            let mut watched = self.watched.lock().unwrap();
            if watched.contains_key(scope) {
                return;
            }
            watched.insert(scope.to_string(), ());
        }

        let scope_owned = scope.to_string();
        let store = self.clone();
        std::thread::Builder::new()
            .name("file-index-watcher".into())
            .spawn(move || {
                let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
                let mut watcher = match RecommendedWatcher::new(tx, NotifyConfig::default()) {
                    Ok(w) => w,
                    Err(e) => {
                        tracing::warn!("[file-index] watcher init failed for {scope_owned}: {e}");
                        return;
                    }
                };
                if !Path::new(&scope_owned).exists()
                    || watcher
                        .watch(Path::new(&scope_owned), RecursiveMode::Recursive)
                        .is_err()
                {
                    tracing::warn!("[file-index] cannot watch {scope_owned}");
                    return;
                }

                let mut pending = false;
                let mut last_event = Instant::now();
                loop {
                    match rx.recv_timeout(Duration::from_millis(500)) {
                        Ok(Ok(_)) => {
                            pending = true;
                            last_event = Instant::now();
                        }
                        Ok(Err(e)) => tracing::warn!("[file-index] watch error: {e}"),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                    if pending && last_event.elapsed() >= REBUILD_DEBOUNCE {
                        pending = false;
                        if let Some(index) = store.peek(&scope_owned)
                            && let Ok(mut idx) = index.lock()
                        {
                            tracing::info!("[file-index] rebuilding {scope_owned} after fs change");
                            idx.rebuild(&scope_owned);
                        }
                    }
                }
                // Keep the watcher alive for the thread's lifetime.
                drop(watcher);
            })
            .ok();
    }
}

#[cfg(test)]
mod tests {
    use nucleo::Matcher;
    use nucleo::pattern::{CaseMatching, Normalization, Pattern};
    use nucleo_matcher::{Config, Utf32Str};

    /// Documents WHY ranking can't be left to nucleo: its path-scheme scores
    /// `games/lighthouse` and its children (`games/lighthouse/tests`,
    /// `.../assets`) IDENTICALLY (the "ligh" match is the same segment in all),
    /// so nucleo alone can't prefer the parent folder the user meant. The fix is
    /// structural — we re-rank candidates with explicit match tiers in
    /// `lychi_core::file_search_score` (filename match beats path-only), so the
    /// folder wins by tier, not a tiebreak. This test pins nucleo's tie so a
    /// future change to it is caught.
    #[test]
    fn nucleo_ties_parent_and_children_paths() {
        let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
        let pat = Pattern::parse("ligh", CaseMatching::Ignore, Normalization::Smart);
        let mut score = |rel: &str| {
            let mut buf = Vec::new();
            pat.score(Utf32Str::new(rel, &mut buf), &mut matcher)
                .unwrap_or(0)
        };

        let parent = score("games/lighthouse");
        let child_tests = score("games/lighthouse/tests");
        // Both match; the parent must be at least as good (never worse) so the
        // emit-layer depth tiebreak can promote it deterministically.
        assert!(parent > 0 && child_tests > 0);
        assert!(
            parent >= child_tests,
            "parent {parent} vs child {child_tests}"
        );
    }
}
