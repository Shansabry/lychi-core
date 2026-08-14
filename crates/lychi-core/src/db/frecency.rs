use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use redb::{Database, Durability, ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};

use crate::error::LychiError;

use super::FRECENCY;

/// The frecency database, registered once at startup.
///
/// Frecency lives in its OWN redb file (`frecency.redb`), not the user-data
/// `lychi.redb`. It is derived, device-local, machine-learned ranking data — the
/// one hot-path/keyed/multi-process store where redb (its in-memory cache below,
/// its B-tree prefix scans, its multi-process locking) is the right engine, so it
/// stays in redb rather than moving to a flat file. A separate file keeps
/// `lychi.redb` to user-authored content and isolates frecency's growth and any
/// corruption from the user's notes/todos/snippets.
///
/// Every public function reads this global handle rather than taking a `db`
/// argument — the record/get call sites (60+, on the hot path and deep in
/// handlers) do not thread a database around. Unset in tests until `init_store`
/// (or `set_store_for_test`) runs; an unset store makes writes a no-op and reads
/// empty, which degrades ranking to neutral rather than panicking.
static STORE: OnceLock<Arc<Database>> = OnceLock::new();

/// Register the frecency database. Called once at app startup.
pub fn init_store(db: Arc<Database>) {
    let _ = STORE.set(db);
}

/// The registered frecency database, if any.
///
/// In production this is a single `OnceLock` load (one atomic read, no lock) on
/// the per-keystroke path. Under `cfg(test)` it first consults a per-thread
/// override so each `#[test]` gets its own isolated database without fighting the
/// set-once `STORE` (tests run in parallel in one process, and many assert exact
/// table contents). The override branch is compiled out of release builds.
#[cfg(not(test))]
fn store() -> Option<Arc<Database>> {
    // Cloning the Arc is a single atomic increment — negligible on the hot path,
    // and it unifies the return type with the test override below.
    STORE.get().cloned()
}

#[cfg(test)]
thread_local! {
    static TEST_STORE: std::cell::RefCell<Option<Arc<Database>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn store() -> Option<Arc<Database>> {
    let overridden = TEST_STORE.with(|s| s.borrow().clone());
    overridden.or_else(|| STORE.get().cloned())
}

/// Point this thread's frecency store at `db` (test-only). Each test calls this
/// with its own `open_test_database()` so it is fully isolated. `pub(crate)` so
/// tests in other modules (executor, suggestions, zero_state) that exercise
/// frecency can register their own store the same way.
#[cfg(test)]
pub(crate) fn set_store_for_test(db: Arc<Database>) {
    // The frecency table must exist in a bare test database.
    {
        let txn = db.begin_write().unwrap();
        txn.open_table(FRECENCY).unwrap();
        txn.commit().unwrap();
    }
    TEST_STORE.with(|s| *s.borrow_mut() = Some(db));
    // A fresh store must not serve another test's cached entries.
    invalidate();
}

/// Open a write transaction with per-write fsync DISABLED (`Durability::None`).
///
/// Frecency is not critical data: losing the last few ranking updates on a crash
/// just means a suggestion ranks slightly stale and self-corrects on next use. So
/// we skip the fsync every `commit()` would otherwise do — the write reaches the
/// OS page cache and is flushed on the OS's own schedule. This is the one real
/// perf lever for a per-keystroke keyed store, and it keeps the crash guarantee a
/// file+periodic-flush design would give, without leaving redb.
fn begin_write(db: &Arc<Database>) -> Result<redb::WriteTransaction, LychiError> {
    let mut txn = db.begin_write()?;
    // Best-effort: if the redb version returns a Result here, a failure just means
    // this commit stays durable (fsync'd) — correct, only slightly slower.
    let _ = txn.set_durability(Durability::None);
    Ok(txn)
}

/// Frecency entry: tracks access frequency and recency for a single item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrecencyEntry {
    /// Total number of times this item was accessed.
    pub count: u32,
    /// Timestamps of last N accesses (ring buffer, keep last 10).
    /// Milliseconds since UNIX epoch.
    pub recent_timestamps: Vec<u64>,
}

const MAX_RECENT: usize = 10;

impl FrecencyEntry {
    fn new(now_ms: u64) -> Self {
        Self {
            count: 1,
            recent_timestamps: vec![now_ms],
        }
    }

    /// Record a new access.
    pub fn record_access(&mut self, now_ms: u64) {
        self.count += 1;
        self.recent_timestamps.push(now_ms);
        if self.recent_timestamps.len() > MAX_RECENT {
            self.recent_timestamps.remove(0);
        }
    }

    /// Calculate frecency score (0.0 to 1.0, normalized).
    ///
    /// Recency weights:
    /// - Last hour:  1.0
    /// - Last day:   0.7
    /// - Last week:  0.4
    /// - Older:      0.1
    pub fn score(&self, now_ms: u64) -> f64 {
        let mut total = 0.0;
        for &ts in &self.recent_timestamps {
            let age_hours = (now_ms.saturating_sub(ts)) as f64 / 3_600_000.0;
            let weight = if age_hours < 1.0 {
                1.0
            } else if age_hours < 24.0 {
                0.7
            } else if age_hours < 168.0 {
                // 7 days
                0.4
            } else {
                0.1
            };
            total += weight;
        }
        // Normalize: max possible = MAX_RECENT * 1.0
        (total / MAX_RECENT as f64).min(1.0)
    }

    /// Circadian affinity multiplier (1.0 neutral, up to 1.15) from the hours at
    /// which this item was historically used. If the current hour matches the
    /// item's usage pattern (within ±1h), boost slightly. Reuses the timestamps
    /// already stored — zero extra storage. A tiebreaker, never a driver.
    pub fn time_affinity(&self, now_ms: u64) -> f64 {
        use chrono::Timelike;
        if self.recent_timestamps.len() < 2 {
            return 1.0; // too little data to infer a routine
        }
        let hour_of = |ms: u64| -> Option<u32> {
            chrono::DateTime::from_timestamp_millis(ms as i64).map(|dt| dt.hour())
        };
        let Some(now_hour) = hour_of(now_ms) else {
            return 1.0;
        };
        let within_band = |h: u32| {
            let diff = (h as i64 - now_hour as i64).rem_euclid(24);
            diff <= 1 || diff >= 23 // ±1 hour, wrapping midnight
        };
        let matches = self
            .recent_timestamps
            .iter()
            .filter_map(|&ts| hour_of(ts))
            .filter(|&h| within_band(h))
            .count();
        let frac = matches as f64 / self.recent_timestamps.len() as f64;
        // frac 0 → 1.0 (no affinity), frac 1 → 1.15 (strong routine match).
        1.0 + 0.15 * frac
    }
}

