use std::collections::HashMap;
use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};

use crate::error::LychiError;

use super::FRECENCY;

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
pub fn record(db: &Arc<Database>, key: &str) -> Result<(), LychiError> {
    let now_ms = super::now_millis();
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(FRECENCY)?;

        let entry = match table.get(key)? {
            Some(existing) => {
                let mut entry: FrecencyEntry =
                    postcard::from_bytes(existing.value()).unwrap_or(FrecencyEntry::new(now_ms));
                entry.record_access(now_ms);
                entry
            }
            None => FrecencyEntry::new(now_ms),
        };

        let bytes = postcard::to_allocvec(&entry)
            .map_err(|e| LychiError::Database(format!("frecency serialize: {e}")))?;
        table.insert(key, bytes.as_slice())?;
    }
    commit_write(txn)
}

/// Record a workspace-scoped access.
///
/// Key format: `ws:<project_root>:<command>` — scoped frecency that tracks
/// which commands are frequently used in which project directory.
pub fn record_workspace(
    db: &Arc<Database>,
    project_root: &str,
    command: &str,
) -> Result<(), LychiError> {
    let normalized = project_root.trim_end_matches('/');
    let key = format!("ws:{normalized}:{command}");
    record(db, &key)
}

/// Record that a command ran in a specific repo within a multi-repo container.
/// Learns the user's active repo per container (`repo:<container>:<repo_path>`),
/// so the most-used repo becomes the silent default.
pub fn record_repo_choice(
    db: &Arc<Database>,
    container: &str,
    repo_path: &str,
) -> Result<(), LychiError> {
    let container = container.trim_end_matches('/');
    let key = format!("repo:{container}:{repo_path}");
    record(db, &key)
}

/// Frecency scores of repos chosen within a container: `repo_path -> score`.
pub fn get_repo_choice_scores(db: &Arc<Database>, container: &str) -> HashMap<String, f64> {
    let container = container.trim_end_matches('/');
    let prefix = format!("repo:{container}:");
    get_scores(db)
        .into_iter()
        .filter_map(|(key, score)| key.strip_prefix(&prefix).map(|r| (r.to_string(), score)))
        .collect()
}

