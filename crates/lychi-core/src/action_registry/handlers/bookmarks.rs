use async_trait::async_trait;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem, ExecContext};
use crate::error::LychiError;

/// Cached nucleo matcher.
static MATCHER: Mutex<Option<Matcher>> = Mutex::new(None);

const CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct Bookmark {
    title: String,
    url: String,
    #[allow(dead_code)] // Used for future folder-path display in completions
    folder: Option<String>,
}

struct BookmarkCache {
    bookmarks: Vec<Bookmark>,
    loaded_at: Instant,
}

static CACHE: RwLock<Option<BookmarkCache>> = RwLock::new(None);

/// Detect Chromium-based browser bookmark file paths.
fn detect_chromium_paths() -> Vec<PathBuf> {
    let Some(config_dir) = dirs::config_dir() else {
        return Vec::new();
    };

    let browser_dirs = [
        "google-chrome",
        "chromium",
        "BraveSoftware/Brave-Browser",
        "microsoft-edge",
    ];

    let mut paths = Vec::new();

    for browser_dir in &browser_dirs {
        let base = config_dir.join(browser_dir);

        // Check Default profile
        let default_bookmarks = base.join("Default/Bookmarks");
        if default_bookmarks.exists() {
            paths.push(default_bookmarks);
        }

        // Check numbered profiles (Profile 1, Profile 2, etc.)
        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("Profile ") {
                    let profile_bookmarks = entry.path().join("Bookmarks");
                    if profile_bookmarks.exists() {
                        paths.push(profile_bookmarks);
                    }
                }
            }
        }
    }

    paths
}

/// Parse a Chromium Bookmarks JSON file.
fn parse_chromium_bookmarks(path: &Path) -> Vec<Bookmark> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };

    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Vec::new();
    };

    let mut bookmarks = Vec::new();
    let Some(roots) = json.get("roots").and_then(|r| r.as_object()) else {
        return bookmarks;
    };

    for (_root_name, root_node) in roots {
        collect_bookmarks(root_node, None, &mut bookmarks);
    }

    bookmarks
}

/// Recursively collect bookmarks from a Chromium bookmark node tree.
fn collect_bookmarks(
    node: &serde_json::Value,
    parent_folder: Option<&str>,
    bookmarks: &mut Vec<Bookmark>,
) {
    let Some(node_type) = node.get("type").and_then(|t| t.as_str()) else {
        return;
    };

    match node_type {
        "url" => {
            let title = node
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let url = node
                .get("url")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string();

            if !url.is_empty() {
                bookmarks.push(Bookmark {
                    title,
                    url,
                    folder: parent_folder.map(|s| s.to_string()),
                });
            }
        }
        "folder" => {
            let folder_name = node
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("Unknown");

            let full_path = match parent_folder {
                Some(parent) => format!("{parent}/{folder_name}"),
                None => folder_name.to_string(),
            };

            if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
                for child in children {
                    collect_bookmarks(child, Some(&full_path), bookmarks);
                }
            }
        }
        _ => {}
    }
}

/// Load all bookmarks from detected browsers, with deduplication.
fn load_all_bookmarks() -> Vec<Bookmark> {
    let paths = detect_chromium_paths();
    let mut all_bookmarks = Vec::new();
    let mut seen_urls = std::collections::HashSet::new();

    for path in paths {
        for bm in parse_chromium_bookmarks(&path) {
            if seen_urls.insert(bm.url.clone()) {
                all_bookmarks.push(bm);
            }
        }
    }

    all_bookmarks
}

/// Get bookmarks from cache, refreshing if stale.
fn get_bookmarks() -> Vec<Bookmark> {
    // Try read cache first
    {
        let cache = CACHE.read().unwrap();
        if let Some(ref c) = *cache
            && c.loaded_at.elapsed() < CACHE_TTL
        {
            return c.bookmarks.clone();
        }
    }

    // Cache miss or stale — reload
    let bookmarks = load_all_bookmarks();
    let mut cache = CACHE.write().unwrap();
    *cache = Some(BookmarkCache {
        bookmarks: bookmarks.clone(),
        loaded_at: Instant::now(),
    });
    bookmarks
}

