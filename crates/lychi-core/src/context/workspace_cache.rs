//! Per-window workspace cache + root-index LRU for IDE workspace detection.
//!
//! The workspace cache maps `window_id` → last-known workspace path. It is
//! best-effort: fresh `gather()` always runs and updates/overwrites the cache.
//! The cache is consulted by the fast path to seed IDE workspace hints for
//! windows that were previously resolved.
//!
//! The root-index LRU caches `(root, token) → Option<path>` results from
//! `find_nested()` so repeated summons don't re-walk the same directory tree.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Cached workspace resolution for a specific window.
#[derive(Clone, Debug)]
pub struct CachedWorkspace {
    /// Resolved absolute path to the project root.
    pub path: String,
    /// The folder token extracted from the window title (e.g. "Lychi", "fcc").
    pub token: String,
    /// Which project marker validated this path (e.g. ".git", "Cargo.toml").
    pub marker: String,
    /// When this entry was resolved.
    pub resolved_at: Instant,
}

/// How long a workspace cache entry is trusted without revalidation.
const CACHE_TTL: Duration = Duration::from_secs(600); // 10 minutes

/// Hard cap on the per-window workspace cache. Keys are window UUIDs, so a
/// closed window's entry never comes back — without a cap they accumulate for
/// the whole session. 256 is far more distinct windows than any real session.
const WORKSPACE_CACHE_MAX_ENTRIES: usize = 256;

/// Per-window workspace cache. Key = `window_id` (KWin UUID / X11 hex ID).
static WORKSPACE_CACHE: Mutex<Option<HashMap<String, CachedWorkspace>>> = Mutex::new(None);

/// Store a workspace resolution for a window.
pub fn set(window_id: &str, entry: CachedWorkspace) {
    let Ok(mut guard) = WORKSPACE_CACHE.lock() else {
        return;
    };
    let map = guard.get_or_insert_with(HashMap::new);
    // Bound the map: at capacity, drop the oldest entry (closed windows never
    // re-key, so the oldest is the most likely to be dead). Cheap O(n) scan only
    // when full, which is rare.
    if map.len() >= WORKSPACE_CACHE_MAX_ENTRIES
        && !map.contains_key(window_id)
        && let Some(oldest) = map
            .iter()
            .min_by_key(|(_, v)| v.resolved_at)
            .map(|(k, _)| k.clone())
    {
        map.remove(&oldest);
    }
    map.insert(window_id.to_string(), entry);
}

/// Look up a cached workspace for a window. Returns `None` if:
/// - No entry exists for this `window_id`
/// - The entry is older than `CACHE_TTL` AND the path/marker no longer exists on disk
///
/// Evicts stale entries on failed revalidation.
pub fn get(window_id: &str) -> Option<CachedWorkspace> {
    let Ok(mut guard) = WORKSPACE_CACHE.lock() else {
        return None;
    };
    let entry = guard.as_ref()?.get(window_id)?.clone();

    // Within TTL → trust without revalidation
    if entry.resolved_at.elapsed() < CACHE_TTL {
        return Some(entry);
    }

    // Beyond TTL → cheap revalidation: path must still exist + marker present
    let path = std::path::Path::new(&entry.path);
    if path.is_dir() && path.join(&entry.marker).exists() {
        return Some(entry);
    }

    // Stale — evict immediately
    if let Some(map) = guard.as_mut() {
        map.remove(window_id);
    }
    None
}

// ── Root-Index LRU ──────────────────────────────────────────────────────────

/// Cached result of a `find_nested()` search: `(root, token) → Option<path>`.
#[derive(Clone, Debug)]
struct RootIndexEntry {
    result: Option<String>,
    resolved_at: Instant,
}

const ROOT_INDEX_TTL: Duration = Duration::from_secs(300); // 5 minutes
const ROOT_INDEX_MAX_ENTRIES: usize = 100;

static ROOT_INDEX: Mutex<Option<HashMap<(String, String), RootIndexEntry>>> = Mutex::new(None);

