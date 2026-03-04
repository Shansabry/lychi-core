//! Context caching — avoids redundant subprocess spawns on rapid re-summons.
//!
//! Each detector (git, docker, project) gets a lightweight cache keyed by
//! cheap filesystem checks (mtime, HEAD content). Caches are stored in a
//! global `Mutex` and checked at the start of each `gather()` call.
//!
//! Cache lifetime is short (2-5s) — just enough to make rapid summons instant
//! without showing stale data.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use super::network::NetworkContext;
use super::{DockerContext, GitContext, ProjectContext};

// ── Invalidation Reason ─────────────────────────────────────────────────

/// Why a cache entry was invalidated. Stored as `last_invalidation` so the
/// `ctx` debug command can show what caused the most recent cache miss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidationReason {
    /// First access — no cached entry exists yet.
    Cold,
    /// TTL expired.
    Expired,
    /// Git HEAD content changed (branch switch, commit).
    HeadChanged,
    /// Git index mtime changed (git add, git reset, git stash).
    IndexChanged,
    /// Query directory changed (different repo or project root).
    DirChanged,
    /// Project marker file mtime changed (Cargo.toml, package.json, etc.).
    MarkerChanged,
}

impl InvalidationReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "COLD",
            Self::Expired => "EXPIRED",
            Self::HeadChanged => "HEAD_CHANGED",
            Self::IndexChanged => "INDEX_CHANGED",
            Self::DirChanged => "DIR_CHANGED",
            Self::MarkerChanged => "MARKER_CHANGED",
        }
    }
}

// ── Git Cache ────────────────────────────────────────────────────────────

struct GitCacheEntry {
    result: Option<GitContext>,
    repo_root: String,
    /// Content of `.git/HEAD` at cache time.
    head_content: String,
    /// mtime of `.git/index` at cache time (tracks staging area changes).
    index_mtime: Option<SystemTime>,
    created: Instant,
    last_invalidation: Option<InvalidationReason>,
}

static GIT_CACHE: Mutex<Option<GitCacheEntry>> = Mutex::new(None);

const GIT_TTL: Duration = Duration::from_secs(2);

/// Check git cache. Returns `Some(cached_result)` if valid, `None` if stale/missing.
pub fn get_git(repo_root: &str) -> Option<Option<GitContext>> {
    let mut guard = GIT_CACHE.lock().ok()?;

    let reason = match guard.as_ref() {
        None => Some(InvalidationReason::Cold),
        Some(entry) if entry.created.elapsed() > GIT_TTL => Some(InvalidationReason::Expired),
        Some(entry) if entry.repo_root != repo_root => Some(InvalidationReason::DirChanged),
        Some(entry) => {
            let current_head =
                std::fs::read_to_string(Path::new(repo_root).join(".git/HEAD")).unwrap_or_default();
            if current_head.trim() != entry.head_content {
                Some(InvalidationReason::HeadChanged)
            } else {
                let current_index_mtime = Path::new(repo_root)
                    .join(".git/index")
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok());
                if current_index_mtime != entry.index_mtime {
                    Some(InvalidationReason::IndexChanged)
                } else {
                    None // cache hit
                }
            }
        }
    };

    if let Some(reason) = reason {
        // Record why we missed, then return None
        if let Some(entry) = guard.as_mut() {
            entry.last_invalidation = Some(reason);
        }
        return None;
    }

    Some(guard.as_ref().unwrap().result.clone())
}

