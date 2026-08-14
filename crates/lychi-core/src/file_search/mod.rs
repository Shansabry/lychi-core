//! Pure file-search engine: path completions, directory listing, mount-point
//! detection, result ranking helpers, and the persistent fuzzy index. No Tauri —
//! the src-tauri bridge wraps these in `spawn_blocking` + `#[tauri::command]` and
//! owns the emit/streaming glue.

/// The persistent per-scope fuzzy file index (nucleo engine + fs watcher).
pub mod corpus;
pub mod live;
pub mod rank;
pub mod session;

pub use corpus::{PathData, indexed_path_count};

use crate::action_registry::CompletionItem;
use crate::error::LychiError;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

/// Resolve a partial path to an absolute path.
/// - `/...` → absolute path as-is
/// - `~/...` → expand ~ to home
/// - anything else → treat as relative to home directory (e.g. `Do` → `~/Do`)
pub fn resolve_path(raw: &str) -> PathBuf {
    if raw.starts_with('/') {
        PathBuf::from(raw)
    } else if let Some(rest) = raw.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(raw))
    } else if raw == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw))
    } else {
        // Bare name like "Do" → treat as ~/Do
        dirs::home_dir()
            .map(|h| h.join(raw))
            .unwrap_or_else(|| PathBuf::from(raw))
    }
}

/// Build a display label using `~/` prefix for home-relative paths.
pub fn build_label(original_partial: &str, entry_name: &str, is_dir: bool) -> String {
    let trailing = if is_dir { "/" } else { "" };

    // Determine the directory prefix from the partial path
    let prefix = if original_partial.starts_with('/') {
        // Absolute path — keep as-is
        match original_partial.rfind('/') {
            Some(idx) => original_partial[..=idx].to_string(),
            None => "/".to_string(),
        }
    } else if original_partial.starts_with("~/") {
        // Already has ~/ prefix — use directory portion as-is
        match original_partial.rfind('/') {
            Some(idx) => original_partial[..=idx].to_string(),
            None => "~/".to_string(),
        }
    } else {
        // Bare name or ~ — prefix with ~/
        match original_partial.rfind('/') {
            Some(idx) => format!("~/{}", &original_partial[..=idx]),
            None => "~/".to_string(),
        }
    };

    format!("{prefix}{entry_name}{trailing}")
}

/// Given the text after `@`, return filesystem completions.
///
/// - Empty or `~` → list home directory
/// - Bare name (e.g. `Do`) → filter home directory contents
/// - `/...` → absolute path
/// - Directories sort before files, max 10 results
pub fn list_path_completions_sync(partial: String) -> Result<Vec<CompletionItem>, LychiError> {
    let raw = partial.trim();

    let (dir_to_list, stem_filter): (PathBuf, String) =
        if raw.is_empty() || raw == "~" || raw == "~/" {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
            (home, String::new())
        } else {
            let resolved = resolve_path(raw);
            if raw.ends_with('/') {
                (resolved, String::new())
            } else {
                let parent = resolved
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("/"));
                let stem = resolved
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                (parent, stem)
            }
        };

    if !dir_to_list.exists() || !dir_to_list.is_dir() {
        return Ok(Vec::new());
    }

    // Set up fuzzy matcher when there's a filter query
    let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
    let pattern = if !stem_filter.is_empty() {
        Some(Atom::new(
            &stem_filter,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
            false,
        ))
    } else {
        None
    };

    let read_dir = std::fs::read_dir(&dir_to_list)?;

    let mut entries: Vec<CompletionItem> = read_dir
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            // Skip hidden files unless the user is explicitly typing a dot
            if name.starts_with('.') && !stem_filter.starts_with('.') {
                return None;
            }
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

            // Fuzzy match when there's a filter, otherwise show all
            let match_score = if let Some(ref p) = pattern {
                let mut buf = Vec::new();
                let haystack = Utf32Str::new(&name, &mut buf);
                match p.score(haystack, &mut matcher) {
                    Some(s) => s,
                    None => return None,
                }
            } else {
                0
            };

            let label = build_label(raw, &name, is_dir);
            let description = if is_dir {
                Some("Folder".into())
            } else {
                name.rsplit('.')
                    .next()
                    .filter(|ext| !ext.is_empty() && ext.len() < 6 && *ext != name)
                    .map(|ext| ext.to_uppercase())
            };
            // Dirs get a large boost so they sort first, plus fuzzy score
            let score = if is_dir {
                1000 + match_score
            } else {
                match_score
            };
            Some(CompletionItem {
                label,
                icon_path: if is_dir {
                    Some("__folder__".into())
                } else {
                    None
                },
                score,
                description,
                reason: None,
                thumb_b64: None,
                ..Default::default()
            })
        })
        .collect();

    // Sort by score descending (dirs first due to 1000+ boost, then by match quality)
    entries.sort_by_key(|b| std::cmp::Reverse(b.score));

    entries.truncate(15);
    Ok(entries)
}