/// Look up a cached `find_nested()` result.
pub fn get_root_index(root: &str, token: &str) -> Option<Option<String>> {
    let Ok(guard) = ROOT_INDEX.lock() else {
        return None;
    };
    let entry = guard
        .as_ref()?
        .get(&(root.to_string(), token.to_string()))?;
    if entry.resolved_at.elapsed() < ROOT_INDEX_TTL {
        Some(entry.result.clone())
    } else {
        None // expired
    }
}

/// Store a `find_nested()` result in the root-index LRU.
pub fn set_root_index(root: &str, token: &str, result: Option<String>) {
    let Ok(mut guard) = ROOT_INDEX.lock() else {
        return;
    };
    let map = guard.get_or_insert_with(HashMap::new);

    // Simple eviction: if at capacity, remove oldest entry
    if map.len() >= ROOT_INDEX_MAX_ENTRIES
        && let Some(oldest_key) = map
            .iter()
            .min_by_key(|(_, v)| v.resolved_at)
            .map(|(k, _)| k.clone())
    {
        map.remove(&oldest_key);
    }

    map.insert(
        (root.to_string(), token.to_string()),
        RootIndexEntry {
            result,
            resolved_at: Instant::now(),
        },
    );
}

// ── Strong-Child Cache ─────────────────────────────────────────────────────

/// Cached result of `has_strong_child()`: does a soft-marker directory
/// contain a child/grandchild with a strong marker?
#[derive(Clone, Debug)]
struct StrongChildEntry {
    result: bool,
    resolved_at: Instant,
}

const STRONG_CHILD_TTL: Duration = Duration::from_secs(300); // 5 minutes
const STRONG_CHILD_MAX_ENTRIES: usize = 256;

static STRONG_CHILD_CACHE: Mutex<Option<HashMap<String, StrongChildEntry>>> = Mutex::new(None);

/// Look up a cached strong-child result. Returns `None` if not cached or expired.
pub fn get_strong_child(path: &str) -> Option<bool> {
    let Ok(guard) = STRONG_CHILD_CACHE.lock() else {
        return None;
    };
    let entry = guard.as_ref()?.get(path)?;
    if entry.resolved_at.elapsed() < STRONG_CHILD_TTL {
        Some(entry.result)
    } else {
        None
    }
}

/// Store a strong-child result.
pub fn set_strong_child(path: &str, result: bool) {
    let Ok(mut guard) = STRONG_CHILD_CACHE.lock() else {
        return;
    };
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() >= STRONG_CHILD_MAX_ENTRIES
        && !map.contains_key(path)
        && let Some(oldest) = map
            .iter()
            .min_by_key(|(_, v)| v.resolved_at)
            .map(|(k, _)| k.clone())
    {
        map.remove(&oldest);
    }
    map.insert(
        path.to_string(),
        StrongChildEntry {
            result,
            resolved_at: Instant::now(),
        },
    );
}

// ── Code-Root Cache ──────────────────────────────────────────────────────

/// Cached result of `resolve_code_root()`: the resolved code root path
/// for an IDE workspace, or None if no unique candidate was found.
#[derive(Clone, Debug)]
struct CodeRootEntry {
    result: Option<String>,
    resolved_at: Instant,
}

const CODE_ROOT_TTL: Duration = Duration::from_secs(300); // 5 minutes
const CODE_ROOT_MAX_ENTRIES: usize = 256;

static CODE_ROOT_CACHE: Mutex<Option<HashMap<String, CodeRootEntry>>> = Mutex::new(None);

/// Look up a cached code-root result. Returns `None` if not cached or expired.
/// Inner `Option<String>` is `None` when no unique candidate exists.
pub fn get_code_root(workspace: &str) -> Option<Option<String>> {
    let Ok(guard) = CODE_ROOT_CACHE.lock() else {
        return None;
    };
    let entry = guard.as_ref()?.get(workspace)?;
    if entry.resolved_at.elapsed() < CODE_ROOT_TTL {
        Some(entry.result.clone())
    } else {
        None
    }
}