/// Record an access for a given key.
pub fn record(key: &str) -> Result<(), LychiError> {
    let Some(db) = store() else {
        return Ok(());
    };
    let now_ms = super::now_millis();
    let txn = begin_write(&db)?;
    {
        let mut table = txn.open_table(FRECENCY)?;

        let entry = match table.get(key)? {
            Some(existing) => {
                let mut entry: FrecencyEntry =
                    crate::db::decode_value(existing.value()).unwrap_or(FrecencyEntry::new(now_ms));
                entry.record_access(now_ms);
                entry
            }
            None => FrecencyEntry::new(now_ms),
        };

        let bytes = crate::db::encode_row(&entry)?;
        table.insert(key, bytes.as_slice())?;
    }
    commit_write(txn)
}

/// Record a workspace-scoped access.
///
/// Key format: `ws:<project_root>:<command>` — scoped frecency that tracks
/// which commands are frequently used in which project directory.
pub fn record_workspace(project_root: &str, command: &str) -> Result<(), LychiError> {
    let normalized = project_root.trim_end_matches('/');
    let key = format!("ws:{normalized}:{command}");
    record(&key)
}

/// Record that a command ran in a specific repo within a multi-repo container.
/// Learns the user's active repo per container (`repo:<container>:<repo_path>`),
/// so the most-used repo becomes the silent default.
pub fn record_repo_choice(container: &str, repo_path: &str) -> Result<(), LychiError> {
    let container = container.trim_end_matches('/');
    let key = format!("repo:{container}:{repo_path}");
    record(&key)
}

/// Frecency scores of repos chosen within a container: `repo_path -> score`.
pub fn get_repo_choice_scores(container: &str) -> HashMap<String, f64> {
    let container = container.trim_end_matches('/');
    let prefix = format!("repo:{container}:");
    get_scores()
        .into_iter()
        .filter_map(|(key, score)| key.strip_prefix(&prefix).map(|r| (r.to_string(), score)))
        .collect()
}

/// Get workspace-scoped frecency scores for a specific project.
///
/// Returns a map of `command -> score` for commands previously run in this project.
pub fn get_workspace_scores(project_root: &str) -> HashMap<String, f64> {
    let normalized = project_root.trim_end_matches('/');
    let prefix = format!("ws:{normalized}:");
    let all_scores = get_scores();
    all_scores
        .into_iter()
        .filter_map(|(key, score)| {
            key.strip_prefix(&prefix)
                .map(|cmd| (cmd.to_string(), score))
        })
        .collect()
}

/// Record an accepted contextual suggestion.
///
/// Key format: `sug:<context_key>:<command>` — Alfred-style "latching": the
/// suggestion engine learns which suggestions the user actually accepts in
/// which context (project root, focused app, or global).
pub fn record_suggestion(context_key: &str, command: &str) -> Result<(), LychiError> {
    let key = format!("sug:{context_key}:{command}");
    record(&key)
}

/// Get learned suggestion-acceptance scores for a context.
///
/// Returns a map of `command -> score (0.0 to 1.0)` for suggestions the user
/// previously accepted in this context.
pub fn get_suggestion_scores(context_key: &str) -> HashMap<String, f64> {
    let prefix = format!("sug:{context_key}:");
    get_scores()
        .into_iter()
        .filter_map(|(key, score)| {
            key.strip_prefix(&prefix)
                .map(|cmd| (cmd.to_string(), score))
        })
        .collect()
}

/// Record that the user chose a particular no-match FALLBACK action (`"ask"` or
/// `"web"`). Keyed globally (`fallback:<action>`), so Lychi learns which escape
/// hatch this user prefers and orders the two rows accordingly — if you keep
/// picking "Search web" over "Ask AI", web starts appearing first.
pub fn record_fallback_choice(action: &str) -> Result<(), LychiError> {
    record(&format!("fallback:{action}"))
}

/// Learned frecency score for a fallback action (`"ask"`/`"web"`), 0.0 if never
/// chosen. Used to order the "Ask AI" / "Search web" rows by preference.
pub fn get_fallback_score(action: &str) -> f64 {
    get_scores()
        .get(&format!("fallback:{action}"))
        .copied()
        .unwrap_or(0.0)
}

/// Get frecency scores for all tracked items.
/// Returns a map of key -> score (0.0 to 1.0).
/// The deserialized frecency table, cached between writes.
///
/// `get_scores` runs on **every keystroke** (app_launcher and window_switcher
/// both call it), and it used to do a full redb scan plus a postcard decode per
/// row every time. Measured: 0.46ms at 100 entries, 5.3ms at 1000, 24ms at
/// 5000 — so a 20-character query cost ~482ms of pure frecency scanning once a
/// user had accumulated 5k tracked items. Linear in total usage: the launcher
/// got slower the more it was used.
///
/// What is cached is the ENTRIES, not the scores. Scores decay with time and
/// are recomputed per call — arithmetic over a HashMap, which is not what cost
/// anything. `record` bumps the generation, so a write invalidates this without
/// needing to know it exists.
type CachedEntries = Vec<(String, FrecencyEntry)>;
static ENTRY_CACHE: std::sync::Mutex<Option<(u64, CachedEntries)>> = std::sync::Mutex::new(None);

/// Bumped on every write; compared under the cache lock to detect staleness.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Commit a write and invalidate the read cache.
///
/// Every writer goes through this rather than committing and remembering to
/// bump. A cache whose invalidation is a step each caller must not forget is a
/// cache that goes stale the first time someone adds a writer — which happened
/// immediately: `clear` wiped the table while a populated cache kept serving
/// the deleted scores, and a pre-existing test caught it.
fn commit_write(txn: redb::WriteTransaction) -> Result<(), LychiError> {
    txn.commit()?;
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    Ok(())
}