/// Fuzzy jump-to-file for the `@` reference flow — the Claude-Code-style pointer.
///
/// Unlike [`list_path_completions_sync`] (which lists ONE directory's immediate
/// children), this queries the persistent recursive fuzzy index for `scope` and
/// returns the best matches ANYWHERE under it, ranked by the same tier +
/// frecency/recency blend the `/` search uses. Returns a FLAT list (folders
/// boosted to the top, no section-header rows) with the same `CompletionItem`
/// shape the `@` browser already emits — so the frontend's drill/insert logic
/// (trailing `/` = folder, no slash = file) keeps working unchanged.
///
/// The index builds lazily on first touch and fills in the background; an early
/// call simply returns whatever has been indexed so far (same as `/` search).
pub fn fuzzy_path_completions(
    live: &std::sync::Arc<live::LiveSearch>,
    scope: &str,
    query: &str,
    _db: &std::sync::Arc<redb::Database>,
    limit: usize,
) -> Vec<CompletionItem> {
    // One-shot: start a session, take what has matched, done. No redraw callback
    // — there is nowhere to deliver a later result to — so this reads whatever is
    // ready. Correct rather than complete: every item genuinely matches `query`,
    // because a session can only ever answer its own query.
    let generation = live.begin(scope, query, std::sync::Arc::new(|| {}));
    let Some(results) = live.results(generation, RANK_POOL) else {
        return Vec::new();
    };

    let frecency_scores = crate::db::frecency::get_scores();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let home = dirs::home_dir();

    // The SAME ranker the `/` search uses — not a copy of it. Both surfaces
    // answer "which of these did you mean" identically by construction.
    let ranked = rank::rank(query, results.items, |d| {
        ranking_bonus(d, frecency_scores.get(&d.full_path()).copied(), now_secs)
    });

    // Flat list (no folder/file sections): folders just sort first via the tier
    // ordering, and the `@` browser renders one list.
    let mut next_score: u16 = (ranked.len() + 2) as u16;
    ranked
        .into_iter()
        .take(limit)
        .map(|r| {
            next_score = next_score.saturating_sub(1);
            CompletionItem {
                label: rank::display_label(&r.data, home.as_deref()),
                icon_path: if r.data.is_dir() {
                    Some("__folder__".into())
                } else {
                    None
                },
                score: next_score,
                description: r.description,
                ..Default::default()
            }
        })
        .collect()
}

/// Over-fetch from nucleo before re-ranking: it fuzzy-filtered already, so this
/// pool is "paths that match at all", and our ranker reorders the whole pool
/// before anything is truncated. Shared so both surfaces over-fetch alike.
pub const RANK_POOL: usize = 400;

pub fn list_directories_sync(path: String) -> Result<Vec<DirEntry>, LychiError> {
    let dir = if path.is_empty() {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    } else {
        resolve_path(&path)
    };

    if !dir.exists() || !dir.is_dir() {
        return Ok(Vec::new());
    }

    let read_dir = std::fs::read_dir(&dir)?;

    let mut entries: Vec<DirEntry> = read_dir
        .flatten()
        .filter_map(|entry| {
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if !is_dir {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            if name.starts_with('.') {
                return None;
            }
            let path = entry.path().to_string_lossy().to_string();
            Some(DirEntry { name, path })
        })
        .collect();

    entries.sort_by_key(|a| a.name.to_lowercase());
    Ok(entries)
}

#[derive(serde::Serialize, specta::Type)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
}

// --- Recursive file search ---

#[derive(Clone, serde::Serialize, specta::Type)]
pub struct MountPoint {
    pub path: String,
    pub label: String,
}

