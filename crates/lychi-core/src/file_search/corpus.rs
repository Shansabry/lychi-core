//! The shared path store: what files exist, kept fresh.
//!
//! This is the half of file search that is genuinely shared. Walking a home
//! directory costs ~0.4s across all cores and produces ~160k paths; every search
//! wants that same data, so it is built once per scope and reused.
//!
//! What it deliberately does NOT own is a query, a pattern, or a matcher. That
//! separation is the point of this module. Previously one `Nucleo` — one pattern
//! column — was shared by three independent consumers (the `/` stream, nucleo's
//! own redraw callback, and the one-shot `@` completion path), so any of them
//! could reparse the pattern under another's feet and a reader had no way to know
//! whose results it was holding. The observable symptom was searching `readme`
//! and getting the previous query's files: measured at 12 of 15 reads against a
//! real home index while the walk was in flight.
//!
//! Matching state therefore lives in [`super::session::SearchSession`], one per
//! search, mirroring Helix's picker (nucleo's reference consumer) where one
//! picker owns one matcher and exactly one place reads the snapshot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use ignore::WalkBuilder;
use notify::{
    Config as NotifyConfig, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};

/// Max path depth the indexer descends.
const MAX_SEARCH_DEPTH: usize = 10;

/// A directory holding more than this many descendant directories is treated as
/// machine-generated bulk and watched only at its root.
///
/// Chosen from the measured shape of a real home directory rather than picked
/// round: hand-made trees (projects, documents, config) sit in the tens-to-low
/// -hundreds, while generated ones start an order of magnitude above that —
/// on the dev machine, flutter at 3.6k, Postman at 4.9k, the Android SDK at
/// 17k, against a median project well under 100. 1000 sits in the empty gap
/// between those populations, so it separates them without needing to know
/// what any of them are called.
const MAX_WATCHED_SUBTREE: usize = 1000;

/// Quiet period after a filesystem event before rebuilding a scope's paths.
const REBUILD_DEBOUNCE: Duration = Duration::from_secs(2);

/// Floor on how often a full re-walk may run, independent of the debounce.
///
/// The debounce answers "has the filesystem been quiet for 2s?" — under a build
/// or an `npm install` that is satisfied continuously, so on its own it permits
/// a full re-walk every 2s indefinitely. This answers the different question of
/// how often the walk may run at all. 30s is comfortably longer than a walk of a
/// large home directory, so a busy filesystem costs one walk per 30s rather than
/// a permanent treadmill.
const MIN_REWALK_INTERVAL_MS: u64 = 30_000;

/// Milliseconds since an arbitrary fixed point, for interval comparisons only.
///
/// Monotonic: `Instant`-based, so a wall-clock adjustment (NTP, DST, suspend)
/// cannot make the interval check see time move backwards and refuse rebuilds
/// forever.
fn now_ms() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Total paths currently held across every live corpus.
///
/// Read by the health monitor, which runs on its own thread with no access to
/// app state, so this is process-wide rather than per-store. Reported alongside
/// RSS to attribute memory growth: a flat count beside rising anonymous memory
/// means the growth is allocator fragmentation, not a bigger index — and those
/// two have completely different fixes.
static INDEXED_PATHS: AtomicUsize = AtomicUsize::new(0);

/// Snapshot of [`INDEXED_PATHS`].
pub fn indexed_path_count() -> usize {
    INDEXED_PATHS.load(Ordering::Relaxed)
}

/// The payload stored per indexed path.
///
/// Deliberately does NOT carry size or mtime. Getting those means a `statx` per
/// entry during the walk, and the walk visits every path in the scope while
/// only a few hundred are ever ranked and ten or so rendered. Measured warm on
/// a 373k-path scope: **1.71s readdir-only vs 5.77s with metadata, a 3.4× tax**
/// on every rebuild, paid for data almost none of which is read.
///
/// `is_dir` stays because it is free — it comes from `d_type` in the
/// `getdents64` the walk already did, with no extra syscall.
///
/// Size and mtime are fetched by [`stat_now`] for the ranked pool instead
/// (`RANK_POOL` = 400 per query, measured at ~1ms warm).
#[derive(Clone)]
pub struct PathData {
    pub full_path: String,
    pub file_name: String,
    /// Path relative to the search scope — the match column, so nucleo's path
    /// scheme bonuses tail/filename characters (fzf "path scheme").
    pub rel_path: String,
    pub is_dir: bool,
}

