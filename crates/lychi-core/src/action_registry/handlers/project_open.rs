use async_trait::async_trait;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::RwLock;
use std::time::Instant;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem};
use crate::error::LychiError;

#[derive(Debug)]
struct ProjectEntry {
    name: String,
    path: PathBuf,
}

type ProjectCacheData = (Vec<String>, HashMap<String, ProjectEntry>);

/// Cache of discovered projects. Uses RwLock so it can be invalidated
/// when project directories change in settings.
static PROJECT_CACHE: RwLock<Option<ProjectCacheData>> = RwLock::new(None);

pub struct ProjectOpen {
    directories: Vec<String>,
}

impl ProjectOpen {
    pub fn with_directories(directories: Vec<String>) -> Self {
        // Invalidate cache when directories change
        if let Ok(mut guard) = PROJECT_CACHE.write() {
            *guard = None;
        }
        Self { directories }
    }

    fn expand_path(path: &str) -> PathBuf {
        if let Some(rest) = path.strip_prefix("~/") {
            dirs::home_dir()
                .map(|h| h.join(rest))
                .unwrap_or_else(|| PathBuf::from(path))
        } else {
            PathBuf::from(path)
        }
    }

    fn discover_projects(directories: &[String]) -> HashMap<String, ProjectEntry> {
        let mut entries = HashMap::new();

        for dir_str in directories {
            let dir = Self::expand_path(dir_str);
            Self::scan_dir(&dir, 0, 3, &mut entries);
        }

        entries
    }

    fn scan_dir(
        dir: &PathBuf,
        depth: usize,
        max_depth: usize,
        entries: &mut HashMap<String, ProjectEntry>,
    ) {
        if depth >= max_depth {
            return;
        }
        let read_dir = match fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if name.starts_with('.') {
                continue;
            }
            let key = name.to_lowercase();
            entries.entry(key).or_insert_with(|| ProjectEntry {
                name,
                path: path.clone(),
            });
            // Recurse into subdirectories
            Self::scan_dir(&path, depth + 1, max_depth, entries);
        }
    }

    fn get_projects(&self) -> HashMap<String, ProjectEntry> {
        // Check cache
        if let Ok(guard) = PROJECT_CACHE.read()
            && let Some((cached_dirs, entries)) = guard.as_ref()
            && cached_dirs == &self.directories
        {
            // Clone the entries since we can't return a reference to the guard
            return entries
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        ProjectEntry {
                            name: v.name.clone(),
                            path: v.path.clone(),
                        },
                    )
                })
                .collect();
        }

        // Discover and cache
        let entries = Self::discover_projects(&self.directories);
        if let Ok(mut guard) = PROJECT_CACHE.write() {
            *guard = Some((self.directories.clone(), {
                entries
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            ProjectEntry {
                                name: v.name.clone(),
                                path: v.path.clone(),
                            },
                        )
                    })
                    .collect()
            }));
        }
        entries
    }

    fn fuzzy_match<'a>(
        entries: &'a HashMap<String, ProjectEntry>,
        query: &str,
    ) -> Vec<(&'a ProjectEntry, u16)> {
        if query.is_empty() {
            return Vec::new();
        }

        let mut matcher = Matcher::new(Config::DEFAULT);
        let pattern = Atom::new(
            query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
            false,
        );

        let mut results: Vec<(&ProjectEntry, u16)> = entries
            .values()
            .filter_map(|entry| {
                let mut buf = Vec::new();
                let haystack = Utf32Str::new(&entry.name, &mut buf);
                let score = pattern.score(haystack, &mut matcher)?;
                Some((entry, score))
            })
            .collect();

        results.sort_by(|a, b| b.1.cmp(&a.1));
        results
    }
}

/// Invalidate the project cache so the next lookup re-scans directories.
pub fn invalidate_project_cache() {
    if let Ok(mut guard) = PROJECT_CACHE.write() {
        *guard = None;
    }
}

#[async_trait]
impl ActionHandler for ProjectOpen {
    fn id(&self) -> &str {
        "project"
    }

    fn description(&self) -> &str {
        "Open a project folder in the code editor"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let query = args.trim();
        if query.is_empty() {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some("Usage: project <name>".to_string()),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            });
        }

        let start = Instant::now();
        let entries = self.get_projects();

        let matches = Self::fuzzy_match(&entries, query);
        let (entry, _) = matches.into_iter().next().ok_or_else(|| {
            LychiError::ExecutionFailed(format!("No project matching '{query}' found"))
        })?;

        let path = &entry.path;

        Command::new("code").arg(path).spawn().map_err(|e| {
            LychiError::ExecutionFailed(format!("Failed to open {} in editor: {e}", entry.name))
        })?;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(ActionResult {
            success: true,
            output: Some(format!("Opened {} in editor", entry.name)),
            error: None,
            duration_ms,
            routed_by: None,
            open_url: None,
            needs_confirmation: None,
            risk_level: None,
                output_type: None,
                executed_args: None,
        })
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let query = partial.trim();
        if query.is_empty() {
            return Vec::new();
        }

        let entries = self.get_projects();
        let matches = Self::fuzzy_match(&entries, query);

        matches
            .into_iter()
            .take(8)
            .map(|(entry, score)| CompletionItem {
                label: entry.name.clone(),
                icon_path: None,
                score,
            })
            .collect()
    }
}