/// Store a git detection result in the cache.
pub fn set_git(repo_root: &str, result: &Option<GitContext>) {
    let head_content = std::fs::read_to_string(Path::new(repo_root).join(".git/HEAD"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let index_mtime = Path::new(repo_root)
        .join(".git/index")
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok());

    if let Ok(mut guard) = GIT_CACHE.lock() {
        let prev_invalidation = guard.as_ref().and_then(|e| e.last_invalidation);
        *guard = Some(GitCacheEntry {
            result: result.clone(),
            repo_root: repo_root.to_string(),
            head_content,
            index_mtime,
            created: Instant::now(),
            last_invalidation: prev_invalidation,
        });
    }
}

// ── Docker Cache ─────────────────────────────────────────────────────────

struct DockerCacheEntry {
    result: Option<DockerContext>,
    created: Instant,
    last_invalidation: Option<InvalidationReason>,
}

static DOCKER_CACHE: Mutex<Option<DockerCacheEntry>> = Mutex::new(None);

const DOCKER_TTL: Duration = Duration::from_secs(3);

/// Check docker cache. Returns `Some(cached_result)` if valid, `None` if stale.
pub fn get_docker() -> Option<Option<DockerContext>> {
    let mut guard = DOCKER_CACHE.lock().ok()?;

    let reason = match guard.as_ref() {
        None => Some(InvalidationReason::Cold),
        Some(entry) if entry.created.elapsed() > DOCKER_TTL => Some(InvalidationReason::Expired),
        _ => None,
    };

    if let Some(reason) = reason {
        if let Some(entry) = guard.as_mut() {
            entry.last_invalidation = Some(reason);
        }
        return None;
    }

    Some(guard.as_ref().unwrap().result.clone())
}

/// Store a docker detection result in the cache.
pub fn set_docker(result: &Option<DockerContext>) {
    if let Ok(mut guard) = DOCKER_CACHE.lock() {
        let prev_invalidation = guard.as_ref().and_then(|e| e.last_invalidation);
        *guard = Some(DockerCacheEntry {
            result: result.clone(),
            created: Instant::now(),
            last_invalidation: prev_invalidation,
        });
    }
}

// ── Project Cache ────────────────────────────────────────────────────────

struct ProjectCacheEntry {
    result: Option<ProjectContext>,
    /// mtime of the marker file (Cargo.toml, package.json, etc.) at cache time.
    marker_mtime: Option<SystemTime>,
    created: Instant,
    last_invalidation: Option<InvalidationReason>,
}

static PROJECT_CACHE: Mutex<Option<ProjectCacheEntry>> = Mutex::new(None);

const PROJECT_TTL: Duration = Duration::from_secs(5);

/// Check project cache. Returns `Some(cached_result)` if valid, `None` if stale.
pub fn get_project(dir: &str) -> Option<Option<ProjectContext>> {
    let mut guard = PROJECT_CACHE.lock().ok()?;

    let reason = match guard.as_ref() {
        None => Some(InvalidationReason::Cold),
        Some(entry) if entry.created.elapsed() > PROJECT_TTL => Some(InvalidationReason::Expired),
        Some(entry) => {
            if let Some(ref proj) = entry.result {
                if !dir.starts_with(&proj.root) {
                    Some(InvalidationReason::DirChanged)
                } else {
                    let current_mtime = marker_mtime_for(&proj.kind, &proj.root);
                    if current_mtime != entry.marker_mtime {
                        Some(InvalidationReason::MarkerChanged)
                    } else {
                        None // cache hit
                    }
                }
            } else {
                None // cached None result, still valid
            }
        }
    };

    if let Some(reason) = reason {
        if let Some(entry) = guard.as_mut() {
            entry.last_invalidation = Some(reason);
        }
        return None;
    }

    Some(guard.as_ref().unwrap().result.clone())
}

/// Store a project detection result in the cache.
pub fn set_project(result: &Option<ProjectContext>) {
    let marker_mtime = result
        .as_ref()
        .and_then(|proj| marker_mtime_for(&proj.kind, &proj.root));

    if let Ok(mut guard) = PROJECT_CACHE.lock() {
        let prev_invalidation = guard.as_ref().and_then(|e| e.last_invalidation);
        *guard = Some(ProjectCacheEntry {
            result: result.clone(),
            marker_mtime,
            created: Instant::now(),
            last_invalidation: prev_invalidation,
        });
    }
}

/// Get the mtime of the primary marker file for a project kind.
fn marker_mtime_for(kind: &super::ProjectKind, root: &str) -> Option<SystemTime> {
    let marker = match kind {
        super::ProjectKind::Rust => "Cargo.toml",
        super::ProjectKind::Node => "package.json",
        super::ProjectKind::Python => "pyproject.toml",
        super::ProjectKind::Go => "go.mod",
        super::ProjectKind::Flutter => "pubspec.yaml",
        super::ProjectKind::Docker => "Dockerfile",
    };
    Path::new(root)
        .join(marker)
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
}

// ── Network Cache ────────────────────────────────────────────────────────

struct NetworkCacheEntry {
    result: Option<NetworkContext>,
    created: Instant,
    last_invalidation: Option<InvalidationReason>,
}

static NETWORK_CACHE: Mutex<Option<NetworkCacheEntry>> = Mutex::new(None);

const NETWORK_TTL: Duration = Duration::from_secs(10);

/// Check network cache. Returns `Some(cached_result)` if valid, `None` if stale.
pub fn get_network() -> Option<Option<NetworkContext>> {
    let mut guard = NETWORK_CACHE.lock().ok()?;

    let reason = match guard.as_ref() {
        None => Some(InvalidationReason::Cold),
        Some(entry) if entry.created.elapsed() > NETWORK_TTL => Some(InvalidationReason::Expired),
        _ => None,
    };

    if let Some(reason) = reason {
        if let Some(entry) = guard.as_mut() {
            entry.last_invalidation = Some(reason);
        }
        return None;
    }

    Some(guard.as_ref().unwrap().result.clone())
}

/// Store a network detection result in the cache.
pub fn set_network(result: &Option<NetworkContext>) {
    if let Ok(mut guard) = NETWORK_CACHE.lock() {
        let prev_invalidation = guard.as_ref().and_then(|e| e.last_invalidation);
        *guard = Some(NetworkCacheEntry {
            result: result.clone(),
            created: Instant::now(),
            last_invalidation: prev_invalidation,
        });
    }
}

// ── Terminal CWD Cache ───────────────────────────────────────────────────

struct TerminalCwdCacheEntry {
    result: Option<String>,
    source: super::terminal_probe::ProbeSource,
    wm_class: String,
    pid: u32,
    created: Instant,
}

static TERMINAL_CWD_CACHE: Mutex<Option<TerminalCwdCacheEntry>> = Mutex::new(None);

const TERMINAL_CWD_TTL: Duration = Duration::from_secs(2);

/// Check terminal CWD cache. Returns `Some(cached_result)` if valid.
pub fn get_terminal_cwd(wm_class: &str, pid: u32) -> Option<Option<String>> {
    let guard = TERMINAL_CWD_CACHE.lock().ok()?;
    let entry = guard.as_ref()?;

    if entry.created.elapsed() > TERMINAL_CWD_TTL {
        return None;
    }
    if entry.wm_class != wm_class || entry.pid != pid {
        return None;
    }

    Some(entry.result.clone())
}

/// Store a terminal CWD probe result in the cache.
pub fn set_terminal_cwd(
    wm_class: &str,
    pid: u32,
    result: &Option<String>,
    source: super::terminal_probe::ProbeSource,
) {
    if let Ok(mut guard) = TERMINAL_CWD_CACHE.lock() {
        *guard = Some(TerminalCwdCacheEntry {
            result: result.clone(),
            source,
            wm_class: wm_class.to_string(),
            pid,
            created: Instant::now(),
        });
    }
}

// ── Cache stats (for ctx debug) ──────────────────────────────────────────

/// Summary of cache state for debug display.
pub struct CacheStats {
    pub git_age_ms: Option<u64>,
    pub git_invalidation: Option<InvalidationReason>,
    pub docker_age_ms: Option<u64>,
    pub docker_invalidation: Option<InvalidationReason>,
    pub project_age_ms: Option<u64>,
    pub project_invalidation: Option<InvalidationReason>,
    pub network_age_ms: Option<u64>,
    pub network_invalidation: Option<InvalidationReason>,
    pub terminal_cwd_age_ms: Option<u64>,
    pub terminal_cwd_source: Option<String>,
}

/// Get current cache ages for the `ctx` debug command.
pub fn stats() -> CacheStats {
    let (git_age_ms, git_invalidation) = GIT_CACHE
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref()
                .map(|e| (e.created.elapsed().as_millis() as u64, e.last_invalidation))
        })
        .map(|(age, inv)| (Some(age), inv))
        .unwrap_or((None, None));
    let (docker_age_ms, docker_invalidation) = DOCKER_CACHE
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref()
                .map(|e| (e.created.elapsed().as_millis() as u64, e.last_invalidation))
        })
        .map(|(age, inv)| (Some(age), inv))
        .unwrap_or((None, None));
    let (project_age_ms, project_invalidation) = PROJECT_CACHE
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref()
                .map(|e| (e.created.elapsed().as_millis() as u64, e.last_invalidation))
        })
        .map(|(age, inv)| (Some(age), inv))
        .unwrap_or((None, None));

    let (network_age_ms, network_invalidation) = NETWORK_CACHE
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref()
                .map(|e| (e.created.elapsed().as_millis() as u64, e.last_invalidation))
        })
        .map(|(age, inv)| (Some(age), inv))
        .unwrap_or((None, None));

    let (terminal_cwd_age_ms, terminal_cwd_source) = TERMINAL_CWD_CACHE
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref().map(|e| {
                (
                    e.created.elapsed().as_millis() as u64,
                    e.source.as_str().to_string(),
                )
            })
        })
        .map(|(age, src)| (Some(age), Some(src)))
        .unwrap_or((None, None));

    CacheStats {
        git_age_ms,
        git_invalidation,
        docker_age_ms,
        docker_invalidation,
        project_age_ms,
        project_invalidation,
        network_age_ms,
        network_invalidation,
        terminal_cwd_age_ms,
        terminal_cwd_source,
    }
}