/// Size and modification time for one path, read on demand.
///
/// Returns `(None, None)` when the path has gone away between indexing and
/// ranking — a stale corpus entry is normal and must not fail the search.
pub fn stat_now(full_path: &str) -> (Option<u64>, Option<u64>) {
    let Ok(meta) = std::fs::metadata(full_path) else {
        return (None, None);
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    (Some(meta.len()), modified)
}

/// The traversal rules that define what "indexed" means.
///
/// One definition, used by both the indexing walk and the watcher's directory
/// enumeration. They must not drift: watching a superset wastes kernel watches,
/// watching a subset silently leaves parts of the corpus stale.
fn index_walker(scope: &str) -> WalkBuilder {
    let mut b = WalkBuilder::new(scope);
    b.hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        // Honor `.gitignore`/`.ignore` even outside a git repo, so each
        // project's own rules apply (adaptive noise filtering).
        .require_git(false)
        .follow_links(false)
        .max_depth(Some(MAX_SEARCH_DEPTH));
    b
}

/// Would this path appear in the corpus?
///
/// The watcher's event filter. It must agree with [`index_walker`]'s
/// `hidden(true)`: a non-recursive watch on a directory still reports its
/// hidden children, so without this an event for a dotfile we never index
/// triggers a full rebuild. Checking every component (not just the file name)
/// catches `~/.cache/foo/bar.txt`, where only an ancestor is hidden.
///
/// Deliberately NOT re-running the ignore rules here: that would mean building
/// a `WalkBuilder` per event, and a `.gitignore`d file inside an indexed tree
/// is a rare enough false positive to be worth the simplicity. Hidden paths
/// are the ones that fire constantly.
fn is_indexable(path: &Path) -> bool {
    use std::path::Component;
    !path.components().any(|c| {
        // Only Normal components can be hidden. `.` and `..` are path syntax,
        // and matching them on a leading dot would reject every relative path.
        matches!(c, Component::Normal(s)
            if s.to_str().is_some_and(|s| s.starts_with('.')))
    })
}

/// Does this event mean the set of files on disk changed?
///
/// The corpus answers "which files exist"; only create/modify/remove can change
/// that answer. `notify` documents `Access` as explicitly *non-mutating* —
/// opening, reading, or executing a file — so reacting to it rebuilds in
/// response to nothing having changed.
///
/// That is not a theoretical worry, it is a feedback loop. `notify`'s inotify
/// backend requests `IN_OPEN` in its watch mask unconditionally, so *every file
/// the rebuild's own walk reads* is reported back to us as an event. The
/// rebuild therefore schedules the next rebuild, forever, at whatever rate a
/// full walk takes — measured at one every ~2.4s on a completely idle machine,
/// with RSS climbing on each pass. It was latent from the first day of the
/// watcher and only became visible once the watches actually succeeded.
fn is_change(kind: &EventKind) -> bool {
    !matches!(kind, EventKind::Access(_))
}

/// Descendant-directory count for every directory, from one pass over a
/// sorted path list. Each directory contributes 1 to each of its
/// ancestors, so counts are exact rather than sampled.
fn subtree_sizes(dirs: &[PathBuf]) -> HashMap<PathBuf, usize> {
    let mut counts = HashMap::with_capacity(dirs.len());
    for dir in dirs {
        let mut cur = dir.as_path();
        while let Some(parent) = cur.parent() {
            *counts.entry(parent.to_path_buf()).or_insert(0usize) += 1;
            cur = parent;
        }
    }
    counts
}

/// Directories to watch: the indexed set, minus machine-generated bulk.
///
/// Watching is not free — each inotify watch costs kernel memory plus userspace
/// bookkeeping, and watching all indexed directories on the dev machine cost
/// ~580MB RSS. But indexing them is cheap. So the two sets are allowed to differ
/// HERE and only here: everything stays searchable, and only live-update
/// coverage is traded away.
///
/// What gets pruned is decided by subtree SIZE, not by name. A directory holding
/// thousands of descendants is machine-generated — an SDK, a toolchain, a
/// package cache — regardless of what it's called. A hardcoded list of names
/// would have caught none of the ones actually found.
///
/// The cost of pruning one: a file created inside it won't show up until the
/// next restart. For an SDK's internals that's the right trade.
fn walk_watched_dirs(scope: &str) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = index_walker(scope)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_dir()))
        .map(|e| e.into_path())
        .collect();

    // Sorted paths put every subtree in one contiguous run, so a single pass can
    // skip a pruned directory's descendants without a second traversal.
    dirs.sort_unstable();

    let counts = subtree_sizes(&dirs);
    let scope_root = Path::new(scope);
    let mut kept = Vec::with_capacity(dirs.len());
    let mut pruned: Option<PathBuf> = None;
    for dir in dirs {
        if let Some(root) = &pruned {
            if dir.starts_with(root) {
                continue;
            }
            pruned = None;
        }
        // The scope root is never a prune candidate. It has more descendants
        // than anything under it by definition, so testing it would prune the
        // entire tree and watch nothing — the exact opposite of the goal.
        // (Caught by prune_skips_only_the_oversized_subtree.)
        if dir == scope_root {
            kept.push(dir);
            continue;
        }
        if counts.get(&dir).copied().unwrap_or(0) > MAX_WATCHED_SUBTREE {
            tracing::debug!(
                "[corpus] not watching {} ({} dirs — machine-generated bulk)",
                dir.display(),
                counts.get(&dir).copied().unwrap_or(0)
            );
            // Keep the root itself: creating a file directly inside it is
            // plausible; churn happens further down.
            kept.push(dir.clone());
            pruned = Some(dir);
            continue;
        }
        kept.push(dir);
    }
    kept
}