/// Store a code-root result (even None, to avoid re-scanning).
pub fn set_code_root(workspace: &str, result: Option<String>) {
    let Ok(mut guard) = CODE_ROOT_CACHE.lock() else {
        return;
    };
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() >= CODE_ROOT_MAX_ENTRIES
        && !map.contains_key(workspace)
        && let Some(oldest) = map
            .iter()
            .min_by_key(|(_, v)| v.resolved_at)
            .map(|(k, _)| k.clone())
    {
        map.remove(&oldest);
    }
    map.insert(
        workspace.to_string(),
        CodeRootEntry {
            result,
            resolved_at: Instant::now(),
        },
    );
}

/// Evict a stale code-root entry (called when revalidation fails).
pub fn evict_code_root(workspace: &str) {
    let Ok(mut guard) = CODE_ROOT_CACHE.lock() else {
        return;
    };
    if let Some(map) = guard.as_mut() {
        map.remove(workspace);
    }
}

// ── Periodic sweep ───────────────────────────────────────────────────────────

/// Drop every TTL-expired entry from all four caches. The per-`set` caps bound
/// worst-case memory, but entries for closed windows / stale paths would sit at
/// their last size until the cap forces eviction — this reclaims them promptly.
/// Cheap: a `retain` over maps that hold at most a few hundred entries. Call it
/// from a periodic background task (see the caller in `src-tauri`).
///
/// `WORKSPACE_CACHE` intentionally keeps entries past `CACHE_TTL` (they stay
/// usable via on-read revalidation), so we sweep it on a longer grace so a
/// still-open window's entry isn't dropped just for being idle. The other three
/// are pure memoization: once expired they carry no value, so drop at TTL.
pub fn sweep_expired() {
    let workspace_grace = CACHE_TTL * 4; // ~40 min: well past any revalidation window
    if let Ok(mut guard) = WORKSPACE_CACHE.lock()
        && let Some(map) = guard.as_mut()
    {
        map.retain(|_, e| e.resolved_at.elapsed() < workspace_grace);
    }
    if let Ok(mut guard) = ROOT_INDEX.lock()
        && let Some(map) = guard.as_mut()
    {
        map.retain(|_, e| e.resolved_at.elapsed() < ROOT_INDEX_TTL);
    }
    if let Ok(mut guard) = STRONG_CHILD_CACHE.lock()
        && let Some(map) = guard.as_mut()
    {
        map.retain(|_, e| e.resolved_at.elapsed() < STRONG_CHILD_TTL);
    }
    if let Ok(mut guard) = CODE_ROOT_CACHE.lock()
        && let Some(map) = guard.as_mut()
    {
        map.retain(|_, e| e.resolved_at.elapsed() < CODE_ROOT_TTL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_drops_expired_memoization_entries_but_keeps_fresh_ones() {
        // Fresh entry stays; a manually-aged entry is swept.
        set_strong_child("/fresh/path", true);
        {
            let mut guard = STRONG_CHILD_CACHE.lock().unwrap();
            let map = guard.get_or_insert_with(HashMap::new);
            map.insert(
                "/expired/path".to_string(),
                StrongChildEntry {
                    result: true,
                    resolved_at: Instant::now() - STRONG_CHILD_TTL - Duration::from_secs(1),
                },
            );
        }
        sweep_expired();
        assert_eq!(get_strong_child("/fresh/path"), Some(true));
        assert_eq!(get_strong_child("/expired/path"), None);
    }

    #[test]
    fn workspace_set_evicts_oldest_at_capacity() {
        // Fill past capacity; the map never exceeds the cap.
        for i in 0..(WORKSPACE_CACHE_MAX_ENTRIES + 10) {
            set(
                &format!("win-{i}"),
                CachedWorkspace {
                    path: "/p".into(),
                    token: "t".into(),
                    marker: ".git".into(),
                    resolved_at: Instant::now(),
                },
            );
        }
        let guard = WORKSPACE_CACHE.lock().unwrap();
        let len = guard.as_ref().map(|m| m.len()).unwrap_or(0);
        assert!(
            len <= WORKSPACE_CACHE_MAX_ENTRIES,
            "cache exceeded cap: {len}"
        );
    }
}