/// Force the read cache to re-read on the next access.
///
/// For writers that DON'T go through `commit_write` — notably backup restore,
/// which replaces the FRECENCY table rows inside its own transaction. Without
/// this bump the in-process cache keeps serving the pre-restore scores, so the
/// launcher ranks by data the user just restored away. Cheap; the next
/// `with_entries` re-reads once.
pub fn invalidate() {
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

/// Run `f` over every entry, reading from cache when the table has not changed.
///
/// Takes a closure rather than returning the Vec so the cached entries are
/// borrowed under the lock instead of cloned — at 5000 entries the clone was
/// most of what remained after caching.
fn with_entries<R>(f: impl FnOnce(&[(String, FrecencyEntry)]) -> R) -> R {
    let generation = GENERATION.load(std::sync::atomic::Ordering::Acquire);
    {
        let cache = ENTRY_CACHE.lock();
        if let Ok(ref cache) = cache
            && let Some((cached_gen, ref entries)) = **cache
            && cached_gen == generation
        {
            return f(entries);
        }
    }

    let mut entries = Vec::new();
    if let Some(db) = store()
        && let Ok(txn) = db.begin_read()
        && let Ok(table) = txn.open_table(FRECENCY)
        && let Ok(iter) = table.iter()
    {
        for item in iter.flatten() {
            let (key, value) = item;
            if let Ok(entry) = crate::db::decode_value::<FrecencyEntry>(value.value()) {
                entries.push((key.value().to_string(), entry));
            }
        }
    }

    let mut cache = ENTRY_CACHE.lock();
    if let Ok(ref mut cache) = cache {
        **cache = Some((generation, entries));
        if let Some((_, ref entries)) = **cache {
            return f(entries);
        }
    }
    // Lock poisoned: answer from the fresh read rather than failing.
    f(&entries_fallback())
}

/// Only reachable if the cache mutex is poisoned, which means another thread
/// panicked while holding it. An empty set degrades ranking, never breaks it.
fn entries_fallback() -> Vec<(String, FrecencyEntry)> {
    Vec::new()
}

/// Run `f` over every row whose key starts with `prefix`, via a redb range
/// scan — O(log n + matches), never a full-table walk.
///
/// This is the required read shape for the namespaced stores (`latch:`, `imp:`,
/// `ws:`). Each of them first shipped as `table.iter()` + `strip_prefix` per
/// row — a full scan and a full decode of EVERY frecency row, on the
/// per-keystroke path. That is the exact "launcher gets slower the more it is
/// used" curve measured and banned at the top of this file (0.46ms at 100
/// rows, 24ms at 5000); the A4 cache fixed `get_scores` and the newer learning
/// features quietly re-imported the pattern. The keys are prefix-ordered, so
/// the B-tree can answer a prefix question directly: seek to `prefix`, stop at
/// the first key past it.
///
/// Generic over the row type because the FRECENCY table multiplexes structs by
/// namespace (`imp:` rows are `ImpressionEntry`, the rest `FrecencyEntry`) —
/// which is also why these reads cannot ride the typed `ENTRY_CACHE`.
fn for_prefix<T: serde::de::DeserializeOwned>(prefix: &str, mut f: impl FnMut(&str, T)) {
    let Some(db) = store() else {
        return;
    };
    let Ok(txn) = db.begin_read() else {
        return;
    };
    let Ok(table) = txn.open_table(FRECENCY) else {
        return;
    };
    let Ok(range) = table.range(prefix..) else {
        return;
    };
    for (key, value) in range.flatten() {
        // Keys are sorted: the first key that no longer carries the prefix
        // ends the namespace, and everything after it is other data.
        let Some(rest) = key.value().strip_prefix(prefix) else {
            break;
        };
        if let Ok(entry) = crate::db::decode_value::<T>(value.value()) {
            f(rest, entry);
        }
    }
}

pub fn get_scores() -> HashMap<String, f64> {
    let now_ms = super::now_millis();
    with_entries(|entries| {
        let mut scores = HashMap::with_capacity(entries.len());
        for (key, entry) in entries {
            let score = entry.score(now_ms);
            if score > 0.0 {
                scores.insert(key.clone(), score * entry.time_affinity(now_ms));
            }
        }
        scores
    })
}

/// Workspace commands with score AND raw use count — `command → (score, count)`.
///
/// The zero-state quality gate needs "how many times", which the score alone
/// cannot answer (a single use minutes ago outscores five uses last week).
/// Rides the same [`with_entries`] cache as [`get_scores`]; no extra scan.
pub fn get_workspace_stats(project_root: &str) -> HashMap<String, (f64, u32)> {
    let now_ms = super::now_millis();
    let normalized = project_root.trim_end_matches('/');
    let prefix = format!("ws:{normalized}:");
    with_entries(|entries| {
        let mut stats = HashMap::new();
        for (key, entry) in entries {
            if let Some(cmd) = key.strip_prefix(&prefix) {
                let score = entry.score(now_ms);
                if score > 0.0 {
                    stats.insert(
                        cmd.to_string(),
                        (score * entry.time_affinity(now_ms), entry.count),
                    );
                }
            }
        }
        stats
    })
}

/// Per-command circadian [`FrecencyEntry::time_affinity`] for one workspace's
/// commands (`ws:<project_root>:<command>` keys). Returns `command → affinity`
/// (1.0 neutral, up to 1.15) so the cold-path ranker can give workspace-memory
/// suggestions the same "knows your routine" tiebreak the recents get. Commands
/// with too little history (or absent) map to 1.0 by the caller's default.
pub fn get_workspace_affinity(project_root: &str) -> HashMap<String, f64> {
    let now_ms = super::now_millis();
    let normalized = project_root.trim_end_matches('/');
    let prefix = format!("ws:{normalized}:");
    let mut affinity = HashMap::new();
    for_prefix::<FrecencyEntry>(&prefix, |cmd, entry| {
        affinity.insert(cmd.to_string(), entry.time_affinity(now_ms));
    });
    affinity
}

// ── Impression tracking (self-tuning CTR demotion) ──────────────────────

/// Impressions vs acceptances for a `(context, command)` suggestion. Feeds the
/// acceptance-rate signal that DEMOTES chronically-ignored suggestions — the
/// denominator the acceptance-only `sug:` store never had.
///
/// Key format: `imp:<context_key>:<command>`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImpressionEntry {
    /// Times shown at panel-settle.
    pub impressions: u32,
    /// Times the shown command was then executed.
    pub accepts: u32,
    /// Last update (ms since epoch) — for read-side decay/recovery.
    pub last_ms: u64,
}

/// Half-life for impression/accept decay. Stale suppression heals over time:
/// as counts decay toward zero, an ignored item drops below the suppression
/// impression floor and gets a probationary re-showing.
const IMPRESSION_HALFLIFE_DAYS: f64 = 14.0;

/// Decay counts toward zero by age (geometric, 14-day half-life). Applied
/// read-side so the pure scorer stays clock-free.
fn decay_counts(entry: &ImpressionEntry, now_ms: u64) -> (u32, u32) {
    let age_days = (now_ms.saturating_sub(entry.last_ms)) as f64 / 86_400_000.0;
    if age_days <= 0.0 {
        return (entry.accepts, entry.impressions);
    }
    let factor = 0.5_f64.powf(age_days / IMPRESSION_HALFLIFE_DAYS);
    (
        (entry.accepts as f64 * factor).round() as u32,
        (entry.impressions as f64 * factor).round() as u32,
    )
}

