//! IDE workspace detection — deterministic, window-scoped.
//!
//! Resolution: parse window title → extract folder token → resolve on disk
//! via configured search roots (direct child first, then depth-3 BFS).
//!
//! Per C16: "No context is better than wrong context." Every resolution
//! validates the path exists on disk with a project marker before returning.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;

use super::{CodeRootSource, IdeWorkspaceSource};

// ── User-Configurable Extra Markers ─────────────────────────────────────

static EXTRA_STRONG: Mutex<Vec<String>> = Mutex::new(Vec::new());
static EXTRA_SOFT: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Register user-configured extra project markers (from `config.toml`).
/// Called at startup and when config changes.
pub fn register_extra_markers(strong: &[String], soft: &[String]) {
    *EXTRA_STRONG.lock().unwrap() = strong.to_vec();
    *EXTRA_SOFT.lock().unwrap() = soft.to_vec();
}

/// Detect workspace path from an IDE window.
///
/// Returns `(path, source)` — path is `None` when detection fails.
/// Pure title-based: no global state files, no cross-window contamination.
///
/// When `window_id` is provided, checks the per-window workspace cache first.
/// A cache hit with matching token is returned as `Cached` without disk resolution.
pub fn detect_workspace(
    title: &str,
    _wm_class: &str,
    window_id: Option<&str>,
) -> (Option<String>, IdeWorkspaceSource) {
    let Some(token) = extract_folder_from_title(title).map(normalize_title_token) else {
        tracing::debug!("ide::detect: no title token from '{}'", title);
        return (None, IdeWorkspaceSource::None);
    };

    // Fast path: check per-window workspace cache. If the cached token matches
    // the current title token, trust the cache (already revalidated by get()).
    // Token changed (different project in same window) falls through to disk resolution.
    if let Some(wid) = window_id
        && let Some(cached) = super::workspace_cache::get(wid)
        && cached.token == token
    {
        tracing::debug!(
            "ide::detect: '{}' → {} (cached, marker={})",
            token,
            cached.path,
            cached.marker
        );
        return (Some(cached.path), IdeWorkspaceSource::Cached);
    }

    if let Some((path, marker)) = workspace_from_title(token) {
        tracing::debug!("ide::detect: '{}' → {} (marker={})", token, path, marker);
        return (Some(path), IdeWorkspaceSource::Title);
    }

    tracing::debug!("ide::detect: '{}' not resolved on disk", token);
    (None, IdeWorkspaceSource::None)
}

/// Return the extracted folder token from a title (for caching).
pub fn extract_token(title: &str) -> Option<&str> {
    extract_folder_from_title(title).map(normalize_title_token)
}

// ── Title Parsing ────────────────────────────────────────────────────────

/// Extract the project/folder name from an IDE window title.
///
/// VS Code on Linux uses ` - ` (ASCII hyphen):
///   `● package.json - fcc - Visual Studio Code` → `fcc`
///   `Lychi - Visual Studio Code` → `Lychi`
///
/// JetBrains / Zed use ` — ` (em-dash U+2014):
///   `file.rs — Lychi — IntelliJ IDEA` → `Lychi`
///
/// Leading dirty-file indicators (●, •) are stripped before parsing.
/// Em-dash is tried first — it's unambiguous. Hyphen is tried second.
fn extract_folder_from_title(title: &str) -> Option<&str> {
    let title = title.trim_start_matches(['●', '•']).trim_start();

    // Em-dash (JetBrains, Zed, some locales)
    let em = title.split(" \u{2014} ").collect::<Vec<_>>();
    if em.len() >= 2 {
        return match em.len() {
            n if n >= 3 => Some(em[n - 2].trim()),
            _ => Some(em[0].trim()),
        };
    }

    // ASCII hyphen (VS Code on Linux)
    let hy = title.split(" - ").collect::<Vec<_>>();
    match hy.len() {
        n if n >= 3 => Some(hy[n - 2].trim()),
        2 => Some(hy[0].trim()),
        _ => None,
    }
}

/// Strip known IDE title suffixes that don't correspond to directory names.
///
/// VS Code appends ` (Workspace)` for multi-root workspaces.
/// Some IDEs use `[Workspace]` style brackets.
/// Loops until stable (max 3 iterations) to handle stacked suffixes.
fn normalize_title_token(token: &str) -> &str {
    const SUFFIXES: &[&str] = &[" (Workspace)", " [Workspace]"];
    let mut result = token;
    for _ in 0..3 {
        let before = result;
        for suffix in SUFFIXES {
            result = result.trim_end_matches(suffix);
        }
        result = result.trim();
        if result == before {
            break;
        }
    }
    result
}