#[derive(Clone, serde::Serialize, specta::Type)]
pub struct FileSearchResult {
    pub label: String,
    pub full_path: String,
    pub is_dir: bool,
    pub score: u16,
    pub description: Option<String>,
    pub size_bytes: Option<u64>,
    pub modified_secs: Option<u64>,
}

#[derive(Clone, serde::Serialize, specta::Type)]
pub struct FileSearchBatch {
    pub search_id: u64,
    pub results: Vec<FileSearchResult>,
    pub done: bool,
    pub has_ignore_rules: bool,
}

pub fn get_mount_points_sync() -> Result<Vec<MountPoint>, LychiError> {
    let home = dirs::home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|| "/home".into());

    let mut mounts = vec![MountPoint {
        path: home.clone(),
        label: "Home".into(),
    }];

    if let Ok(content) = std::fs::read_to_string("/proc/mounts") {
        let real_fs: &[&str] = &[
            "ext4", "ext3", "btrfs", "xfs", "ntfs", "ntfs3", "vfat", "exfat", "f2fs", "zfs",
        ];
        let skip_paths: &[&str] = &["/boot", "/efi", "/boot/efi"];

        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                continue;
            }
            let mountpoint = parts[1];
            let fstype = parts[2];

            if !real_fs.contains(&fstype) {
                continue;
            }
            if skip_paths.iter().any(|s| mountpoint.starts_with(s)) {
                continue;
            }
            if mountpoint == "/" || mountpoint == home {
                continue;
            }
            if home.starts_with(mountpoint) {
                continue;
            }

            let label = Path::new(mountpoint)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(mountpoint)
                .to_string();

            mounts.push(MountPoint {
                path: mountpoint.to_string(),
                label,
            });
        }
    }

    Ok(mounts)
}

/// Build a display label for a search result path.
pub fn search_display_label(path: &Path, is_dir: bool, home: Option<&Path>) -> String {
    let display = if let Some(home) = home {
        if let Ok(rel) = path.strip_prefix(home) {
            format!("~/{}", rel.display())
        } else {
            path.display().to_string()
        }
    } else {
        path.display().to_string()
    };
    if is_dir {
        format!("{display}/")
    } else {
        display
    }
}

/// A non-selectable section-header row (rendered by CompletionsList as a
/// centered label between two lines via the `__separator__` sentinel).
pub fn section_header(label: &str, next_score: &mut u16) -> FileSearchResult {
    let score = *next_score;
    *next_score = next_score.saturating_sub(1);
    FileSearchResult {
        label: label.to_string(),
        full_path: String::new(),
        is_dir: false,
        score,
        description: Some("__separator__".to_string()),
        size_bytes: None,
        modified_secs: None,
    }
}

/// Stamp a result with the next descending display score.
pub fn finalize_row(mut result: FileSearchResult, next_score: &mut u16) -> FileSearchResult {
    result.score = *next_score;
    *next_score = next_score.saturating_sub(1);
    result
}