/// One indexed path, reference-counted.
///
/// Sessions share these rather than copying them. A `PathData` is six fields
/// including three `String`s, so a per-session clone of a 162k-path corpus cost
/// ~330MB of anonymous memory per search — measured, not estimated. An `Arc`
/// clone is a refcount bump, so N concurrent sessions cost one copy of the data.
pub type SharedPath = Arc<PathData>;

/// An immutable set of paths for one scope, plus the generation it was built at.
///
/// Handed out as an `Arc` so a search can hold it without blocking a rebuild:
/// rebuilds publish a *new* snapshot rather than mutating this one. A search
/// already in flight keeps working against the paths it started with, which is
/// correct — it finishes answering the question it was asked.
pub struct Paths {
    pub items: Vec<SharedPath>,
    /// Bumped per rebuild. A search records this and can tell whether the corpus
    /// moved underneath it.
    pub generation: u64,
}

/// The shared, long-lived path store for one scope.
///
/// Owns the walk and the filesystem watcher. Deliberately owns no query state —
/// see the module docs for why that separation exists.
pub struct PathCorpus {
    scope: String,
    /// The current path snapshot. `RwLock` because reads (starting a search) are
    /// frequent and writes (rebuilds) are rare; a reader clones the `Arc` and
    /// releases the lock immediately.
    paths: RwLock<Arc<Paths>>,
    /// Bumped each time a rebuild starts, so a superseded walk can stop early.
    generation: AtomicU64,
    /// Counts published snapshots. Distinct from `generation` (which counts
    /// rebuild *attempts*) so the initial empty snapshot can never be mistaken
    /// for a walked one.
    published: AtomicU64,
    /// False until the first walk has finished.
    complete: AtomicBool,
    /// A walk thread is running right now. At most one may be in flight.
    ///
    /// Without this, `rebuild` spawned a thread per call and the debounce only
    /// bounded how *bursty* the calls were, not how many ran at once. Each walk
    /// accumulates its own `Vec` of up to the whole scope before publishing, so N
    /// concurrent walks meant N copies of the corpus resident simultaneously.
    /// Superseded walks do quit, but only when they next reach the generation
    /// check — after they have already allocated.
    ///
    /// Same guard the app-index watcher has always had (`desktop_apps/watcher.rs`).
    walking: AtomicBool,
    /// When the last walk finished, as millis since process start. Pairs with
    /// [`MIN_REWALK_INTERVAL`] to put a floor on the re-walk *rate* — the
    /// debounce only ever governed the quiet period before one.
    last_walk_ms: AtomicU64,
    /// A change arrived while a walk was in flight, so re-walk when it lands.
    /// Without this a guarded rebuild would simply drop changes that arrive
    /// mid-walk, leaving the corpus silently stale — trading a memory bug for a
    /// correctness one.
    dirty: AtomicBool,
    /// Called whenever a new snapshot is published, so live searches re-run.
    on_change: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
}