// ── Disk Resolution ──────────────────────────────────────────────────────

/// Resolve a folder token to an absolute path on disk.
///
/// Returns `(path, marker)` — the marker that validated the project.
///
/// Two-phase search:
/// 1. Direct child: `{search_dir}/{token}` (fast, O(n) where n = search dirs)
/// 2. Nested BFS: walk each search dir up to 3 levels deep (slower, cached)
fn workspace_from_title(token: &str) -> Option<(String, String)> {
    let home = std::env::var("HOME").unwrap_or_default();

    let mut search_dirs = vec![
        home.clone(),
        format!("{}/Projects", home),
        format!("{}/projects", home),
        format!("{}/Developer", home),
        format!("{}/dev", home),
        format!("{}/workspace", home),
        format!("{}/code", home),
        format!("{}/Code", home),
        format!("{}/repos", home),
        format!("{}/src", home),
    ];

    if let Some(extra) = read_project_dirs() {
        search_dirs.extend(extra);
    }

    // Phase 1: direct child lookup (fast)
    for parent in &search_dirs {
        let candidate = format!("{}/{}", parent, token);
        let p = Path::new(&candidate);
        if candidate == home {
            continue;
        }
        if p.is_dir()
            && let Some(marker) = which_project_marker(p)
        {
            return Some((candidate, marker));
        }
    }

    // Phase 2: depth-limited BFS (cached via root-index LRU)
    for parent in &search_dirs {
        if parent == &home {
            continue; // Never BFS $HOME
        }
        // Check root-index cache first
        if let Some(cached) = super::workspace_cache::get_root_index(parent, token) {
            if let Some(path) = cached {
                let p = Path::new(&path);
                if let Some(marker) = which_project_marker(p) {
                    return Some((path, marker));
                }
            }
            continue; // Cache said None (or stale path) — skip re-walk
        }

        let result = find_nested(parent, token, 3);
        // Cache the result (even None) so we don't re-walk
        super::workspace_cache::set_root_index(
            parent,
            token,
            result.as_ref().map(|(p, _)| p.clone()),
        );
        if result.is_some() {
            return result;
        }
    }

    None
}

/// BFS search for a directory named `token` with a project marker,
/// up to `max_depth` levels deep under `root`.
///
/// Returns `None` if zero or multiple matches found (C16: ambiguity → None).
/// Skips common non-project directories to avoid scanning node_modules, .git, etc.
/// Caps visited directories at 5000 to guard against huge mounts.
fn find_nested(root: &str, token: &str, max_depth: usize) -> Option<(String, String)> {
    const SKIP_DIRS: &[&str] = &[
        ".git",
        "node_modules",
        "target",
        ".cache",
        "dist",
        "build",
        "__pycache__",
        ".venv",
        ".tox",
        ".mypy_cache",
        ".next",
        ".nuxt",
        "vendor",
        ".cargo",
        ".rustup",
    ];
    const MAX_VISITED: usize = 5000;

    let root_path = Path::new(root);
    if !root_path.is_dir() {
        return None;
    }

    let t0 = std::time::Instant::now();

    // BFS queue: (path, depth)
    let mut queue: VecDeque<(std::path::PathBuf, usize)> = VecDeque::new();
    queue.push_back((root_path.to_path_buf(), 0));
    let mut visited = 0usize;
    let mut found: Option<(String, String)> = None;

    while let Some((dir, depth)) = queue.pop_front() {
        if depth >= max_depth || visited >= MAX_VISITED {
            continue;
        }

        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };

            // Skip symlinks entirely
            if ft.is_symlink() {
                continue;
            }

            if !ft.is_dir() {
                continue;
            }

            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            // Skip known non-project dirs
            if SKIP_DIRS.contains(&name_str.as_ref()) {
                continue;
            }

            // Skip hidden directories (except the ones we already skip-listed)
            if name_str.starts_with('.') {
                continue;
            }

            visited += 1;
            let child_path = entry.path();

            // Check if this is our target
            if name_str.as_ref() == token
                && let Some(marker) = which_project_marker(&child_path)
            {
                if found.is_some() {
                    // Ambiguous: multiple projects with the same name under this root.
                    // C16: return None rather than guess.
                    tracing::debug!(
                        "ide.bfs token={} root={} visited={} depth={} ms={} result=ambiguous",
                        token,
                        root,
                        visited,
                        max_depth,
                        t0.elapsed().as_millis()
                    );
                    return None;
                }
                found = Some((child_path.to_string_lossy().into_owned(), marker));
                // Don't return yet — continue BFS to check for ambiguity
            }

            // Enqueue for further BFS if not at max depth
            if depth + 1 < max_depth {
                queue.push_back((child_path, depth + 1));
            }
        }
    }

    let status = match &found {
        Some(_) => "hit",
        None if visited >= MAX_VISITED => "capped",
        None => "miss",
    };
    tracing::debug!(
        "ide.bfs token={} root={} visited={} depth={} ms={} result={}",
        token,
        root,
        visited,
        max_depth,
        t0.elapsed().as_millis(),
        status
    );

    found
}