/// Modified-time (unix secs) of a walk entry, if available — the recency input.
pub fn build_result_modified(entry: &ignore::DirEntry) -> Option<u64> {
    entry
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// A ranking bonus (0..=60) blending usage frecency and recency of modification.
/// Kept small relative to the fuzzy match score (which can be several hundred)
/// so it's a tie-breaker/nudge, never overriding a clearly better name match —
/// matches how Raycast/Alfred layer usage on top of match quality.
pub fn frecency_recency_bonus(
    frecency: Option<f64>,
    modified_secs: Option<u64>,
    now_secs: u64,
) -> u16 {
    // Frecency: get_scores returns 0.0..=1.0 → up to 40 points.
    let freq_bonus = frecency.unwrap_or(0.0).clamp(0.0, 1.0) * 40.0;
    (freq_bonus + recency_bonus(modified_secs, now_secs)).round() as u16
}

/// The ranking bonus for one indexed path: usage frecency + modification
/// recency, with the `statx` for mtime done here.
///
/// This is the whole "how good is this candidate, ignoring the name match"
/// question in one place. Both ranked surfaces (`/` search and the `@`
/// reference) call it through `rank::rank`'s `bonus_for` closure, so they cannot
/// drift — the previous shape had each surface stat the path and call
/// [`frecency_recency_bonus`] itself, which is two copies of one rule.
///
/// The stat lives here rather than at index time deliberately: `rank` applies
/// this only to candidates that survived `classify`, so it runs for the ranked
/// pool (a few hundred) instead of every path in the scope. See [`PathData`].
pub fn ranking_bonus(data: &PathData, frecency: Option<f64>, now_secs: u64) -> u16 {
    let (_, modified_secs) = corpus::stat_now(&data.full_path());
    frecency_recency_bonus(frecency, modified_secs, now_secs)
}

/// Half-life for the recency term, in days.
///
/// Chosen by measuring a real indexed corpus rather than picked round. On the
/// dev machine's home directory (134k indexed files, `.gitignore` honored):
/// median mtime **422 days**, 10th percentile 191 days, and only **0.11%** of
/// files newer than a week. Against that distribution:
///
/// | half-life | today | 1wk | 1mo | 1qtr | the 97% ancient bulk |
/// |-----------|-------|-----|-----|------|----------------------|
/// | 7d        | 18    | 10  | 1   | 0    | 0                    |
/// | **30d**   | 20    | 17  | 10  | 2.5  | ~0                   |
/// | 180d      | 20    | 19  | 18  | 14   | 4-10                 |
///
/// 7 days is too sharp: it zeroes everything past a fortnight, so the term does
/// nothing for 99.9% of the corpus. 180 days is too flat: it hands the ancient
/// bulk points they have not earned. 30 keeps resolution across the first month
/// — where the files someone is actually working on live — and lets the rest
/// collapse.
///
/// Cross-checks (from a literature review; not independently verified here):
/// Mozilla's Places frecency is reported to use this same shape, and its legacy
/// bucketed form decayed at 0.975/day, commented as a ~28-day half-life. Dumais
/// et al., "Stuff I've Seen" (SIGIR 2003) fitted real re-access against
/// days-since-modified and got a *power* law, which a 5-7 day exponential fits
/// better early but which zeroes the tail — and ~46% of their re-accesses fell
/// outside one month. Anything in 21-45 days is defensible; 7 vs 30 is the
/// difference that shows.
///
/// Adding this to the match score is safe only because the tier system keeps
/// filename matches ~100 points above path-only ones while this whole bonus
/// caps at 60: recency reorders *within* a tier and can never beat a better
/// name match. Do not narrow that gap.
const RECENCY_HALF_LIFE_DAYS: f64 = 30.0;

/// Maximum points the recency term can contribute.
const RECENCY_MAX: f64 = 20.0;

/// Recency component of the ranking bonus: `0..=RECENCY_MAX`, decaying
/// **exponentially** with age.
///
/// Exponential rather than the previous linear ramp for two reasons.
///
/// The linear version was `20 * (1 - age/30)`, clamped at zero — so everything
/// older than 30 days scored exactly 0 and became indistinguishable, while a
/// 29-day-old file still carried 0.7 points. On the measured corpus that cliff
/// hard-zeroed **97.5% of all files**, so the term was doing almost nothing
/// except separating a tiny sliver — and it spent its resolution on the 29-vs-31
/// day boundary, which nobody can perceive.
///
/// Exponential decay puts the resolution where the distinctions actually are
/// (today vs this week vs this month) and decays smoothly instead of falling
/// off a cliff, so ordering degrades gracefully rather than collapsing to a tie.
/// It is also the shape Mozilla's frecency — the origin of the term — is
/// reported to use.
///
/// Unknown mtime scores 0 rather than guessing. Under lazy stat (see
/// [`corpus::stat_now`]) an unreadable or deleted path yields `None`, and a
/// missing file must not be *promoted* by the absence of evidence.
///
/// A future mtime (clock skew, a bad archive) is treated as "now" rather than
/// discarded: it is still the freshest thing we know about.
fn recency_bonus(modified_secs: Option<u64>, now_secs: u64) -> f64 {
    let Some(m) = modified_secs else { return 0.0 };
    let age_days = (now_secs.saturating_sub(m)) as f64 / 86_400.0;
    RECENCY_MAX * 0.5_f64.powf(age_days / RECENCY_HALF_LIFE_DAYS)
}

/// Pre-walk home directory to warm the OS filesystem cache.
/// Called once at startup in a background thread so the first user search hits warm cache.
pub fn warmup_fs_cache() {
    let Some(home) = dirs::home_dir() else { return };
    let walker = WalkBuilder::new(&home)
        .hidden(true)
        .ignore(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .follow_links(false)
        .build();
    let mut count = 0u64;
    for _ in walker {
        count += 1;
        if count >= 100_000 {
            break;
        }
    }
    tracing::debug!(
        "FS cache warmup: visited {count} entries under {}",
        home.display()
    );
}

#[cfg(test)]
mod tests {
    use super::{RECENCY_HALF_LIFE_DAYS, frecency_recency_bonus, recency_bonus};

    const DAY: u64 = 86_400;

    #[test]
    fn nucleo_scores_the_full_word_query() {
        // Reproduce the live bug: does `lighthouse` match filenames containing
        // "lighthouse"? If nucleo returns None here, the matcher/config is the
        // bug; if it scores fine, the 0-results are purely the cancelled walk.
        use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
        use nucleo_matcher::{Config, Matcher, Utf32Str};

        let mut matcher = Matcher::new(Config::DEFAULT);
        let names = [
            "lighthouse_solo.png",
            "lighthouse.png",
            "lighthouse",
            "LIGHTHOUSE.gd",
            "turborepo-light.svg",
        ];
        for q in ["light", "lighthouse"] {
            let pat = Atom::new(
                q,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
                false,
            );
            for name in names {
                let mut buf = Vec::new();
                let hay = Utf32Str::new(name, &mut buf);
                let score = pat.score(hay, &mut matcher);
                // Every listed name contains "light"; the first four contain
                // "lighthouse". The matcher must score them (Some), not None.
                if name.contains(q) {
                    assert!(
                        score.is_some(),
                        "query {q:?} should match {name:?} (substring) but got None"
                    );
                }
            }
        }
    }

    #[test]
    fn no_signals_is_zero() {
        assert_eq!(frecency_recency_bonus(None, None, 1_000 * DAY), 0);
    }

    #[test]
    fn full_frecency_caps_at_forty() {
        // Frecency 1.0, no recency → 40.
        assert_eq!(frecency_recency_bonus(Some(1.0), None, 1_000 * DAY), 40);
        // Out-of-range frecency is clamped.
        assert_eq!(frecency_recency_bonus(Some(5.0), None, 1_000 * DAY), 40);
    }

    #[test]
    fn recent_modification_adds_up_to_twenty() {
        let now = 1_000 * DAY;
        // Touched right now → full 20.
        assert_eq!(frecency_recency_bonus(None, Some(now), now), 20);
    }

    /// The defining property of the curve: every half-life halves the bonus.
    /// A linear ramp would fail this at the second and third points.
    #[test]
    fn recency_halves_every_half_life() {
        let now = 1_000 * DAY;
        let hl = RECENCY_HALF_LIFE_DAYS as u64;
        assert_eq!(frecency_recency_bonus(None, Some(now - hl * DAY), now), 10);
        assert_eq!(
            frecency_recency_bonus(None, Some(now - 2 * hl * DAY), now),
            5
        );
        assert_eq!(
            frecency_recency_bonus(None, Some(now - 3 * hl * DAY), now),
            3 // 2.5, rounded
        );
    }

    /// The bug in the old linear ramp: it hard-zeroed at 30 days, so 97.5% of a
    /// real corpus scored exactly 0 and could not be ordered at all. The decay
    /// must stay strictly monotone past that cliff.
    ///
    /// Asserted on the raw `f64` rather than the rounded `u16`: past a few
    /// months the bonus is deliberately a fraction of a point (it should not be
    /// competing with a name match), so rounding ties are correct behaviour
    /// while an *inversion* would not be.
    #[test]
    fn old_files_still_rank_against_each_other() {
        let now: u64 = 10_000 * DAY;
        let f31 = recency_bonus(Some(now - 31 * DAY), now);
        let f180 = recency_bonus(Some(now - 180 * DAY), now);
        let f365 = recency_bonus(Some(now - 365 * DAY), now);
        let f6y = recency_bonus(Some(now - 2_190 * DAY), now);
        assert!(f31 > f180, "31d ({f31}) must beat 180d ({f180})");
        assert!(f180 > f365, "180d ({f180}) must beat 365d ({f365})");
        assert!(f365 > f6y, "365d ({f365}) must beat 6y ({f6y})");
        // The old formula returned exactly 0.0 for every one of these.
        assert!(f31 > 0.0 && f6y >= 0.0);
    }

    /// Pin the half-life to the range the corpus measurement supports.
    ///
    /// The shape tests above are parameterised over the constant, so they pass
    /// at any value — deliberately, since they check the curve. This checks the
    /// *decision*. On a real indexed corpus (median mtime 422 days, 0.11% of
    /// files under a week old) a 7-day half-life zeroes the term for 99.9% of
    /// files, and a 180-day one hands the ancient 97% points they have not
    /// earned. Anything in 21-45 days behaves the same; outside it does not.
    #[test]
    fn half_life_stays_in_the_defensible_range() {
        assert!(
            (21.0..=45.0).contains(&RECENCY_HALF_LIFE_DAYS),
            "half-life {RECENCY_HALF_LIFE_DAYS}d is outside the measured-defensible \
             range; if this is deliberate, re-measure the corpus and update the \
             table in the constant's doc comment"
        );

        // The properties that range is chosen to give, at week/month scale.
        let now: u64 = 10_000 * DAY;
        let month = frecency_recency_bonus(None, Some(now - 30 * DAY), now);
        let quarter = frecency_recency_bonus(None, Some(now - 90 * DAY), now);
        assert!(
            (8..=12).contains(&month),
            "a month-old file should sit near half the cap, got {month}"
        );
        assert!(
            quarter <= 4,
            "a quarter-old file should be nearly spent, got {quarter}"
        );
    }

    /// The bonus must never exceed its stated cap, whatever the inputs — the
    /// tier system's safety argument depends on this ceiling holding.
    #[test]
    fn bonus_never_exceeds_its_cap() {
        let now: u64 = 10_000 * DAY;
        for mtime in [None, Some(now), Some(now + 999 * DAY), Some(0)] {
            for frec in [None, Some(0.0), Some(1.0), Some(99.0)] {
                let b = frecency_recency_bonus(frec, mtime, now);
                assert!(b <= 60, "bonus {b} exceeded the 60-point cap");
            }
        }
    }

    /// Resolution belongs where the distinctions are: among recent files.
    ///
    /// Checked on the `f64`, because the public bonus is a `u16` and at a 30-day
    /// half-life today and yesterday both round to 20 — a real limit of the
    /// integer scale, not of the curve. Whole *weeks* do separate after
    /// rounding, which is the granularity that matters for ordering.
    #[test]
    fn recent_days_are_distinguishable() {
        let now = 1_000 * DAY;
        let today = recency_bonus(Some(now), now);
        let yesterday = recency_bonus(Some(now - DAY), now);
        let last_week = recency_bonus(Some(now - 7 * DAY), now);
        assert!(
            today > yesterday && yesterday > last_week,
            "{today} / {yesterday} / {last_week}"
        );

        // And the rounded scale must still separate week-scale differences.
        let w0 = frecency_recency_bonus(None, Some(now), now);
        let w2 = frecency_recency_bonus(None, Some(now - 14 * DAY), now);
        let w6 = frecency_recency_bonus(None, Some(now - 42 * DAY), now);
        assert!(w0 > w2 && w2 > w6, "{w0} / {w2} / {w6}");
    }

    #[test]
    fn frecency_and_recency_combine() {
        let now = 1_000 * DAY;
        // Frequently used AND fresh → 40 + 20 = 60 (the cap).
        assert_eq!(frecency_recency_bonus(Some(1.0), Some(now), now), 60);
    }

    /// Unknown mtime must score zero, not a default. Under lazy stat a deleted
    /// or unreadable path yields `None`, and absence of evidence must never
    /// PROMOTE a file above one we know is old.
    #[test]
    fn unknown_mtime_scores_zero() {
        // 10_000 days so a decade-old mtime is still a positive timestamp.
        let now: u64 = 10_000 * DAY;
        assert_eq!(frecency_recency_bonus(None, None, now), 0);
        // Specifically: it must not beat a genuinely ancient file's score.
        let ancient = frecency_recency_bonus(None, Some(now - 3_650 * DAY), now);
        assert!(frecency_recency_bonus(None, None, now) <= ancient);
    }

    /// A future mtime (clock skew, a bad tarball) is clamped to "now" rather
    /// than discarded — it is still the freshest thing we know — and must not
    /// panic on the subtraction.
    #[test]
    fn future_mtime_is_clamped_to_now_not_panicking() {
        let now = 1_000 * DAY;
        assert_eq!(frecency_recency_bonus(None, Some(now + DAY), now), 20);
        assert_eq!(
            frecency_recency_bonus(None, Some(u64::MAX), now),
            20,
            "must saturate, not overflow"
        );
    }
}
