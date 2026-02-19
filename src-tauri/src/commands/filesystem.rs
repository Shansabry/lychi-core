use lychi_core::action_registry::CompletionItem;
use lychi_core::error::LychiError;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use ignore::WalkBuilder;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};
use tauri::{AppHandle, Emitter, State};

use crate::state::AppState;

/// Resolve a partial path to an absolute path.
/// - `/...` → absolute path as-is
/// - `~/...` → expand ~ to home
/// - anything else → treat as relative to home directory (e.g. `Do` → `~/Do`)
fn resolve_path(raw: &str) -> PathBuf {
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
fn build_label(original_partial: &str, entry_name: &str, is_dir: bool) -> String {
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
#[tauri::command]
pub async fn list_path_completions(partial: String) -> Result<Vec<CompletionItem>, LychiError> {
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
            })
        })
        .collect();

    // Sort by score descending (dirs first due to 1000+ boost, then by match quality)
    entries.sort_by(|a, b| b.score.cmp(&a.score));

    entries.truncate(15);
    Ok(entries)
}

/// List subdirectories of the given path (directories only, absolute paths).
/// Used by the in-app folder picker to avoid the native GTK dialog which
/// crashes on Wayland layer-shell surfaces.
#[tauri::command]
pub async fn list_directories(path: String) -> Result<Vec<DirEntry>, LychiError> {
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

    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(entries)
}

#[derive(serde::Serialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
}

// --- Recursive file search ---

#[derive(Clone, serde::Serialize)]
pub struct MountPoint {
    pub path: String,
    pub label: String,
}

#[derive(Clone, serde::Serialize)]
pub struct FileSearchResult {
    pub label: String,
    pub full_path: String,
    pub is_dir: bool,
    pub score: u16,
    pub description: Option<String>,
}

#[derive(Clone, serde::Serialize)]
pub struct FileSearchBatch {
    pub search_id: u64,
    pub results: Vec<FileSearchResult>,
    pub done: bool,
}

/// Detect real mounted filesystems. Home directory is always first.
#[tauri::command]
pub async fn get_mount_points() -> Result<Vec<MountPoint>, LychiError> {
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
fn search_display_label(path: &Path, is_dir: bool) -> String {
    let home = dirs::home_dir();
    let display = if let Some(ref home) = home {
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

/// Start a recursive fuzzy file search. Results are streamed via events.
#[tauri::command]
pub async fn start_file_search(
    query: String,
    scope: String,
    search_id: u64,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), LychiError> {
    state.active_file_search.store(search_id, Ordering::SeqCst);

    let active_id = state.active_file_search.clone();

    tauri::async_runtime::spawn_blocking(move || {
        walk_and_emit(&query, &scope, search_id, &active_id, &app);
    });

    Ok(())
}

/// Cancel any in-flight file search.
#[tauri::command]
pub async fn cancel_file_search(state: State<'_, AppState>) -> Result<(), LychiError> {
    state.active_file_search.store(0, Ordering::SeqCst);
    Ok(())
}

fn walk_and_emit(
    query: &str,
    scope: &str,
    search_id: u64,
    active_id: &Arc<std::sync::atomic::AtomicU64>,
    app: &AppHandle,
) {
    const BATCH_SIZE: usize = 10;
    const MAX_RESULTS: usize = 50;

    let is_listing = query.is_empty();

    let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
    let pattern = Atom::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
        false,
    );

    let mut builder = WalkBuilder::new(scope);
    builder
        .hidden(true)
        .ignore(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .follow_links(false);
    if is_listing {
        builder.max_depth(Some(1));
    }
    let walker = builder.build();

    let mut batch: Vec<FileSearchResult> = Vec::with_capacity(BATCH_SIZE);
    let mut total_sent: usize = 0;

    for entry in walker {
        if active_id.load(Ordering::Relaxed) != search_id {
            return;
        }

        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let file_name = match entry.file_name().to_str() {
            Some(n) => n,
            None => continue,
        };

        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

        // Skip the root directory itself and hidden files in listing mode
        if is_listing {
            if entry.depth() == 0 {
                continue;
            }
            if file_name.starts_with('.') {
                continue;
            }
        }

        let score = if is_listing {
            // Directory listing — no fuzzy scoring, dirs first
            if is_dir { 100 } else { 50 }
        } else {
            let mut buf = Vec::new();
            let haystack = Utf32Str::new(file_name, &mut buf);
            match pattern.score(haystack, &mut matcher) {
                Some(s) => s,
                None => continue,
            }
        };

        let path = entry.path();

        let description = if is_dir {
            Some("Folder".into())
        } else {
            file_name
                .rsplit('.')
                .next()
                .filter(|ext| !ext.is_empty() && ext.len() < 6 && *ext != file_name)
                .map(|ext| ext.to_uppercase())
        };

        batch.push(FileSearchResult {
            label: search_display_label(path, is_dir),
            full_path: path.to_string_lossy().to_string(),
            is_dir,
            score,
            description,
        });

        if batch.len() >= BATCH_SIZE {
            batch.sort_by(|a, b| b.score.cmp(&a.score));
            let _ = app.emit(
                "lychi://file-search-results",
                FileSearchBatch {
                    search_id,
                    results: std::mem::take(&mut batch),
                    done: false,
                },
            );
            total_sent += BATCH_SIZE;
            if total_sent >= MAX_RESULTS {
                break;
            }
        }
    }

    batch.sort_by(|a, b| b.score.cmp(&a.score));
    let _ = app.emit(
        "lychi://file-search-results",
        FileSearchBatch {
            search_id,
            results: batch,
            done: true,
        },
    );
}