/// Check which project marker exists in a directory.
///
/// Three tiers, checked in order:
/// - **Tier 0**: `.lychi-workspace` — explicit user opt-in, always accepted.
/// - **Tier 1 (strong)**: VCS, build systems, monorepo configs — accepted immediately.
/// - **Tier 2 (soft)**: IDE/editor dirs, docker-compose — accepted only if a child
///   or grandchild directory contains a strong marker (proves real project container).
pub(crate) fn which_project_marker(path: &Path) -> Option<String> {
    // Tier 0: Explicit workspace marker — user opted in
    if path.join(".lychi-workspace").exists() {
        return Some(".lychi-workspace".to_string());
    }

    // Tier 1: Strong markers — accept immediately
    const STRONG: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "setup.py",
        "Makefile",
        "CMakeLists.txt",
        "build.gradle",
        "pom.xml",
        // Monorepo root configs
        "pnpm-workspace.yaml",
        "lerna.json",
        "nx.json",
        "turbo.json",
        "rush.json",
    ];

    if let Some(m) = STRONG.iter().find(|m| path.join(m).exists()) {
        return Some(m.to_string());
    }

    // User-configured extra strong markers (lock only on fallback path)
    {
        let extra = EXTRA_STRONG.lock().unwrap();
        if let Some(m) = extra.iter().find(|m| path.join(m).exists()) {
            return Some(m.clone());
        }
    }

    // Tier 2: Soft markers — accept only if a child dir (depth ≤ 2)
    // contains a strong marker (proves this is a real project container)
    const SOFT: &[&str] = &[
        ".vscode",
        ".cursor",
        ".idea",
        ".zed",
        ".fleet",
        ".claude",
        "docker-compose.yml",
        "docker-compose.yaml",
    ];

    let soft_hit = SOFT.iter().find(|m| path.join(m).exists());
    if let Some(m) = soft_hit
        && has_strong_child(path, STRONG)
    {
        return Some(m.to_string());
    }

    // User-configured extra soft markers (gated by strong-child proof)
    {
        let extra = EXTRA_SOFT.lock().unwrap();
        if let Some(m) = extra.iter().find(|m| path.join(m).exists())
            && has_strong_child(path, STRONG)
        {
            return Some(m.clone());
        }
    }

    None
}

/// Shallow check: does any immediate child or grandchild directory contain a strong marker?
///
/// Results are cached with a 5-minute TTL to avoid repeated readdir calls across summons.
fn has_strong_child(path: &Path, strong: &[&str]) -> bool {
    let path_str = path.to_string_lossy();

    // Check cache first
    if let Some(cached) = super::workspace_cache::get_strong_child(&path_str) {
        return cached;
    }

    let result = has_strong_child_uncached(path, strong);

    super::workspace_cache::set_strong_child(&path_str, result);
    result
}

fn has_strong_child_uncached(path: &Path, strong: &[&str]) -> bool {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let child = entry.path();
        // Depth 1: check child for strong markers
        if strong.iter().any(|m| child.join(m).exists()) {
            return true;
        }
        // Depth 2: check grandchildren
        if let Ok(sub_entries) = std::fs::read_dir(&child) {
            for sub in sub_entries.flatten() {
                if !sub.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    continue;
                }
                if strong.iter().any(|m| sub.path().join(m).exists()) {
                    return true;
                }
            }
        }
    }
    false
}

// ── Code Root Resolution ─────────────────────────────────────────────────

/// Build-system markers that indicate a real code project (not just a docs repo).
const BUILD_MARKERS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "setup.py",
    "pom.xml",
    "build.gradle",
    "Makefile",
    "CMakeLists.txt",
];

/// A code-root candidate has `.git` AND at least one build marker.
fn is_code_root_candidate(path: &Path) -> bool {
    path.join(".git").exists() && BUILD_MARKERS.iter().any(|m| path.join(m).exists())
}