impl PathCorpus {
    /// Create an empty corpus for `scope` and kick off the first walk.
    pub fn new(scope: &str) -> Arc<Self> {
        let corpus = Self::new_unstarted(scope);
        corpus.start_walk();
        corpus
    }

    /// Create the corpus WITHOUT walking, so subscribers can register first.
    pub fn new_unstarted(scope: &str) -> Arc<Self> {
        Arc::new(Self {
            scope: scope.to_string(),
            paths: RwLock::new(Arc::new(Paths {
                items: Vec::new(),
                generation: 0,
            })),
            generation: AtomicU64::new(0),
            published: AtomicU64::new(0),
            complete: AtomicBool::new(false),
            walking: AtomicBool::new(false),
            last_walk_ms: AtomicU64::new(0),
            dirty: AtomicBool::new(false),
            on_change: Mutex::new(Vec::new()),
        })
    }

    /// Start the background walk. Idempotent per corpus by construction — only
    /// `new`/`CorpusStore::start` call it, each exactly once.
    ///
    /// Goes through `begin_walk` rather than `spawn_walk` so the very first walk
    /// claims the in-flight flag like any other. Bypassing it would let a watcher
    /// event during the initial walk start a second concurrent one.
    pub fn start_walk(self: &Arc<Self>) {
        self.begin_walk();
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// The current paths. Cheap: clones an `Arc`, never the path list.
    pub fn paths(&self) -> Arc<Paths> {
        self.paths.read().unwrap().clone()
    }

    /// Whether the first walk has finished. A search can still run before this;
    /// it just sees fewer paths and re-runs when the corpus changes.
    pub fn is_complete(&self) -> bool {
        self.complete.load(Ordering::Relaxed)
    }

    /// Register a callback fired when a new snapshot is published.
    ///
    /// This is how a live search learns to re-match: the corpus does not know
    /// what a search is, only that someone wants to be told when paths change.
    pub fn subscribe(&self, f: Arc<dyn Fn() + Send + Sync>) {
        self.on_change.lock().unwrap().push(f);
    }

    fn notify_subscribers(&self) {
        // Cloned out of the lock: a subscriber may start a search, which reads
        // the corpus, and holding `on_change` across that invites a deadlock.
        let subs = self.on_change.lock().unwrap().clone();
        for f in subs {
            f();
        }
    }

    /// Publish a fresh path list, replacing whatever was there.
    ///
    /// The published generation counts *publishes*, not rebuild attempts. It has
    /// to: consumers compare "the snapshot I hold" against "the newest snapshot"
    /// to decide whether they are stale, so the initial empty snapshot must not
    /// share a number with the first real one. It previously did — both were 0 —
    /// which made a search started before the walk finished decide it was already
    /// up to date and stay empty forever.
    fn publish(&self, items: Vec<SharedPath>) {
        INDEXED_PATHS.store(items.len(), Ordering::Relaxed);
        let generation = self.published.fetch_add(1, Ordering::AcqRel) + 1;
        *self.paths.write().unwrap() = Arc::new(Paths { items, generation });
        self.notify_subscribers();
    }

    /// Walk the scope in parallel and publish the result.
    ///
    /// Collects into a `Vec` and publishes once, rather than streaming into a
    /// matcher. Searches are cheap to re-run against a finished list, and a
    /// single atomic swap means no consumer ever observes a half-built corpus —
    /// which is what made partial state observable before.
    fn spawn_walk(self: &Arc<Self>) {
        let me = self.clone();
        let my_gen = self.generation.load(Ordering::Acquire);
        std::thread::Builder::new()
            .name("lychi-corpus-walk".into())
            .spawn(move || {
                let collected: Mutex<Vec<SharedPath>> = Mutex::new(Vec::new());
                index_walker(&me.scope).build_parallel().run(|| {
                    let collected = &collected;
                    let me = me.clone();
                    Box::new(move |result: Result<ignore::DirEntry, ignore::Error>| {
                        use ignore::WalkState;
                        // A newer rebuild superseded us — stop this walk.
                        if me.generation.load(Ordering::Acquire) != my_gen {
                            return WalkState::Quit;
                        }
                        let Ok(entry) = result else {
                            return WalkState::Continue;
                        };
                        if entry.depth() == 0 {
                            return WalkState::Continue; // skip the scope root
                        }
                        if let Some(data) = to_path_data(&entry, &me.scope) {
                            // Batched per walk thread would be faster, but the
                            // parallel walk hands each thread its own closure
                            // and this lock is held for a push, not a walk.
                            collected.lock().unwrap().push(Arc::new(data));
                        }
                        WalkState::Continue
                    })
                });

                if me.generation.load(Ordering::Acquire) != my_gen {
                    // Superseded; the newer walk owns the corpus. Still release
                    // the in-flight flag — an early return that skipped it would
                    // wedge the corpus permanently un-rebuildable.
                    me.finish_walk();
                    return;
                }
                let items = collected.into_inner().unwrap();
                me.publish(items);
                me.complete.store(true, Ordering::Relaxed);
                me.finish_walk();
            })
            .ok();
    }

    /// Release the in-flight flag and re-walk if changes arrived meanwhile.
    ///
    /// The re-walk is scheduled on a short timer rather than started inline: a
    /// dirty flag set during a walk usually means the filesystem is still busy,
    /// and starting immediately would reproduce the every-2s treadmill this
    /// guard exists to stop. Waiting out `MIN_REWALK_INTERVAL_MS` lets the burst
    /// settle and collapses many changes into one walk.
    fn finish_walk(self: &Arc<Self>) {
        self.last_walk_ms.store(now_ms(), Ordering::Release);
        self.walking.store(false, Ordering::Release);

        if !self.dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        let me = self.clone();
        std::thread::Builder::new()
            .name("lychi-corpus-redo".into())
            .spawn(move || {
                std::thread::sleep(Duration::from_millis(MIN_REWALK_INTERVAL_MS));
                // Re-check rather than force: the watcher may have already
                // started one while we slept.
                if !me.begin_walk() {
                    // Refused because a walk is running — that walk will observe
                    // `dirty` itself, so the change is not lost.
                    tracing::debug!("[corpus] deferred re-walk folded into a running one");
                }
            })
            .ok();
    }

    /// Re-walk and republish (called after filesystem changes).
    ///
    /// Returns `false` when the request was absorbed rather than started: either
    /// a walk is already running, or the last one finished too recently. In both
    /// cases the corpus is marked dirty and the walk happens when it can, so a
    /// refused rebuild delays the refresh — it never drops it.
    fn rebuild(self: &Arc<Self>) -> bool {
        // Rate floor. The watcher's debounce governs the quiet period BEFORE a
        // rebuild; it says nothing about how often rebuilds may happen. Under
        // sustained churn (a build, an npm install) the debounce is satisfied
        // every 2s forever, so without this a full re-walk ran every 2s.
        let since_last = now_ms().saturating_sub(self.last_walk_ms.load(Ordering::Acquire));
        if since_last < MIN_REWALK_INTERVAL_MS {
            self.dirty.store(true, Ordering::Release);
            return false;
        }
        self.begin_walk()
    }

    /// Start a walk if none is running. The single place `walking` is claimed.
    fn begin_walk(self: &Arc<Self>) -> bool {
        if self.walking.swap(true, Ordering::AcqRel) {
            // Already walking — record that the result will be out of date.
            self.dirty.store(true, Ordering::Release);
            return false;
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.complete.store(false, Ordering::Relaxed);
        self.spawn_walk();
        true
    }
}

/// Build the stored payload for one walk entry.
fn to_path_data(entry: &ignore::DirEntry, scope: &str) -> Option<PathData> {
    let file_name = entry.file_name().to_str()?.to_string();
    // `file_type()` is free — `d_type` from the getdents64 the walk already did.
    // `metadata()` is NOT: it is a statx per entry. See `PathData`.
    let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
    let full_path = entry.path().to_string_lossy().into_owned();
    let rel_path = entry
        .path()
        .strip_prefix(scope)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| full_path.clone());
    Some(PathData {
        full_path,
        file_name,
        rel_path,
        is_dir,
    })
}

/// Per-scope corpora, built lazily and kept for the session.
#[derive(Default)]
pub struct CorpusStore {
    by_scope: Mutex<HashMap<String, Arc<PathCorpus>>>,
    watched: Mutex<HashMap<String, ()>>,
}

impl CorpusStore {
    /// The corpus for `scope`, building it (and its watcher) on first use.
    pub fn get_or_build(self: &Arc<Self>, scope: &str) -> Arc<PathCorpus> {
        self.get_or_build_tracked(scope).0
    }

