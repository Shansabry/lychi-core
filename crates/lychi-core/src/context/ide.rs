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
    pid: u32,
    title: &str,
    wm_class: &str,
    window_id: Option<&str>,
) -> (Option<String>, IdeWorkspaceSource) {
    // ── Design invariant ────────────────────────────────────────────────
    // The window TITLE is the source of truth for *which* project is
    // focused: it is per-window and always reflects the current project
    // (switching projects in a window changes the title immediately). The
    // `/proc` and config-state signals only *resolve a path* — they must
    // never *override which project* the title names, because they lag:
    // config `storage.json` is written lazily/globally and can point at a
    // different or stale project. So:
    //   1. Title token → disk search — PRIMARY, token-consistent by design.
    //   2. proc / config — FALLBACK only when the title gives no usable
    //      token, or its disk search fails. Their result is accepted only if
    //      it stays consistent with the title token (never contradicts it).
    let token = extract_folder_from_title(title).map(normalize_title_token);

    // Fast path: per-window cache, valid only when the token still matches
    // AND the cached path is still consistent with that token — so a path
    // resolved from a lagging source can never stay pinned to a token it
    // doesn't belong to.
    if let (Some(tok), Some(wid)) = (token, window_id)
        && let Some(cached) = super::workspace_cache::get(wid)
        && cached.token == tok
        && path_matches_token(&cached.path, tok)
    {
        tracing::debug!("ide::detect: '{tok}' → {} (cached)", cached.path);
        return (Some(cached.path), IdeWorkspaceSource::Cached);
    }

    // 1. PRIMARY — resolve the title's token on disk. The search finds a
    //    folder named like the token, so the result always matches the
    //    focused project (incl. the exact subfolder, not a parent).
    if let Some(tok) = token
        && let Some((path, marker)) = workspace_from_title(tok)
    {
        tracing::debug!("ide::detect: '{tok}' → {path} (title, marker={marker})");
        return (Some(path), IdeWorkspaceSource::Title);
    }

    // 2. FALLBACK — only reached when the title gave no token or didn't
    //    resolve on disk. Ground-truth `/proc` first (accurate when the
    //    editor was launched from the project), then config state. If a
    //    token exists, the candidate must be consistent with it.
    if let Some(path) = super::ide_proc::detect(pid)
        && token.is_none_or(|t| path_matches_token(&path, t))
    {
        tracing::debug!("ide::detect: pid={pid} → {path} (proc)");
        return (Some(path), IdeWorkspaceSource::Proc);
    }
    if let Some(path) = super::ide_config::detect(wm_class)
        && token.is_none_or(|t| path_matches_token(&path, t))
    {
        tracing::debug!("ide::detect: {wm_class} → {path} (config)");
        return (Some(path), IdeWorkspaceSource::Config);
    }

    tracing::debug!("ide::detect: unresolved (title='{title}')");
    (None, IdeWorkspaceSource::None)
}

/// Whether a resolved path belongs to the project the title token names —
/// i.e. its final path segment equals the token (case-insensitively). Guards
/// against a lagging proc/config source pinning a stale/parent path (`amt`)
/// onto a different focused project (`amt-course-registration`).
pub(crate) fn path_matches_token(path: &str, token: &str) -> bool {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(token))
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

/// Whether `path` is itself a single project/repo (`.git` + build marker) —
/// vs a container that merely holds repos. The run-target resolver uses this to
/// tell "single-repo workspace" from "multi-repo container".
pub fn is_project_dir(path: &Path) -> bool {
    is_code_root_candidate(path)
}

/// Signals for picking the ACTIVE sub-repo when a workspace root is a container
/// of several repos (e.g. `amt/` holding three sibling repos). Mirrors how
/// VS Code scopes SCM to the repo containing the active file: we proxy "active
/// file" with the window title's subfolder token and the focused terminal's
/// cwd. Both optional; `None`/empty means "no disambiguation signal".
#[derive(Debug, Default, Clone)]
pub struct ActiveHint<'a> {
    /// Subfolder token from the IDE window title (names the focused sub-repo).
    pub title_token: Option<&'a str>,
    /// Focused terminal's cwd — only pass when coherent with the workspace
    /// (same repo/project), so a terminal in a DIFFERENT project can't hijack.
    pub terminal_cwd: Option<&'a str>,
}

/// Enumerate code-root candidates directly under a container (children and
/// grandchildren with `.git` + a build marker). Skips VCS/build noise dirs.
/// This is the set of repos a multi-repo workspace like `amt/` holds.
pub fn enumerate_child_repos(container: &Path) -> Vec<String> {
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
    let mut candidates = Vec::new();
    let Ok(entries) = std::fs::read_dir(container) else {
        return candidates;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() || !ft.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || SKIP.contains(&name_str.as_ref()) {
            continue;
        }
        let child = entry.path();
        if is_code_root_candidate(&child) {
            candidates.push(child.to_string_lossy().into_owned());
        }
        if let Ok(sub_entries) = std::fs::read_dir(&child) {
            for sub in sub_entries.flatten() {
                if !sub.file_type().map(|f| f.is_dir()).unwrap_or(false) {
                    continue;
                }
                let grandchild = sub.path();
                if is_code_root_candidate(&grandchild) {
                    candidates.push(grandchild.to_string_lossy().into_owned());
                }
            }
        }
    }
    candidates
}