/// Get workspace-scoped frecency scores for a specific project.
///
/// Returns a map of `command -> score` for commands previously run in this project.
pub fn get_workspace_scores(db: &Arc<Database>, project_root: &str) -> HashMap<String, f64> {
    let normalized = project_root.trim_end_matches('/');
    let prefix = format!("ws:{normalized}:");
    let all_scores = get_scores(db);
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
pub fn record_suggestion(
    db: &Arc<Database>,
    context_key: &str,
    command: &str,
) -> Result<(), LychiError> {
    let key = format!("sug:{context_key}:{command}");
    record(db, &key)
}

/// Get learned suggestion-acceptance scores for a context.
///
/// Returns a map of `command -> score (0.0 to 1.0)` for suggestions the user
/// previously accepted in this context.
pub fn get_suggestion_scores(db: &Arc<Database>, context_key: &str) -> HashMap<String, f64> {
    let prefix = format!("sug:{context_key}:");
    get_scores(db)
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
pub fn record_fallback_choice(db: &Arc<Database>, action: &str) -> Result<(), LychiError> {
    record(db, &format!("fallback:{action}"))
}

/// Learned frecency score for a fallback action (`"ask"`/`"web"`), 0.0 if never
/// chosen. Used to order the "Ask AI" / "Search web" rows by preference.
pub fn get_fallback_score(db: &Arc<Database>, action: &str) -> f64 {
    get_scores(db)
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

/// Run `f` over every entry, reading from cache when the table has not changed.
///
/// Takes a closure rather than returning the Vec so the cached entries are
/// borrowed under the lock instead of cloned — at 5000 entries the clone was
/// most of what remained after caching.
fn with_entries<R>(db: &Arc<Database>, f: impl FnOnce(&[(String, FrecencyEntry)]) -> R) -> R {
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
    if let Ok(txn) = db.begin_read()
        && let Ok(table) = txn.open_table(FRECENCY)
        && let Ok(iter) = table.iter()
    {
        for item in iter.flatten() {
            let (key, value) = item;
            if let Ok(entry) = postcard::from_bytes::<FrecencyEntry>(value.value()) {
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

pub fn get_scores(db: &Arc<Database>) -> HashMap<String, f64> {
    let now_ms = super::now_millis();
    with_entries(db, |entries| {
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

/// Like [`get_scores`] but each score is multiplied by the item's circadian
/// [`FrecencyEntry::time_affinity`] — so commands used at this hour rank
/// slightly higher. Used for the zero-state recents (a "knows your routine"
/// tiebreak, not a driver).
pub fn get_scores_with_affinity(db: &Arc<Database>) -> HashMap<String, f64> {
    let now_ms = super::now_millis();
    let mut scores = HashMap::new();

    let Ok(txn) = db.begin_read() else {
        return scores;
    };
    let Ok(table) = txn.open_table(FRECENCY) else {
        return scores;
    };
    let Ok(iter) = table.iter() else {
        return scores;
    };

    for item in iter.flatten() {
        let (key, value) = item;
        let Ok(entry) = postcard::from_bytes::<FrecencyEntry>(value.value()) else {
            continue;
        };
        let score = entry.score(now_ms);
        if score > 0.0 {
            scores.insert(key.value().to_string(), score * entry.time_affinity(now_ms));
        }
    }

    scores
}

/// Per-command circadian [`FrecencyEntry::time_affinity`] for one workspace's
/// commands (`ws:<project_root>:<command>` keys). Returns `command → affinity`
/// (1.0 neutral, up to 1.15) so the cold-path ranker can give workspace-memory
/// suggestions the same "knows your routine" tiebreak the recents get. Commands
/// with too little history (or absent) map to 1.0 by the caller's default.
pub fn get_workspace_affinity(db: &Arc<Database>, project_root: &str) -> HashMap<String, f64> {
    let now_ms = super::now_millis();
    let normalized = project_root.trim_end_matches('/');
    let prefix = format!("ws:{normalized}:");
    let mut affinity = HashMap::new();

    let Ok(txn) = db.begin_read() else {
        return affinity;
    };
    let Ok(table) = txn.open_table(FRECENCY) else {
        return affinity;
    };
    let Ok(iter) = table.iter() else {
        return affinity;
    };

    for item in iter.flatten() {
        let (key, value) = item;
        let Some(cmd) = key.value().strip_prefix(&prefix) else {
            continue;
        };
        let Ok(entry) = postcard::from_bytes::<FrecencyEntry>(value.value()) else {
            continue;
        };
        affinity.insert(cmd.to_string(), entry.time_affinity(now_ms));
    }

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
pub fn record_impressions(
    db: &Arc<Database>,
    context_key: &str,
    commands: &[String],
) -> Result<(), LychiError> {
    if commands.is_empty() {
        return Ok(());
    }
    let now_ms = super::now_millis();
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(FRECENCY)?;
        for command in commands {
            let key = format!("imp:{context_key}:{command}");
            let mut entry: ImpressionEntry = table
                .get(key.as_str())?
                .and_then(|v| postcard::from_bytes(v.value()).ok())
                .unwrap_or_default();
            entry.impressions = entry.impressions.saturating_add(1);
            entry.last_ms = now_ms;
            let bytes = postcard::to_allocvec(&entry)
                .map_err(|e| LychiError::Database(format!("impression serialize: {e}")))?;
            table.insert(key.as_str(), bytes.as_slice())?;
        }
    }
    commit_write(txn)
}

/// Record one acceptance for a `(context, command)`. Called alongside
/// `record_suggestion` when the user runs a shown suggestion.
pub fn record_acceptance(
    db: &Arc<Database>,
    context_key: &str,
    command: &str,
) -> Result<(), LychiError> {
    let now_ms = super::now_millis();
    let key = format!("imp:{context_key}:{command}");
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(FRECENCY)?;
        let mut entry: ImpressionEntry = table
            .get(key.as_str())?
            .and_then(|v| postcard::from_bytes(v.value()).ok())
            .unwrap_or_default();
        entry.accepts = entry.accepts.saturating_add(1);
        entry.last_ms = now_ms;
        let bytes = postcard::to_allocvec(&entry)
            .map_err(|e| LychiError::Database(format!("acceptance serialize: {e}")))?;
        table.insert(key.as_str(), bytes.as_slice())?;
    }
    commit_write(txn)
}

/// Get decayed `(accepts, impressions)` per command for a context.
pub fn get_impression_stats(db: &Arc<Database>, context_key: &str) -> HashMap<String, (u32, u32)> {
    let now_ms = super::now_millis();
    let prefix = format!("imp:{context_key}:");
    let mut out = HashMap::new();

    let Ok(txn) = db.begin_read() else {
        return out;
    };
    let Ok(table) = txn.open_table(FRECENCY) else {
        return out;
    };
    let Ok(iter) = table.iter() else {
        return out;
    };

    for item in iter.flatten() {
        let (key, value) = item;
        let key = key.value();
        let Some(command) = key.strip_prefix(&prefix) else {
            continue;
        };
        let Ok(entry) = postcard::from_bytes::<ImpressionEntry>(value.value()) else {
            continue;
        };
        let (accepts, impressions) = decay_counts(&entry, now_ms);
        out.insert(command.to_string(), (accepts, impressions));
    }
    out
}

/// Clear all learned ranking data (frecency for history, workspace, and
/// suggestion-acceptance keys). Resets app/command ordering to neutral.
/// Returns the number of entries removed.
pub fn clear(db: &Arc<Database>) -> Result<usize, LychiError> {
    let txn = db.begin_write()?;
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
pub fn record_latch(db: &Arc<Database>, query: &str, command: &str) -> Result<(), LychiError> {
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
    record(db, &format!("latch:{prefix}\u{1}{command}"))
}

/// Commands this user has chosen for this query, as `command -> strength`
/// in 0.0..=1.0.
///
/// Uses `\u{1}` as the field separator rather than `:` because commands
/// routinely contain colons (`https://…`, `run docker: …`), and splitting on a
/// character that appears in the data silently truncates keys.
pub fn get_latches(db: &Arc<Database>, query: &str) -> HashMap<String, f64> {
    let prefix = latch_prefix(query);
    let mut out = HashMap::new();
    if prefix.is_empty() {
        return out;
    }
    let now_ms = super::now_millis();
    let want = format!("latch:{prefix}\u{1}");

    let Ok(txn) = db.begin_read() else {
        return out;
    };
    let Ok(table) = txn.open_table(FRECENCY) else {
        return out;
    };
    let Ok(iter) = table.iter() else {
        return out;
    };

    for (key, value) in iter.flatten() {
        let Some(command) = key.value().strip_prefix(&want) else {
            continue;
        };
        let Ok(entry) = postcard::from_bytes::<FrecencyEntry>(value.value()) else {
            continue;
        };
        let strength = latch_strength(&entry, now_ms);
        if strength > 0.0 {
            out.insert(command.to_string(), strength);
        }
    }
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
mod latch_tests {
    use super::*;
    use crate::db::open_test_database;

    /// The headline case: correcting a query once stops the popular-but-wrong
    /// item being the answer for it. No rule mentions "dnf".
    #[test]
    fn a_latch_binds_a_query_to_the_chosen_command() {
        let db = open_test_database();
        record_latch(&db, "dnf search firefox", "pkg search firefox").unwrap();

        let latches = get_latches(&db, "dnf search firefox");
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
        record_latch(&db, "dnf search firefox", "pkg search firefox").unwrap();
        assert!(get_latches(&db, "open my notes").is_empty());
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
        // The shared head must be at least LATCH_PREFIX_LEN so the two inputs
        // are identical THROUGH the cap, not merely up to it.
        let base = "dnf search firefox extended release "; // > 24 chars
        assert!(base.len() > LATCH_PREFIX_LEN, "fixture must exceed the cap");
        record_latch(&db, &format!("{base}stable"), "pkg search firefox").unwrap();
        let latches = get_latches(&db, &format!("{base}nightly"));
        assert!(
            latches.contains_key("pkg search firefox"),
            "queries identical through the cap must share a latch, got: {latches:?}"
        );
    }

    /// …and queries that diverge WITHIN the cap must not.
    #[test]
    fn queries_diverging_inside_the_cap_do_not_share_a_latch() {
        let db = open_test_database();
        record_latch(&db, "dnf search firefox", "pkg search firefox").unwrap();
        assert!(get_latches(&db, "dnf search chromium").is_empty());
    }

    #[test]
    fn case_and_whitespace_do_not_split_a_latch() {
        let db = open_test_database();
        record_latch(&db, "  DNF Search Firefox ", "pkg search firefox").unwrap();
        assert!(get_latches(&db, "dnf search firefox").contains_key("pkg search firefox"));
    }

    /// Repetition strengthens, as Alfred's "2 or 3 times" behaviour requires.
    #[test]
    fn repeated_choices_strengthen_the_latch() {
        let db = open_test_database();
        record_latch(&db, "q", "cmd").unwrap();
        let once = get_latches(&db, "q")["cmd"];
        record_latch(&db, "q", "cmd").unwrap();
        record_latch(&db, "q", "cmd").unwrap();
        assert!(get_latches(&db, "q")["cmd"] > once);
    }

    /// A latch to the literal input teaches nothing and would otherwise fill the
    /// store with no-ops.
    #[test]
    fn a_query_latched_to_itself_is_not_stored() {
        let db = open_test_database();
        record_latch(&db, "firefox", "firefox").unwrap();
        assert!(get_latches(&db, "firefox").is_empty());
    }

    #[test]
    fn empty_input_is_ignored() {
        let db = open_test_database();
        record_latch(&db, "   ", "cmd").unwrap();
        record_latch(&db, "q", "  ").unwrap();
        assert!(get_latches(&db, "   ").is_empty());
        assert!(get_latches(&db, "q").is_empty());
    }

    /// Commands contain colons routinely (`https://…`). Separating fields on
    /// `:` would truncate the command half of the key.
    #[test]
    fn a_command_containing_colons_round_trips() {
        let db = open_test_database();
        record_latch(&db, "docs", "open https://lychi.app/docs").unwrap();
        assert!(get_latches(&db, "docs").contains_key("open https://lychi.app/docs"));
    }

    /// Truncating a multi-byte character mid-sequence panics. Search queries
    /// are exactly where non-ASCII arrives, and this codebase has fixed
    /// char-boundary panics before.
    #[test]
    fn a_long_multibyte_query_does_not_panic() {
        let db = open_test_database();
        let q = "\u{e9}".repeat(40);
        record_latch(&db, &q, "cmd").unwrap();
        assert!(get_latches(&db, &q).contains_key("cmd"));
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
        record(&db, "alpha").unwrap();
        let before = get_scores(&db);
        assert!(before.contains_key("alpha"));
        assert!(!before.contains_key("beta"), "beta not recorded yet");

        // Populate the cache, then write.
        let _ = get_scores(&db);
        record(&db, "beta").unwrap();

        let after = get_scores(&db);
        assert!(
            after.contains_key("beta"),
            "a write did not invalidate the cache: {after:?}"
        );
        assert!(after.contains_key("alpha"), "existing entries must survive");
    }

    /// Repeated access must still raise a score — the cache returns entries,
    /// and scores are recomputed from them, so recording twice has to show.
    #[test]
    fn recording_again_still_raises_the_score() {
        let db = open_test_database();
        record(&db, "once").unwrap();
        record(&db, "twice").unwrap();
        let _ = get_scores(&db); // warm the cache
        record(&db, "twice").unwrap();

        let scores = get_scores(&db);
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
        for n in [100usize, 500, 1000, 2000, 5000] {
            while table_len(&db) < n {
                let k = format!("key {}", table_len(&db));
                record(&db, &k).unwrap();
            }
            let t = std::time::Instant::now();
            for _ in 0..20 {
                let _ = get_scores(&db);
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
        let cmds = vec!["git commit".to_string(), "docker ps".to_string()];
        // One panel-settle records an impression for each shown command.
        record_impressions(&db, "proj:/x", &cmds).unwrap();
        record_impressions(&db, "proj:/x", &cmds).unwrap();
        record_acceptance(&db, "proj:/x", "git commit").unwrap();

        let stats = get_impression_stats(&db, "proj:/x");
        assert_eq!(stats.get("git commit"), Some(&(1, 2))); // 1 accept, 2 impressions
        assert_eq!(stats.get("docker ps"), Some(&(0, 2)));
        // Other contexts are isolated.
        assert!(get_impression_stats(&db, "proj:/y").is_empty());
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
                let bytes = postcard::to_allocvec(&entry).unwrap();
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

        let aff = get_workspace_affinity(&db, "/home/u/proj");
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
        let root = "/home/user/projects/lychi";

        record_workspace(&db, root, "cargo build").unwrap();
        record_workspace(&db, root, "cargo build").unwrap();
        record_workspace(&db, root, "cargo test").unwrap();
        record_workspace(&db, "/other/project", "npm run dev").unwrap();

        let scores = get_workspace_scores(&db, root);
        assert!(scores.contains_key("cargo build"));
        assert!(scores.contains_key("cargo test"));
        assert!(!scores.contains_key("npm run dev")); // different project
        assert!(scores["cargo build"] > scores["cargo test"]); // more accesses
    }

    #[test]
    fn test_workspace_trailing_slash() {
        let db = crate::db::open_test_database();
        record_workspace(&db, "/home/user/project/", "make").unwrap();
        let scores = get_workspace_scores(&db, "/home/user/project");
        assert!(scores.contains_key("make"));
    }

    #[test]
    fn test_suggestion_record_and_get() {
        let db = crate::db::open_test_database();

        record_suggestion(&db, "proj:/home/u/lychi", "run pnpm dev").unwrap();
        record_suggestion(&db, "proj:/home/u/lychi", "run pnpm dev").unwrap();
        record_suggestion(&db, "proj:/home/u/lychi", "git commit").unwrap();
        record_suggestion(&db, "app:firefox", "web rust").unwrap();

        let scores = get_suggestion_scores(&db, "proj:/home/u/lychi");
        assert!(scores.contains_key("run pnpm dev"));
        assert!(scores.contains_key("git commit"));
        assert!(!scores.contains_key("web rust")); // different context
        assert!(scores["run pnpm dev"] > scores["git commit"]);

        // Other context is isolated
        let firefox = get_suggestion_scores(&db, "app:firefox");
        assert_eq!(firefox.len(), 1);
        assert!(firefox.contains_key("web rust"));
    }

    #[test]
    fn test_fallback_choice_learns_preference() {
        let db = crate::db::open_test_database();
        // No history → both zero, so the caller's default (Ask AI first) holds.
        assert_eq!(get_fallback_score(&db, "ask"), 0.0);
        assert_eq!(get_fallback_score(&db, "web"), 0.0);

        // User keeps choosing web → web outscores ask.
        record_fallback_choice(&db, "web").unwrap();
        record_fallback_choice(&db, "web").unwrap();
        record_fallback_choice(&db, "ask").unwrap();
        assert!(
            get_fallback_score(&db, "web") > get_fallback_score(&db, "ask"),
            "web chosen more → should score higher"
        );
    }

    #[test]
    fn test_record_and_get() {
        let db = crate::db::open_test_database();
        record(&db, "firefox").unwrap();
        record(&db, "firefox").unwrap();
        record(&db, "terminal").unwrap();

        let scores = get_scores(&db);
        assert!(scores.contains_key("firefox"));
        assert!(scores.contains_key("terminal"));
        assert!(scores["firefox"] > scores["terminal"]);
    }
}