    /// As [`Self::get_or_build`], also reporting whether it was created now.
    ///
    /// A caller that subscribes to the corpus needs to do so exactly once, and
    /// needs to do it before the first walk publishes — so it has to be able to
    /// tell "I built this" from "it already existed".
    pub fn get_or_build_tracked(self: &Arc<Self>, scope: &str) -> (Arc<PathCorpus>, bool) {
        // Held across creation so two racing callers cannot both build (and both
        // believe they were first, double-subscribing).
        let mut map = self.by_scope.lock().unwrap();
        if let Some(existing) = map.get(scope) {
            return (existing.clone(), false);
        }
        let fresh = PathCorpus::new_unstarted(scope);
        map.insert(scope.to_string(), fresh.clone());
        drop(map);
        (fresh, true)
    }

    /// Begin the walk and the watcher for a corpus returned as newly-created.
    ///
    /// Split from creation so a caller can subscribe FIRST: `publish` notifies
    /// subscribers before `is_complete` flips, so subscribing after the walk
    /// starts can miss the only notification that mattered.
    pub fn start(self: &Arc<Self>, corpus: &Arc<PathCorpus>) {
        corpus.start_walk();
        self.ensure_watcher(corpus.scope(), corpus);
    }

    /// Already-built corpus for `scope`, if any. Never builds.
    pub fn peek(&self, scope: &str) -> Option<Arc<PathCorpus>> {
        self.by_scope.lock().unwrap().get(scope).cloned()
    }