/// Record one impression each for a batch of shown commands, in a single write
/// transaction. Called once per panel-settle (debounced), never per keystroke.
pub fn record_impressions(context_key: &str, commands: &[String]) -> Result<(), LychiError> {
    if commands.is_empty() {
        return Ok(());
    }
    let Some(db) = store() else {
        return Ok(());
    };
    let now_ms = super::now_millis();
    let txn = begin_write(&db)?;
    {
        let mut table = txn.open_table(FRECENCY)?;
        for command in commands {
            let key = format!("imp:{context_key}:{command}");
            let mut entry: ImpressionEntry = table
                .get(key.as_str())?
                .and_then(|v| crate::db::decode_value(v.value()).ok())
                .unwrap_or_default();
            entry.impressions = entry.impressions.saturating_add(1);
            entry.last_ms = now_ms;
            let bytes = crate::db::encode_row(&entry)?;
            table.insert(key.as_str(), bytes.as_slice())?;
        }
    }
    commit_write(txn)
}

/// Record one acceptance for a `(context, command)`. Called alongside
/// `record_suggestion` when the user runs a shown suggestion.
pub fn record_acceptance(context_key: &str, command: &str) -> Result<(), LychiError> {
    let Some(db) = store() else {
        return Ok(());
    };
    let now_ms = super::now_millis();
    let key = format!("imp:{context_key}:{command}");
    let txn = begin_write(&db)?;
    {
        let mut table = txn.open_table(FRECENCY)?;
        let mut entry: ImpressionEntry = table
            .get(key.as_str())?
            .and_then(|v| crate::db::decode_value(v.value()).ok())
            .unwrap_or_default();
        entry.accepts = entry.accepts.saturating_add(1);
        entry.last_ms = now_ms;
        let bytes = crate::db::encode_row(&entry)?;
        table.insert(key.as_str(), bytes.as_slice())?;
    }
    commit_write(txn)
}

/// Get decayed `(accepts, impressions)` per command for a context.
pub fn get_impression_stats(context_key: &str) -> HashMap<String, (u32, u32)> {
    let now_ms = super::now_millis();
    let prefix = format!("imp:{context_key}:");
    let mut out = HashMap::new();
    for_prefix::<ImpressionEntry>(&prefix, |command, entry| {
        let (accepts, impressions) = decay_counts(&entry, now_ms);
        out.insert(command.to_string(), (accepts, impressions));
    });
    out
}

/// Backstop cap on total learning rows. Not a tuning knob: at the measured
/// decode rates the table stays imperceptible well below this, so the cap
/// exists only to bound the pathological case (scripted use, years of uptime)
/// where even dead-row sweeping can't keep growth linear-in-habits rather
/// than linear-in-lifetime. Eviction is oldest-last-used first.
const MAX_LEARNING_ROWS: usize = 50_000;

/// Delete learning rows that can no longer influence ranking, then enforce
/// [`MAX_LEARNING_ROWS`]. Returns how many rows were removed.
///
/// Runs at startup (next to history's tombstone purge), never on the hot path.
/// Without it the table grows monotonically: expiry was read-side only — an
/// expired latch scored 0.0 forever but its row was scanned and decoded
/// forever — and every panel-settle mints `imp:` rows, every correction
/// `latch:` rows. "Habits you drop must stop shaping results" (the latch
/// doc's own rule) now extends to "and stop costing reads".
///
/// Dead means *provably* inert under the read-side rules:
/// - `latch:` rows past [`LATCH_WINDOW_DAYS`] — the hard horizon after which
///   `latch_strength` answers 0.0 unconditionally.
/// - `imp:` rows whose decayed counts round to (0, 0) — invisible to the
///   CTR demotion that reads them.
///
/// Other namespaces decay asymptotically (an ancient row still weighs 0.1 in
/// `score`), so they are never "dead", only cappable.
pub fn prune_expired() -> Result<usize, LychiError> {
    let Some(db) = store() else {
        return Ok(0);
    };
    prune_expired_impl(&db, MAX_LEARNING_ROWS)
}

fn prune_expired_impl(db: &Arc<Database>, max_rows: usize) -> Result<usize, LychiError> {
    let now_ms = super::now_millis();
    let txn = begin_write(db)?;
    let removed;
    {
        let mut table = txn.open_table(FRECENCY)?;

        // One pass: collect the dead, and the last-used stamp of the living
        // (for the cap). Startup-only, so a full scan is the right tool here.
        let mut dead: Vec<String> = Vec::new();
        let mut living: Vec<(u64, String)> = Vec::new();
        for (key, value) in table.iter()?.flatten() {
            let key_str = key.value();
            if key_str.starts_with("latch:") {
                if let Ok(entry) = crate::db::decode_value::<FrecencyEntry>(value.value()) {
                    if latch_strength(&entry, now_ms) <= 0.0 {
                        dead.push(key_str.to_string());
                    } else {
                        let last = entry.recent_timestamps.last().copied().unwrap_or(0);
                        living.push((last, key_str.to_string()));
                    }
                    continue;
                }
            } else if key_str.starts_with("imp:") {
                if let Ok(entry) = crate::db::decode_value::<ImpressionEntry>(value.value()) {
                    if decay_counts(&entry, now_ms) == (0, 0) {
                        dead.push(key_str.to_string());
                    } else {
                        living.push((entry.last_ms, key_str.to_string()));
                    }
                    continue;
                }
            } else if let Ok(entry) = crate::db::decode_value::<FrecencyEntry>(value.value()) {
                let last = entry.recent_timestamps.last().copied().unwrap_or(0);
                living.push((last, key_str.to_string()));
                continue;
            }
            // Undecodable under every known shape: scanned forever, read by
            // nothing. Dead by definition.
            dead.push(key_str.to_string());
        }

        // Backstop: evict oldest-last-used living rows over the cap.
        if living.len() > max_rows {
            living.sort_unstable();
            let excess = living.len() - max_rows;
            dead.extend(living.drain(..excess).map(|(_, k)| k));
        }

        removed = dead.len();
        for key in &dead {
            table.remove(key.as_str())?;
        }
    }
    if removed > 0 {
        tracing::info!("[frecency] pruned {removed} inert learning row(s)");
    }
    // commit_write even when nothing was removed: the generation bump is
    // harmless, and an early return would add a second commit path to keep
    // correct forever.
    commit_write(txn)?;
    Ok(removed)
}