pub struct BookmarkHandler;

impl Default for BookmarkHandler {
    fn default() -> Self {
        Self
    }
}

impl BookmarkHandler {
    pub fn new() -> Self {
        Self
    }

    /// Truncate a URL for display, removing protocol prefix.
    fn truncate_url(url: &str, max_len: usize) -> String {
        let display = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(url);

        if display.len() > max_len {
            format!("{}…", &display[..max_len - 1])
        } else {
            display.to_string()
        }
    }
}

#[async_trait]
impl ActionHandler for BookmarkHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["bm", "bookmark"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "bm"
    }

    fn description(&self) -> &str {
        "Search and open browser bookmarks (bm <query>)"
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            return Ok(ActionResult::err("Usage: bm <query>".to_string()));
        }

        let bookmarks = get_bookmarks();

        // If the arg looks like a URL (from a completion's description), open it directly
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            return Ok(ActionResult::navigate(trimmed.to_string(), false));
        }

        // Find best matching bookmark by title
        let lower = trimmed.to_lowercase();
        let found = bookmarks
            .iter()
            .find(|bm| bm.title.to_lowercase() == lower)
            .or_else(|| {
                bookmarks
                    .iter()
                    .find(|bm| bm.title.to_lowercase().contains(&lower))
            });

        match found {
            Some(bm) => Ok(ActionResult::navigate(bm.url.clone(), false)),
            None => Ok(ActionResult::err(format!(
                "No bookmark found for: {trimmed}"
            ))),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let bookmarks = get_bookmarks();
        let query = partial.trim();

        // Empty query → show recent/all bookmarks (first 20)
        if query.is_empty() {
            return bookmarks
                .iter()
                .take(20)
                .enumerate()
                .map(|(i, bm)| CompletionItem {
                    label: bm.title.clone(),
                    icon_path: None,
                    score: (20 - i) as u16,
                    description: Some(Self::truncate_url(&bm.url, 60)),
                    reason: None,
                    thumb_b64: None,
                    run: Some(format!("bm {}", bm.url)),
                    ..Default::default()
                })
                .collect();
        }

        // Fuzzy match against title and URL
        let mut matcher_guard = MATCHER.lock().unwrap();
        let matcher = matcher_guard.get_or_insert_with(|| Matcher::new(Config::DEFAULT));

        let pattern = Atom::new(
            query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
            false,
        );

        let mut buf = Vec::new();
        let mut results: Vec<(usize, u16)> = Vec::new();

        for (i, bm) in bookmarks.iter().enumerate() {
            // Match against title
            let haystack_str = format!("{} {}", bm.title, bm.url);
            buf.clear();
            let haystack = Utf32Str::new(&haystack_str, &mut buf);
            if let Some(score) = pattern.score(haystack, matcher) {
                results.push((i, score));
            }
        }

        results.sort_by(|a, b| b.1.cmp(&a.1));
        results.truncate(20);

        results
            .into_iter()
            .filter_map(|(i, score)| {
                bookmarks.get(i).map(|bm| CompletionItem {
                    label: bm.title.clone(),
                    icon_path: None,
                    score,
                    description: Some(Self::truncate_url(&bm.url, 60)),
                    reason: None,
                    thumb_b64: None,
                    run: Some(format!("bm {}", bm.url)),
                    ..Default::default()
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_url() {
        assert_eq!(
            BookmarkHandler::truncate_url("https://github.com/anthropics/claude-code", 30),
            "github.com/anthropics/claude-…"
        );
        assert_eq!(
            BookmarkHandler::truncate_url("https://example.com", 30),
            "example.com"
        );
    }

    #[test]
    fn test_detect_chromium_paths_runs() {
        // Should not panic, may return empty on systems without Chrome
        let _paths = detect_chromium_paths();
    }
}