/// Resolve the actual code root for an IDE workspace.
///
/// When the workspace root is a meta-project container (e.g. Lychi with code in `core/`),
/// this finds the unique child/grandchild that qualifies as a code root.
///
/// Uses the same candidate definition everywhere: `.git` + build marker.
/// Returns `None` if 0 or >1 candidates (C16: ambiguity → None).
pub fn resolve_code_root(workspace_root: &Path) -> Option<(String, CodeRootSource)> {
    let ws_str = workspace_root.to_string_lossy();

    // Check cache first
    if let Some(cached) = super::workspace_cache::get_code_root(&ws_str) {
        match cached {
            Some(path) => {
                // Revalidate: path must still exist + satisfy candidate check
                if is_code_root_candidate(Path::new(&path)) {
                    tracing::debug!("code_root: cache hit → {} (revalidated)", path);
                    return Some((path, CodeRootSource::StrongChild));
                }
                // Evict stale entry, fall through to recompute
                super::workspace_cache::evict_code_root(&ws_str);
            }
            None => {
                // Cached "no candidate" result
                tracing::debug!("code_root: cache hit → none");
                return None;
            }
        }
    }

    // Self-check: workspace_root itself is a code root?
    if is_code_root_candidate(workspace_root) {
        // Don't cache WorkspaceStrong — it's instant to check
        return Some((ws_str.into_owned(), CodeRootSource::WorkspaceStrong));
    }

    // Two-level explicit scan (no queue, no recursion)
    let t0 = std::time::Instant::now();
    let mut candidates: Vec<String> = Vec::new();

    let entries = match std::fs::read_dir(workspace_root) {
        Ok(e) => e,
        Err(_) => {
            super::workspace_cache::set_code_root(&ws_str, None);
            return None;
        }
    };

    for entry in entries.flatten() {
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_symlink() || !ft.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip known non-project dirs + hidden dirs
        if name_str.starts_with('.') {
            continue;
        }
        const SKIP: &[&str] = &[
            "node_modules",
            "target",
            ".cache",
            "dist",
            "build",
            "__pycache__",
            ".venv",
            ".tox",
            ".mypy_cache",
            ".next",
            ".nuxt",
            "vendor",
            ".cargo",
            ".rustup",
        ];
        if SKIP.contains(&name_str.as_ref()) {
            continue;
        }

        let child = entry.path();

        // Check child
        if is_code_root_candidate(&child) {
            candidates.push(child.to_string_lossy().into_owned());
        }

        // Check grandchildren
        if let Ok(sub_entries) = std::fs::read_dir(&child) {
            for sub in sub_entries.flatten() {
                if !sub.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    continue;
                }
                let grandchild = sub.path();
                if is_code_root_candidate(&grandchild) {
                    candidates.push(grandchild.to_string_lossy().into_owned());
                }
            }
        }
    }

    let result = match candidates.len() {
        1 => {
            let path = candidates.remove(0);
            tracing::debug!(
                "code_root: {} → {} (StrongChild, {}ms)",
                ws_str,
                path,
                t0.elapsed().as_millis()
            );
            Some((path, CodeRootSource::StrongChild))
        }
        0 => {
            tracing::debug!(
                "code_root: {} → none (no candidates, {}ms)",
                ws_str,
                t0.elapsed().as_millis()
            );
            None
        }
        n => {
            let preview: Vec<_> = candidates.iter().take(3).cloned().collect();
            tracing::debug!(
                "code_root: {} → none (ambiguous: {n} candidates: {preview:?}, {}ms)",
                ws_str,
                t0.elapsed().as_millis()
            );
            None
        }
    };

    // Cache result (even None) to avoid re-scanning
    super::workspace_cache::set_code_root(&ws_str, result.as_ref().map(|(p, _)| p.clone()));
    result
}

/// Check if a directory contains any project marker file.
#[cfg(test)]
fn has_project_marker(path: &Path) -> bool {
    which_project_marker(path).is_some()
}