/// Clear all learned ranking data (frecency for history, workspace, and
/// suggestion-acceptance keys). Resets app/command ordering to neutral.
/// Returns the number of entries removed.
pub fn clear() -> Result<usize, LychiError> {
    let Some(db) = store() else {
        return Ok(0);
    };
    let txn = begin_write(&db)?;
    let removed;
    {
        let mut table = txn.open_table(FRECENCY)?;
        let keys: Vec<String> = table
            .iter()?
            .filter_map(|r| r.ok().map(|(k, _)| k.value().to_string()))
            .collect();
        removed = keys.len();
        for key in &keys {
            table.remove(key.as_str())?;
        }
    }
    commit_write(txn)?;
    Ok(removed)
}

// ── Query→result latching ───────────────────────────────────────────────
//
// Every other store in this file is keyed **item → count**: how often was this
// command run, in this context. That answers "what does this user use a lot",
// and it is the wrong question for ranking a specific query.
//
// The failure it produces: a popular app outranks everything for ANY query that
// merely mentions it. Typing `dnf search firefox` offered Firefox first, because
// Firefox is genuinely the user's most-run app. No amount of per-item learning
// fixes that — the signal needed is "for THIS query, the user chose THAT".
//
// So latching keys **query → command**. Correct `dnf search firefox` once and
// the launcher stops offering Firefox for it, without any rule mentioning the
// word "dnf". That is the [[feedback_dynamic_over_hardcoded]] property: usage-
// driven and name-agnostic by construction.
//
// Modelled on Alfred's knowledge store, including the part that is easy to skip:
// a rolling window. Habits you drop must stop shaping results, so a latch decays
// and expires rather than accumulating for the life of the install.

/// Rolling window for latches, matching Alfred's documented ~4 weeks.
///
/// Not a half-life but a hard horizon: past this, a latch scores 0.0 and is
/// treated as absent. A latch is a statement about current intent, and a
/// month-old intent is not evidence about today.
const LATCH_WINDOW_DAYS: f64 = 28.0;

/// Longest query prefix used as a latch key.
///
/// Latching on the WHOLE query would almost never hit twice — real input varies
/// by a trailing word. Latching on too little would bind unrelated queries
/// together. This is a cap, not a fixed length: shorter queries key on
/// themselves.
const LATCH_PREFIX_LEN: usize = 24;

/// The latch key for a query: normalised, capped prefix.
///
/// Case and surrounding whitespace are noise. Truncation is by CHARACTER, not
/// byte — slicing a multi-byte character mid-sequence panics, and search queries
/// are exactly where non-ASCII input shows up.
fn latch_prefix(query: &str) -> String {
    query
        .trim()
        .to_lowercase()
        .chars()
        .take(LATCH_PREFIX_LEN)
        .collect()
}

/// Record that, for this query, the user chose this command.
///
/// Called on acceptance. Unlike the other learning stores this needs NO context
/// key: the query itself is the context. That also means it still learns when
/// there is no project or focused app — the case where per-context learning
/// records nothing at all.
pub fn record_latch(query: &str, command: &str) -> Result<(), LychiError> {
    let prefix = latch_prefix(query);
    if prefix.is_empty() || command.trim().is_empty() {
        return Ok(());
    }
    // A latch binding a query to ITSELF teaches nothing — the user typed a
    // command and it ran. Latching is for when the chosen row differs from the
    // literal input.
    if prefix == command.trim().to_lowercase() {
        return Ok(());
    }
    record(&format!("latch:{prefix}\u{1}{command}"))
}

/// Commands this user has chosen for this query, as `command -> strength`
/// in 0.0..=1.0.
///
/// Uses `\u{1}` as the field separator rather than `:` because commands
/// routinely contain colons (`https://…`, `run docker: …`), and splitting on a
/// character that appears in the data silently truncates keys.
pub fn get_latches(query: &str) -> HashMap<String, f64> {
    let prefix = latch_prefix(query);
    let mut out = HashMap::new();
    if prefix.is_empty() {
        return out;
    }
    let now_ms = super::now_millis();
    let want = format!("latch:{prefix}\u{1}");
    // This runs on EVERY keystroke (Executor::completions) — it must stay a
    // range scan, never a table walk. See `for_prefix`.
    for_prefix::<FrecencyEntry>(&want, |command, entry| {
        let strength = latch_strength(&entry, now_ms);
        if strength > 0.0 {
            out.insert(command.to_string(), strength);
        }
    });
    out
}

/// Strength of a latch: how recently and how often it was chosen, 0.0 outside
/// the window.
///
/// Deliberately saturating rather than linear in count. Alfred's stated
/// behaviour is that two or three corrections re-learn an intention, so the
/// curve must reach useful strength almost immediately — a scale where 20
/// repetitions are needed would never fire in practice.
fn latch_strength(entry: &FrecencyEntry, now_ms: u64) -> f64 {
    let Some(&last) = entry.recent_timestamps.last() else {
        return 0.0;
    };
    let age_days = (now_ms.saturating_sub(last)) as f64 / 86_400_000.0;
    if age_days >= LATCH_WINDOW_DAYS {
        return 0.0;
    }
    // Linear decay across the window: a fresh latch is full strength, one at
    // the horizon is nothing.
    let recency = 1.0 - (age_days / LATCH_WINDOW_DAYS);
    // 1 pick → 0.5, 2 → 0.67, 3 → 0.75, saturating toward 1.0.
    let repetition = entry.count as f64 / (entry.count as f64 + 1.0);
    recency * repetition
}

#[cfg(test)]
mod prune_tests {
    use super::*;
    use crate::db::open_test_database;