    /// Start a filesystem watcher for `scope` (once). On any create/delete/
    /// rename it debounce-rebuilds the corpus after a 2s quiet period.
    fn ensure_watcher(&self, scope: &str, corpus: &Arc<PathCorpus>) {
        {
            let mut watched = self.watched.lock().unwrap();
            if watched.contains_key(scope) {
                return;
            }
            watched.insert(scope.to_string(), ());
        }
        let scope_owned = scope.to_string();
        let corpus = corpus.clone();
        std::thread::Builder::new()
            .name("lychi-corpus-watch".into())
            .spawn(move || {
                let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
                let mut watcher = match RecommendedWatcher::new(tx, NotifyConfig::default()) {
                    Ok(w) => w,
                    Err(e) => {
                        tracing::warn!("[corpus] watcher init failed for {scope_owned}: {e}");
                        return;
                    }
                };

                // A recursive inotify watch costs one watch per directory, so
                // asking for `$HOME` recursively means watching everything —
                // including the generated trees the walk itself skips, an
                // overshoot that blows past fs.inotify.max_user_watches and
                // fails wholesale. Enumerating the dirs we care about and
                // watching each non-recursively is the standard fix (and what
                // editors/indexers do): fewer watches, and a directory the walk
                // would ignore can no longer consume one.
                let mut watched = 0usize;
                let mut failed: Option<notify::Error> = None;
                for dir in walk_watched_dirs(&scope_owned) {
                    match watcher.watch(&dir, RecursiveMode::NonRecursive) {
                        Ok(()) => watched += 1,
                        // Once the kernel limit is hit every subsequent watch
                        // fails the same way, so keep the first error and stop.
                        Err(e) => {
                            failed = Some(e);
                            break;
                        }
                    }
                }

                if let Some(e) = failed {
                    // ENOSPC (28) = out of watches, EMFILE (24) = out of
                    // inotify instances. Both are raised by sysctl, so name
                    // the knob instead of reporting "no space left on device".
                    let hint = match e.kind {
                        notify::ErrorKind::Io(ref io) => match io.raw_os_error() {
                            Some(28) => " — raise fs.inotify.max_user_watches",
                            Some(24) => " — raise fs.inotify.max_user_instances",
                            _ => "",
                        },
                        _ => "",
                    };
                    tracing::warn!(
                        "[corpus] watch incomplete for {scope_owned} after {watched} dirs: \
                         {e}{hint} — live updates cover only part of the tree"
                    );
                } else {
                    tracing::debug!("[corpus] watching {watched} dirs under {scope_owned}");
                }

                // Nothing watched at all — no point parking a thread on a
                // channel that will never receive.
                if watched == 0 {
                    tracing::warn!(
                        "[corpus] no watchable dirs under {scope_owned} — search still \
                         works, but new files need a restart to appear"
                    );
                    return;
                }

                let mut pending = false;
                let mut last_event = Instant::now();
                loop {
                    match rx.recv_timeout(Duration::from_millis(500)) {
                        Ok(Ok(ev)) => {
                            // Two independent filters, both required. The kind
                            // check drops reads (including the ones our own
                            // rebuild walk causes — see `is_change`); the path
                            // check drops writes to files we'd never index.
                            if is_change(&ev.kind) && ev.paths.iter().any(|p| is_indexable(p)) {
                                pending = true;
                                last_event = Instant::now();
                            }
                        }
                        Ok(Err(e)) => tracing::warn!("[corpus] watch error: {e}"),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                    if pending && last_event.elapsed() >= REBUILD_DEBOUNCE {
                        pending = false;
                        if corpus.rebuild() {
                            tracing::info!("[corpus] rebuilding {scope_owned} after fs change");
                        } else {
                            // Absorbed, not dropped: the corpus is marked dirty
                            // and will re-walk once the in-flight walk finishes
                            // or the rate floor elapses.
                            tracing::debug!(
                                "[corpus] rebuild of {scope_owned} deferred (walk in flight \
                                 or within rate floor)"
                            );
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
    use super::*;

    /// Reads are not changes. `notify`'s inotify backend always requests
    /// `IN_OPEN`, so every file the rebuild's own walk reads comes back as
    /// `Access(Open)`. If those counted, the rebuild would schedule the next
    /// rebuild forever.
    #[test]
    fn reads_are_not_changes() {
        use notify::event::{AccessKind, AccessMode};
        for kind in [
            EventKind::Access(AccessKind::Open(AccessMode::Any)),
            EventKind::Access(AccessKind::Open(AccessMode::Read)),
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
            EventKind::Access(AccessKind::Read),
            EventKind::Access(AccessKind::Any),
        ] {
            assert!(!is_change(&kind), "{kind:?} is a read, not a change");
        }
    }

    /// The other half: a filter that dropped everything would also stop the
    /// loop, while silently freezing the corpus.
    #[test]
    fn mutations_are_changes() {
        use notify::event::{CreateKind, ModifyKind, RemoveKind, RenameMode};
        for kind in [
            EventKind::Create(CreateKind::File),
            EventKind::Create(CreateKind::Folder),
            EventKind::Remove(RemoveKind::File),
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            // `Any`/`Other` are notify's "unknown kernel bitmask" cases. Treat
            // them as changes: a missed rebuild is a silently stale corpus,
            // which is worse than one redundant walk.
            EventKind::Any,
            EventKind::Other,
        ] {
            assert!(is_change(&kind), "{kind:?} must rebuild");
        }
    }

    /// Lychi's own database lives under ~/.local/share/lychi, and every write to
    /// it used to rebuild the whole corpus.
    #[test]
    fn hidden_paths_do_not_trigger_rebuilds() {
        for p in [
            "/home/u/.local/share/lychi/lychi.redb",
            "/home/u/.claude.json",
            "/home/u/.cache/plasmashell/qqpc_opengl",
            "/home/u/.config/lychi/config.toml",
        ] {
            assert!(!is_indexable(Path::new(p)), "{p} must not rebuild");
        }
    }

    #[test]
    fn visible_paths_still_trigger_rebuilds() {
        for p in [
            "/home/u/work/report.md",
            "/home/u/Documents/notes/todo.txt",
            "/home/u/src/project/main.rs",
        ] {
            assert!(is_indexable(Path::new(p)), "{p} must rebuild");
        }
    }

    /// `..` is `Component::ParentDir`, whose `as_os_str()` is `".."` — matching
    /// a leading dot on it would reject every relative path.
    #[test]
    fn dot_components_are_not_hidden() {
        assert!(is_indexable(Path::new("../sibling/file.txt")));
        assert!(is_indexable(Path::new("./file.txt")));
    }

    /// A corpus on a path that cannot be walked, so `spawn_walk` finishes
    /// immediately and the tests observe only the guard logic.
    fn test_corpus() -> Arc<PathCorpus> {
        PathCorpus::new_unstarted("/nonexistent-scope-for-tests")
    }

    /// Move the corpus past the rate floor, so the IN-FLIGHT guard is the only
    /// control under test.
    ///
    /// `now_ms()` counts from first call, which in a test binary is near zero —
    /// so leaving `last_walk_ms` at its default puts the corpus permanently
    /// *inside* the floor and the rate check refuses before the in-flight guard
    /// is ever reached. An earlier draft of these tests did exactly that and
    /// passed with the in-flight guard deleted.
    fn move_past_rate_floor(c: &PathCorpus) {
        c.last_walk_ms.store(
            now_ms().saturating_sub(MIN_REWALK_INTERVAL_MS + 1),
            Ordering::Release,
        );
    }

    /// The bug: `rebuild` spawned a walk thread per call, so sustained
    /// filesystem churn stacked N concurrent walks, each accumulating its own
    /// full copy of the corpus. Only one may be in flight.
    #[test]
    fn a_second_walk_is_refused_while_one_is_in_flight() {
        let c = test_corpus();
        move_past_rate_floor(&c);
        // Claim the flag the way a running walk does, without spawning one.
        assert!(!c.walking.swap(true, Ordering::AcqRel));

        assert!(
            !c.begin_walk(),
            "a walk must be refused while another is in flight"
        );
        assert!(
            c.dirty.load(Ordering::Acquire),
            "a refused walk must mark the corpus dirty, not drop the change"
        );
    }

    /// Same guard, reached through the watcher's entry point rather than
    /// directly — the path that actually stacked the concurrent walks.
    #[test]
    fn rebuild_is_refused_while_a_walk_is_in_flight() {
        let c = test_corpus();
        move_past_rate_floor(&c);
        c.walking.store(true, Ordering::Release);

        assert!(
            !c.rebuild(),
            "rebuild must be refused while a walk is in flight, even past the rate floor"
        );
    }

    /// The other half: refusing forever would trade a memory bug for a stale
    /// corpus. `finish_walk` must consume the deferred change, not discard it.
    #[test]
    fn a_refused_walk_is_deferred_not_dropped() {
        let c = test_corpus();
        move_past_rate_floor(&c);
        c.walking.store(true, Ordering::Release);
        c.begin_walk();
        assert!(c.dirty.load(Ordering::Acquire), "the change is remembered");

        c.finish_walk();
        assert!(
            !c.walking.load(Ordering::Acquire),
            "finish_walk must release the in-flight flag"
        );
        assert!(
            !c.dirty.load(Ordering::Acquire),
            "finish_walk must consume the dirty flag it acts on"
        );
    }

    /// The rate floor is a SEPARATE control from the watcher's debounce. The
    /// debounce asks "has the tree been quiet for 2s?", which sustained churn
    /// satisfies over and over; this asks "may a walk run at all yet?".
    #[test]
    fn rebuilds_within_the_rate_floor_are_refused() {
        let c = test_corpus();
        c.last_walk_ms.store(now_ms(), Ordering::Release);
        assert!(!c.walking.load(Ordering::Acquire), "no walk in flight");

        assert!(
            !c.rebuild(),
            "a rebuild within the rate floor must be refused even with no walk running"
        );
        assert!(c.dirty.load(Ordering::Acquire), "and must be remembered");
    }

    /// A superseded walk returns early. If that path skipped `finish_walk`, the
    /// flag would stay set and the corpus would never rebuild again.
    #[test]
    fn finish_walk_releases_the_flag_so_the_next_walk_can_start() {
        let c = test_corpus();
        c.walking.store(true, Ordering::Release);
        c.finish_walk();
        move_past_rate_floor(&c);

        assert!(
            c.begin_walk(),
            "after finish_walk, a new walk must be able to start"
        );
    }

    /// The interval must be long enough to outlast a walk, or the floor cannot
    /// stop the treadmill it exists to stop.
    #[test]
    fn rate_floor_is_longer_than_the_debounce() {
        assert!(
            MIN_REWALK_INTERVAL_MS > REBUILD_DEBOUNCE.as_millis() as u64,
            "a floor shorter than the debounce would never bind"
        );
    }

    #[test]
    fn subtree_sizes_count_all_descendants() {
        let dirs: Vec<PathBuf> = ["/a", "/a/b", "/a/b/c", "/a/d"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let counts = subtree_sizes(&dirs);
        assert_eq!(counts.get(Path::new("/a")).copied(), Some(3));
        assert_eq!(counts.get(Path::new("/a/b")).copied(), Some(1));
    }
}