/// Resolve the actual code root for an IDE workspace.
///
/// When the workspace root is a meta-project container (e.g. `amt/` with three
/// repos, or Lychi with code in `core/`), this finds the code root. A UNIQUE
/// child/grandchild is used directly; when SEVERAL qualify, `hint` disambiguates
/// to the one the user is actually in (title token, then coherent terminal cwd).
/// Returns `None` only on genuine ambiguity (0 candidates, or >1 with no hint).
pub fn resolve_code_root(
    workspace_root: &Path,
    hint: &ActiveHint,
) -> Option<(String, CodeRootSource)> {
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
    let mut candidates: Vec<String> = enumerate_child_repos(workspace_root);
    if candidates.is_empty() && !workspace_root.is_dir() {
        super::workspace_cache::set_code_root(&ws_str, None);
        return None;
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
            // Multiple sibling repos — pick the ACTIVE one using the hint,
            // like VS Code scoping to the active file's repo. Signal order:
            // 1) title token names a candidate; 2) coherent terminal cwd walks
            // up to a candidate. Only give up when neither disambiguates.
            match disambiguate(&candidates, hint) {
                Some(path) => {
                    tracing::debug!(
                        "code_root: {} → {} (StrongChild, disambiguated from {n}, {}ms)",
                        ws_str,
                        path,
                        t0.elapsed().as_millis()
                    );
                    Some((path, CodeRootSource::StrongChild))
                }
                None => {
                    let preview: Vec<_> = candidates.iter().take(3).cloned().collect();
                    tracing::debug!(
                        "code_root: {} → none (ambiguous: {n} candidates: {preview:?}, no hint, {}ms)",
                        ws_str,
                        t0.elapsed().as_millis()
                    );
                    None
                }
            }
        }
    };

    // Cache result (even None) to avoid re-scanning. Note: a hint-disambiguated
    // result IS cached under the workspace root — acceptable because the title
    // token drives the per-window `detect_workspace` cache upstream, and this
    // code-root cache is revalidated on read.
    super::workspace_cache::set_code_root(&ws_str, result.as_ref().map(|(p, _)| p.clone()));
    result
}

/// Pick the active repo from several sibling candidates using the active-file
/// proxy signals. Returns the chosen candidate path, or `None` if neither
/// signal points at exactly one candidate (true ambiguity → caller yields None).
fn disambiguate(candidates: &[String], hint: &ActiveHint) -> Option<String> {
    // 1. Title token: the candidate whose final path segment equals the token.
    if let Some(token) = hint.title_token.filter(|t| !t.is_empty()) {
        let mut matches = candidates
            .iter()
            .filter(|c| path_matches_token(c, token))
            .cloned();
        if let Some(first) = matches.next()
            && matches.next().is_none()
        {
            return Some(first); // exactly one candidate matches the token
        }
    }

    // 2. Coherent terminal cwd: walk it up to its repo root; if that root is
    //    one of the candidates, that's where the user is working.
    if let Some(cwd) = hint.terminal_cwd.filter(|c| !c.is_empty())
        && let Some(repo_root) = super::git::find_git_root(cwd)
        && let Some(hit) = candidates.iter().find(|c| {
            // Same repo root, or same trailing dir name.
            **c == repo_root || path_matches_token(c, repo_root.rsplit('/').next().unwrap_or(""))
        })
    {
        return Some(hit.clone());
    }

    None
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
    fn multi_repo_disambiguation_by_title_token() {
        // Container `amt/` with three sibling repos → the title token picks the
        // focused one instead of the old "ambiguous → None".
        let base = std::env::temp_dir().join(format!("lychi-multirepo-{}", std::process::id()));
        for repo in ["amt-course-registration", "other-repo", "third"] {
            std::fs::create_dir_all(base.join(repo).join(".git")).unwrap();
            std::fs::write(base.join(repo).join("package.json"), "{}").unwrap();
        }
        let cands: Vec<String> = ["amt-course-registration", "other-repo", "third"]
            .iter()
            .map(|r| base.join(r).to_string_lossy().into_owned())
            .collect();

        // Title token names exactly one → chosen.
        let hint = ActiveHint {
            title_token: Some("amt-course-registration"),
            terminal_cwd: None,
        };
        assert_eq!(
            disambiguate(&cands, &hint),
            Some(
                base.join("amt-course-registration")
                    .to_string_lossy()
                    .into_owned()
            )
        );

        // No hint → genuinely ambiguous, None (C16: none over wrong).
        assert_eq!(disambiguate(&cands, &ActiveHint::default()), None);

        // Terminal cwd inside one repo → walks up to it.
        let deep = base.join("other-repo").join("src");
        std::fs::create_dir_all(&deep).unwrap();
        let hint_term = ActiveHint {
            title_token: None,
            terminal_cwd: Some(deep.to_str().unwrap()),
        };
        assert_eq!(
            disambiguate(&cands, &hint_term),
            Some(base.join("other-repo").to_string_lossy().into_owned())
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn path_matches_token_guards_identity() {
        // Exact focused subfolder matches its token.
        assert!(path_matches_token(
            "/mnt/DevSSD/workspace/amt/amt-course-registration",
            "amt-course-registration"
        ));
        // Case-insensitive.
        assert!(path_matches_token("/home/u/Lychi", "lychi"));
        // A stale/parent path must NOT match a different focused token —
        // this is the guard that stops config's `amt` (or a stale `rturn-api`)
        // from being pinned onto the `amt-course-registration` window.
        assert!(!path_matches_token(
            "/mnt/DevSSD/workspace/amt",
            "amt-course-registration"
        ));
        assert!(!path_matches_token(
            "/mnt/DevSSD/workspace/rturn/rturn-api",
            "amt-course-registration"
        ));
    }

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