    /// Write a raw row with a chosen shape and timestamps — the only way to
    /// produce "old" data, since `record` stamps the wall clock.
    fn insert_raw<T: Serialize>(db: &Arc<Database>, key: &str, entry: &T) {
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(FRECENCY).unwrap();
            let bytes = crate::db::encode_row(entry).unwrap();
            table.insert(key, bytes.as_slice()).unwrap();
        }
        commit_write(txn).unwrap();
    }

    fn days_ago_ms(days: f64) -> u64 {
        crate::db::now_millis() - (days * 86_400_000.0) as u64
    }

    /// The read-side rules say an expired latch and a fully-decayed impression
    /// are invisible; prune makes the table agree, and leaves the living alone.
    #[test]
    fn prune_deletes_exactly_the_rows_reads_ignore() {
        let db = open_test_database();
        set_store_for_test(db.clone());

        // Dead: latch past the 28-day horizon; imp decayed to (0,0).
        insert_raw(
            &db,
            "latch:old query\u{1}cmd",
            &FrecencyEntry {
                count: 3,
                recent_timestamps: vec![days_ago_ms(LATCH_WINDOW_DAYS + 1.0)],
            },
        );
        insert_raw(
            &db,
            "imp:ctx:old-cmd",
            &ImpressionEntry {
                impressions: 1,
                accepts: 0,
                last_ms: days_ago_ms(120.0),
            },
        );
        // Living: a fresh latch, a fresh impression, an ordinary frecency row.
        record_latch("new query", "chosen cmd").unwrap();
        record_impressions("ctx", &["fresh-cmd".to_string()]).unwrap();
        record("firefox").unwrap();

        let removed = prune_expired().unwrap();
        assert_eq!(removed, 2, "exactly the two inert rows go");

        assert!(!get_latches("old query").contains_key("cmd"));
        assert!(get_latches("new query").contains_key("chosen cmd"));
        assert!(get_impression_stats("ctx").contains_key("fresh-cmd"));
        assert!(get_scores().contains_key("firefox"));

        // Idempotent: nothing left to prune.
        assert_eq!(prune_expired().unwrap(), 0);
    }

    /// The backstop cap evicts oldest-last-used first and only over the limit.
    #[test]
    fn cap_evicts_oldest_rows_first() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        for i in 0..6 {
            insert_raw(
                &db,
                &format!("cmd-{i}"),
                &FrecencyEntry {
                    count: 1,
                    // cmd-0 is the oldest, cmd-5 the newest.
                    recent_timestamps: vec![days_ago_ms((60 - i) as f64)],
                },
            );
        }
        assert_eq!(prune_expired_impl(&db, 4).unwrap(), 2);
        let scores = get_scores();
        assert!(!scores.contains_key("cmd-0") && !scores.contains_key("cmd-1"));
        assert!(scores.contains_key("cmd-2") && scores.contains_key("cmd-5"));
    }
}

#[cfg(test)]
mod latch_tests {
    use super::*;
    use crate::db::open_test_database;

    /// The range scan must stop at the namespace boundary, not just filter —
    /// and neighbouring keys that share all but the last character must not
    /// bleed in. Guards the `for_prefix` break condition that replaced the
    /// full-table walk (A4's curve, re-shipped by the learning features).
    #[test]
    fn latches_for_neighbouring_prefixes_stay_separate() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        record_latch("aaa", "for-aaa").unwrap();
        record_latch("aab", "for-aab").unwrap();
        record("history:aaa something").unwrap(); // other namespace
        record("zzz").unwrap(); // sorts after every latch