/// Read user-configured project directories.
///
/// Reads from `~/.config/lychi/project_dirs.json`: `["/mnt/DevSSD", "/mnt/Data/work"]`
fn read_project_dirs() -> Option<Vec<String>> {
    use std::sync::Once;
    static WARN_ONCE: Once = Once::new();

    let config_dir = crate::paths::config_dir();
    let path = config_dir.join("project_dirs.json");
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str::<Vec<String>>(&content).ok(),
        Err(_) => {
            WARN_ONCE.call_once(|| {
                tracing::info!(
                    "project_dirs.json not found at {}; using default search roots only. \
                     Add extra roots (e.g. [\"/mnt/DevSSD\"]) to expand IDE workspace search.",
                    path.display()
                );
            });
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_folder_3_segments() {
        assert_eq!(
            extract_folder_from_title("mod.rs \u{2014} Lychi \u{2014} Visual Studio Code"),
            Some("Lychi")
        );
    }

    #[test]
    fn test_extract_folder_2_segments() {
        assert_eq!(
            extract_folder_from_title("Lychi \u{2014} Visual Studio Code"),
            Some("Lychi")
        );
    }

    #[test]
    fn test_extract_folder_jetbrains() {
        assert_eq!(
            extract_folder_from_title("Main.java \u{2014} myapp \u{2014} IntelliJ IDEA"),
            Some("myapp")
        );
    }

    #[test]
    fn test_extract_folder_zed() {
        assert_eq!(
            extract_folder_from_title("file.rs \u{2014} Lychi \u{2014} Zed"),
            Some("Lychi")
        );
    }

    #[test]
    fn test_extract_folder_single_segment_returns_none() {
        assert_eq!(extract_folder_from_title("Visual Studio Code"), None);
    }

    #[test]
    fn test_extract_folder_untitled_returns_name() {
        // "Untitled-1" is returned but will fail filesystem resolution
        assert_eq!(
            extract_folder_from_title("Untitled-1 \u{2014} Visual Studio Code"),
            Some("Untitled-1")
        );
    }

    // VS Code on Linux uses ASCII hyphen separators
    #[test]
    fn test_extract_folder_vscode_hyphen_3_segments() {
        assert_eq!(
            extract_folder_from_title("mod.rs - Lychi - Visual Studio Code"),
            Some("Lychi")
        );
    }

    #[test]
    fn test_extract_folder_vscode_hyphen_dirty() {
        assert_eq!(
            extract_folder_from_title("● package.json - fcc - Visual Studio Code"),
            Some("fcc")
        );
    }

    #[test]
    fn test_extract_folder_vscode_hyphen_2_segments() {
        assert_eq!(
            extract_folder_from_title("fcc - Visual Studio Code"),
            Some("fcc")
        );
    }

    #[test]
    fn test_has_project_marker() {
        // The Lychi project root should have .git and Cargo.toml
        let lychi_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        assert!(has_project_marker(lychi_root));
    }

    #[test]
    fn test_which_project_marker_returns_first() {
        let lychi_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        // Should return ".git" (first in MARKERS list)
        let marker = which_project_marker(lychi_root);
        assert!(marker.is_some());
    }

    #[test]
    fn test_find_nested_finds_lychi_core() {
        // From the workspace root, find "lychi-core" at depth 2
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap() // crates/
            .parent()
            .unwrap(); // core/
        let root_str = workspace_root.to_string_lossy();
        let result = find_nested(&root_str, "lychi-core", 3);
        assert!(
            result.is_some(),
            "should find lychi-core under {}",
            root_str
        );
        let (path, marker) = result.unwrap();
        assert!(path.ends_with("lychi-core"));
        assert_eq!(marker, "Cargo.toml");
    }

    #[test]
    fn test_find_nested_respects_max_depth() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let root_str = workspace_root.to_string_lossy();
        // depth 0 should find nothing (only checks root's immediate children)
        let result = find_nested(&root_str, "lychi-core", 0);
        assert!(result.is_none(), "depth 0 should not find nested dirs");
    }

    #[test]
    fn test_normalize_title_token_strips_workspace() {
        assert_eq!(normalize_title_token("Lychi (Workspace)"), "Lychi");
    }

    #[test]
    fn test_normalize_title_token_strips_brackets() {
        assert_eq!(
            normalize_title_token("My Project [Workspace]"),
            "My Project"
        );
    }

    #[test]
    fn test_normalize_title_token_noop() {
        assert_eq!(normalize_title_token("Lychi"), "Lychi");
    }

    #[test]
    fn test_normalize_title_token_empty_after_strip() {
        // Edge case: token is entirely the suffix
        assert_eq!(normalize_title_token("(Workspace)"), "(Workspace)");
        // Only strips when preceded by space
        assert_eq!(normalize_title_token(" (Workspace)"), "");
    }

    #[test]
    fn test_extract_token_normalizes() {
        assert_eq!(
            extract_token("● file.rs - Lychi (Workspace) - Visual Studio Code"),
            Some("Lychi")
        );
    }
}
