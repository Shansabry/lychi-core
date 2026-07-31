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
    db: &std::sync::Arc<redb::Database>,
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

    let frecency_scores = crate::db::frecency::get_scores(db);
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let home = dirs::home_dir();

    // The SAME ranker the `/` search uses — not a copy of it. Both surfaces
    // answer "which of these did you mean" identically by construction.
    let ranked = rank::rank(query, results.items, |d| {
        frecency_recency_bonus(
            frecency_scores.get(&d.full_path).copied(),
            d.modified_secs,
            now_secs,
        )
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
                icon_path: if r.data.is_dir {
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

    // Recency: touched in the last day → up to 20, decaying to 0 over ~30 days.
    let recency_bonus = match modified_secs {
        Some(m) if now_secs >= m => {
            let age_days = (now_secs - m) as f64 / 86_400.0;
            (20.0 * (1.0 - (age_days / 30.0)).max(0.0)).min(20.0)
        }
        _ => 0.0,
    };

    (freq_bonus + recency_bonus).round() as u16
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
    use super::frecency_recency_bonus;

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
        // ~15 days old → about half.
        let mid = frecency_recency_bonus(None, Some(now - 15 * DAY), now);
        assert!((9..=11).contains(&mid), "got {mid}");
        // Older than 30 days → 0.
        assert_eq!(frecency_recency_bonus(None, Some(now - 40 * DAY), now), 0);
    }

    #[test]
    fn frecency_and_recency_combine() {
        let now = 1_000 * DAY;
        // Frequently used AND fresh → 40 + 20 = 60 (the cap).
        assert_eq!(frecency_recency_bonus(Some(1.0), Some(now), now), 60);
    }

    #[test]
    fn future_mtime_is_ignored_not_panicking() {
        let now = 1_000 * DAY;
        // A modified time in the future (clock skew) → no recency bonus, no panic.
        assert_eq!(frecency_recency_bonus(None, Some(now + DAY), now), 0);
    }
}