        let latches = get_latches("aaa");
        assert_eq!(latches.len(), 1, "got: {latches:?}");
        assert!(latches.contains_key("for-aaa"));
    }

    /// The headline case: correcting a query once stops the popular-but-wrong
    /// item being the answer for it. No rule mentions "dnf".
    #[test]
    fn a_latch_binds_a_query_to_the_chosen_command() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        record_latch("dnf search firefox", "pkg search firefox").unwrap();

        let latches = get_latches("dnf search firefox");
        assert!(latches.contains_key("pkg search firefox"));
        assert!(latches["pkg search firefox"] > 0.0);
        // …and says nothing about the app that used to win.
        assert!(!latches.contains_key("firefox"));
    }

    /// Keyed by QUERY, not by item — the whole point. A different query must
    /// not inherit this binding.
    #[test]
    fn a_latch_does_not_leak_to_an_unrelated_query() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        record_latch("dnf search firefox", "pkg search firefox").unwrap();
        assert!(get_latches("open my notes").is_empty());
    }

    /// Queries differing only AFTER the prefix cap share a latch — the same
    /// intention typed with a different tail.
    ///
    /// Note the fixture: both inputs must be identical through 24 characters.
    /// A first attempt used strings that diverged at character 19, which the
    /// code correctly treated as two distinct queries.
    #[test]
    fn queries_sharing_a_prefix_share_the_latch() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        // The shared head must be at least LATCH_PREFIX_LEN so the two inputs
        // are identical THROUGH the cap, not merely up to it.
        let base = "dnf search firefox extended release "; // > 24 chars
        assert!(base.len() > LATCH_PREFIX_LEN, "fixture must exceed the cap");
        record_latch(&format!("{base}stable"), "pkg search firefox").unwrap();
        let latches = get_latches(&format!("{base}nightly"));
        assert!(
            latches.contains_key("pkg search firefox"),
            "queries identical through the cap must share a latch, got: {latches:?}"
        );
    }

    /// …and queries that diverge WITHIN the cap must not.
    #[test]
    fn queries_diverging_inside_the_cap_do_not_share_a_latch() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        record_latch("dnf search firefox", "pkg search firefox").unwrap();
        assert!(get_latches("dnf search chromium").is_empty());
    }

    #[test]
    fn case_and_whitespace_do_not_split_a_latch() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        record_latch("  DNF Search Firefox ", "pkg search firefox").unwrap();
        assert!(get_latches("dnf search firefox").contains_key("pkg search firefox"));
    }

    /// Repetition strengthens, as Alfred's "2 or 3 times" behaviour requires.
    #[test]
    fn repeated_choices_strengthen_the_latch() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        record_latch("q", "cmd").unwrap();
        let once = get_latches("q")["cmd"];
        record_latch("q", "cmd").unwrap();
        record_latch("q", "cmd").unwrap();
        assert!(get_latches("q")["cmd"] > once);
    }

    /// A latch to the literal input teaches nothing and would otherwise fill the
    /// store with no-ops.
    #[test]
    fn a_query_latched_to_itself_is_not_stored() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        record_latch("firefox", "firefox").unwrap();
        assert!(get_latches("firefox").is_empty());
    }

    #[test]
    fn empty_input_is_ignored() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        record_latch("   ", "cmd").unwrap();
        record_latch("q", "  ").unwrap();
        assert!(get_latches("   ").is_empty());
        assert!(get_latches("q").is_empty());
    }

    /// Commands contain colons routinely (`https://…`). Separating fields on
    /// `:` would truncate the command half of the key.
    #[test]
    fn a_command_containing_colons_round_trips() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        record_latch("docs", "open https://lychi.app/docs").unwrap();
        assert!(get_latches("docs").contains_key("open https://lychi.app/docs"));
    }

    /// Truncating a multi-byte character mid-sequence panics. Search queries
    /// are exactly where non-ASCII arrives, and this codebase has fixed
    /// char-boundary panics before.
    #[test]
    fn a_long_multibyte_query_does_not_panic() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        let q = "\u{e9}".repeat(40);
        record_latch(&q, "cmd").unwrap();
        assert!(get_latches(&q).contains_key("cmd"));
    }

    // ── Decay ───────────────────────────────────────────────────────────
    //
    // Tested on the pure scorer: the store has no clock injection, and
    // back-dating a redb entry to prove decay would be testing the harness.

    #[test]
    fn a_fresh_latch_is_strong() {
        let now = 1_000_000_000_000u64;
        let entry = FrecencyEntry {
            count: 3,
            recent_timestamps: vec![now],
        };
        assert!(latch_strength(&entry, now) > 0.5);
    }

    /// The rolling window is the part most easily omitted, and omitting it is
    /// what makes learned behaviour calcify.
    #[test]
    fn a_latch_past_the_window_is_dead() {
        let now = 1_000_000_000_000u64;
        let long_ago = now - ((LATCH_WINDOW_DAYS as u64 + 1) * 86_400_000);
        let entry = FrecencyEntry {
            count: 99,
            recent_timestamps: vec![long_ago],
        };
        assert_eq!(
            latch_strength(&entry, now),
            0.0,
            "a latch outside the window must not rank, however often it was used"
        );
    }

    #[test]
    fn strength_decays_with_age() {
        let now = 1_000_000_000_000u64;
        let mk = |days: u64| FrecencyEntry {
            count: 3,
            recent_timestamps: vec![now - days * 86_400_000],
        };
        assert!(latch_strength(&mk(1), now) > latch_strength(&mk(20), now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache must not outlive the data. A write bumps the generation, so
    /// the next read rebuilds — without this, a newly-recorded item would stay
    /// invisible to ranking for the rest of the session.
    #[test]
    fn a_write_invalidates_the_cache() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        record("alpha").unwrap();
        let before = get_scores();
        assert!(before.contains_key("alpha"));
        assert!(!before.contains_key("beta"), "beta not recorded yet");

        // Populate the cache, then write.
        let _ = get_scores();
        record("beta").unwrap();

        let after = get_scores();
        assert!(
            after.contains_key("beta"),
            "a write did not invalidate the cache: {after:?}"
        );
        assert!(after.contains_key("alpha"), "existing entries must survive");
    }

    /// The stats accessor must expose true use counts per workspace command —
    /// the zero-state ≥2-uses gate keys on the count, not the score.
    #[test]
    fn workspace_stats_carry_counts() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        record_workspace("/proj", "cargo test").unwrap();
        record_workspace("/proj", "cargo test").unwrap();
        record_workspace("/proj", "kill 1234").unwrap();
        record_workspace("/other", "make").unwrap();

        let stats = get_workspace_stats("/proj/");
        assert_eq!(stats["cargo test"].1, 2);
        assert_eq!(stats["kill 1234"].1, 1);
        assert!(!stats.contains_key("make"), "other workspaces stay out");
        assert!(stats["cargo test"].0 > 0.0);
    }

    /// Repeated access must still raise a score — the cache returns entries,
    /// and scores are recomputed from them, so recording twice has to show.
    #[test]
    fn recording_again_still_raises_the_score() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        record("once").unwrap();
        record("twice").unwrap();
        let _ = get_scores(); // warm the cache
        record("twice").unwrap();

        let scores = get_scores();
        assert!(
            scores["twice"] > scores["once"],
            "twice={} once={}",
            scores["twice"],
            scores["once"]
        );
    }

    /// How `get_scores` scales with the frecency table. It is called per
    /// keystroke from app_launcher and window_switcher, so this is the shape of
    /// the launcher's own latency as usage accumulates.
    ///
    ///     cargo test -p lychi-core --lib frecency -- --ignored --nocapture
    #[test]
    #[ignore]
    fn measure_get_scores_as_the_table_grows() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        for n in [100usize, 500, 1000, 2000, 5000] {
            while table_len(&db) < n {
                let k = format!("key {}", table_len(&db));
                record(&k).unwrap();
            }
            let t = std::time::Instant::now();
            for _ in 0..20 {
                let _ = get_scores();
            }
            println!(
                "  {:>5} entries: get_scores = {:>6}us  (x20 = one 20-keystroke query)",
                n,
                t.elapsed().as_micros() / 20
            );
        }
    }

    fn table_len(db: &Arc<Database>) -> usize {
        use redb::ReadableTableMetadata;
        let txn = db.begin_read().unwrap();
        txn.open_table(crate::db::FRECENCY)
            .map(|t| t.len().unwrap() as usize)
            .unwrap_or(0)
    }
    use crate::db::open_test_database;

    #[test]
    fn impressions_batch_and_accept_roundtrip() {
        let db = open_test_database();
        set_store_for_test(db.clone());
        let cmds = vec!["git commit".to_string(), "docker ps".to_string()];
        // One panel-settle records an impression for each shown command.
        record_impressions("proj:/x", &cmds).unwrap();
        record_impressions("proj:/x", &cmds).unwrap();
        record_acceptance("proj:/x", "git commit").unwrap();

        let stats = get_impression_stats("proj:/x");
        assert_eq!(stats.get("git commit"), Some(&(1, 2))); // 1 accept, 2 impressions
        assert_eq!(stats.get("docker ps"), Some(&(0, 2)));
        // Other contexts are isolated.
        assert!(get_impression_stats("proj:/y").is_empty());
    }

    #[test]
    fn decay_heals_stale_suppression() {
        // An entry that was shown 20× with 0 accepts, last touched ~28 days ago
        // (two half-lives), decays to ~5 impressions → below the suppress floor.
        let now = super::super::now_millis();
        let old = now - (28 * 86_400_000);
        let entry = ImpressionEntry {
            impressions: 20,
            accepts: 0,
            last_ms: old,
        };
        let (accepts, impressions) = decay_counts(&entry, now);
        assert_eq!(accepts, 0);
        assert!(
            impressions <= 6,
            "20 impressions should decay to ~5 over two half-lives, got {impressions}"
        );
    }

    #[test]
    fn time_affinity_boosts_matching_hour() {
        use chrono::Timelike;
        // Build timestamps all at the current hour → strong affinity.
        let now = super::super::now_millis();
        let now_hour = chrono::DateTime::from_timestamp_millis(now as i64)
            .unwrap()
            .hour();
        // Five uses, all at the current hour on prior days.
        let ts: Vec<u64> = (1..=5).map(|d| now - d * 86_400_000).collect();
        let entry = FrecencyEntry {
            count: 5,
            recent_timestamps: ts,
        };
        assert!(entry.time_affinity(now) > 1.1, "same-hour use should boost");
        let _ = now_hour;

        // Timestamps 12h away → no affinity (neutral 1.0).
        let ts_off: Vec<u64> = (1..=5)
            .map(|d| now - d * 86_400_000 - 12 * 3_600_000)
            .collect();
        let entry_off = FrecencyEntry {
            count: 5,
            recent_timestamps: ts_off,
        };
        assert!((entry_off.time_affinity(now) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn workspace_affinity_reflects_usage_hours() {
        use super::FRECENCY;
        let db = open_test_database();
        set_store_for_test(db.clone());
        let now = super::super::now_millis();

        // Seed two workspace commands directly: one used across prior days at
        // THIS hour (strong affinity), one 12h out of phase (neutral).
        let insert = |cmd: &str, ts: Vec<u64>| {
            let entry = FrecencyEntry {
                count: ts.len() as u32,
                recent_timestamps: ts,
            };
            let txn = db.begin_write().unwrap();
            {
                let mut table = txn.open_table(FRECENCY).unwrap();
                let key = format!("ws:/home/u/proj:{cmd}");
                let bytes = crate::db::encode_row(&entry).unwrap();
                table.insert(key.as_str(), bytes.as_slice()).unwrap();
            }
            txn.commit().unwrap();
        };
        insert(
            "cargo test",
            (1..=5).map(|d| now - d * 86_400_000).collect(),
        );
        insert(
            "cargo build",
            (1..=5)
                .map(|d| now - d * 86_400_000 - 12 * 3_600_000)
                .collect(),
        );

        let aff = get_workspace_affinity("/home/u/proj");
        assert!(
            aff.get("cargo test").copied().unwrap() > 1.1,
            "same-hour command carries affinity boost"
        );
        assert!(
            (aff.get("cargo build").copied().unwrap() - 1.0).abs() < 1e-9,
            "out-of-phase command stays neutral"
        );
        // A command from a different workspace must not leak in.
        assert!(!aff.contains_key("npm run dev"));
    }

    #[test]
    fn time_affinity_neutral_with_little_data() {
        let now = super::super::now_millis();
        let entry = FrecencyEntry {
            count: 1,
            recent_timestamps: vec![now],
        };
        assert_eq!(entry.time_affinity(now), 1.0);
    }

    #[test]
    fn fresh_counts_dont_decay() {
        let now = super::super::now_millis();
        let entry = ImpressionEntry {
            impressions: 10,
            accepts: 3,
            last_ms: now,
        };
        assert_eq!(decay_counts(&entry, now), (3, 10));
    }

    #[test]
    fn test_frecency_score_recent() {
        let now = 1_700_000_000_000u64;
        let entry = FrecencyEntry {
            count: 5,
            recent_timestamps: vec![
                now - 1_000,      // 1 second ago
                now - 60_000,     // 1 minute ago
                now - 1_800_000,  // 30 minutes ago
                now - 7_200_000,  // 2 hours ago
                now - 86_400_000, // 1 day ago
            ],
        };
        let score = entry.score(now);
        // 3 within hour (3.0) + 1 within day (0.7) + 1 within week (0.4) = 4.1 / 10 = 0.41
        assert!(score > 0.3 && score < 0.5, "score was {score}");
    }

    #[test]
    fn test_frecency_score_empty() {
        let entry = FrecencyEntry {
            count: 0,
            recent_timestamps: vec![],
        };
        assert_eq!(entry.score(1_700_000_000_000), 0.0);
    }

    #[test]
    fn test_workspace_record_and_get() {
        let db = crate::db::open_test_database();
        set_store_for_test(db.clone());
        let root = "/home/user/projects/lychi";

        record_workspace(root, "cargo build").unwrap();
        record_workspace(root, "cargo build").unwrap();
        record_workspace(root, "cargo test").unwrap();
        record_workspace("/other/project", "npm run dev").unwrap();

        let scores = get_workspace_scores(root);
        assert!(scores.contains_key("cargo build"));
        assert!(scores.contains_key("cargo test"));
        assert!(!scores.contains_key("npm run dev")); // different project
        assert!(scores["cargo build"] > scores["cargo test"]); // more accesses
    }

    #[test]
    fn test_workspace_trailing_slash() {
        let db = crate::db::open_test_database();
        set_store_for_test(db.clone());
        record_workspace("/home/user/project/", "make").unwrap();
        let scores = get_workspace_scores("/home/user/project");
        assert!(scores.contains_key("make"));
    }

    #[test]
    fn test_suggestion_record_and_get() {
        let db = crate::db::open_test_database();
        set_store_for_test(db.clone());

        record_suggestion("proj:/home/u/lychi", "run pnpm dev").unwrap();
        record_suggestion("proj:/home/u/lychi", "run pnpm dev").unwrap();
        record_suggestion("proj:/home/u/lychi", "git commit").unwrap();
        record_suggestion("app:firefox", "web rust").unwrap();

        let scores = get_suggestion_scores("proj:/home/u/lychi");
        assert!(scores.contains_key("run pnpm dev"));
        assert!(scores.contains_key("git commit"));
        assert!(!scores.contains_key("web rust")); // different context
        assert!(scores["run pnpm dev"] > scores["git commit"]);

        // Other context is isolated
        let firefox = get_suggestion_scores("app:firefox");
        assert_eq!(firefox.len(), 1);
        assert!(firefox.contains_key("web rust"));
    }

    #[test]
    fn test_fallback_choice_learns_preference() {
        let db = crate::db::open_test_database();
        set_store_for_test(db.clone());
        // No history → both zero, so the caller's default (Ask AI first) holds.
        assert_eq!(get_fallback_score("ask"), 0.0);
        assert_eq!(get_fallback_score("web"), 0.0);

        // User keeps choosing web → web outscores ask.
        record_fallback_choice("web").unwrap();
        record_fallback_choice("web").unwrap();
        record_fallback_choice("ask").unwrap();
        assert!(
            get_fallback_score("web") > get_fallback_score("ask"),
            "web chosen more → should score higher"
        );
    }

    #[test]
    fn test_record_and_get() {
        let db = crate::db::open_test_database();
        set_store_for_test(db.clone());
        record("firefox").unwrap();
        record("firefox").unwrap();
        record("terminal").unwrap();

        let scores = get_scores();
        assert!(scores.contains_key("firefox"));
        assert!(scores.contains_key("terminal"));
        assert!(scores["firefox"] > scores["terminal"]);
    }
}
