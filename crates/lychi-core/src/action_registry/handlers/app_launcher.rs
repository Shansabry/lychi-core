use async_trait::async_trait;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

use crate::action_registry::handlers::icons::resolve_icon;
use crate::action_registry::{ActionHandler, ActionResult, CompletionItem};
use crate::error::LychiError;

#[derive(Debug)]
struct DesktopEntry {
    name: String,
    exec: String,
    icon: Option<String>,
    /// Cached resolved icon filesystem path
    icon_path: OnceLock<Option<String>>,
}

/// Global cache for desktop entries — discovered once, reused for all queries.
static DESKTOP_ENTRIES: OnceLock<HashMap<String, DesktopEntry>> = OnceLock::new();

pub struct AppLauncher;

impl Default for AppLauncher {
    fn default() -> Self {
        Self::new()
    }
}

impl AppLauncher {
    pub fn new() -> Self {
        Self
    }

    fn entries() -> &'static HashMap<String, DesktopEntry> {
        DESKTOP_ENTRIES.get_or_init(Self::discover_entries)
    }

    fn discover_entries() -> HashMap<String, DesktopEntry> {
        let mut entries = HashMap::new();

        // TODO: cross-platform — Linux-specific XDG paths. macOS needs NSWorkspace/Launch Services,
        // Windows needs Start Menu + shell:AppsFolder enumeration.
        let dirs = [
            PathBuf::from("/usr/share/applications"),
            PathBuf::from("/usr/local/share/applications"),
            PathBuf::from("/var/lib/flatpak/exports/share/applications"),
            PathBuf::from("/var/lib/snapd/desktop/applications"),
            dirs::home_dir()
                .map(|h| h.join(".local/share/applications"))
                .unwrap_or_default(),
            dirs::home_dir()
                .map(|h| h.join(".local/share/flatpak/exports/share/applications"))
                .unwrap_or_default(),
        ];

        for dir in &dirs {
            if let Ok(read_dir) = fs::read_dir(dir) {
                for entry in read_dir.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|ext| ext == "desktop")
                        && let Some(de) = Self::parse_desktop_file(&path)
                    {
                        entries.insert(de.name.to_lowercase(), de);
                    }
                }
            }
        }

        entries
    }

    fn parse_desktop_file(path: &PathBuf) -> Option<DesktopEntry> {
        let content = fs::read_to_string(path).ok()?;
        let mut name = None;
        let mut exec = None;
        let mut icon = None;
        let mut no_display = false;
        let mut hidden = false;
        let mut in_desktop_entry = false;

        for line in content.lines() {
            let line = line.trim();
            if line == "[Desktop Entry]" {
                in_desktop_entry = true;
                continue;
            }
            if line.starts_with('[') {
                in_desktop_entry = false;
                continue;
            }
            if !in_desktop_entry {
                continue;
            }

            if let Some(val) = line.strip_prefix("Name=") {
                if name.is_none() {
                    name = Some(val.to_string());
                }
            } else if let Some(val) = line.strip_prefix("Exec=") {
                exec = Some(
                    val.replace("%u", "")
                        .replace("%U", "")
                        .replace("%f", "")
                        .replace("%F", "")
                        .trim()
                        .to_string(),
                );
            } else if let Some(val) = line.strip_prefix("Icon=") {
                icon = Some(val.to_string());
            } else if line == "NoDisplay=true" {
                no_display = true;
            } else if line == "Hidden=true" {
                hidden = true;
            }
        }

        if no_display || hidden {
            return None;
        }

        Some(DesktopEntry {
            name: name?,
            exec: exec?,
            icon,
            icon_path: OnceLock::new(),
        })
    }

    /// Fuzzy match entries against a query. Returns references sorted by score descending.
    fn fuzzy_match<'a>(
        entries: &'a HashMap<String, DesktopEntry>,
        query: &str,
    ) -> Vec<(&'a DesktopEntry, u16)> {
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

        let mut results: Vec<(&DesktopEntry, u16)> = entries
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

#[async_trait]
impl ActionHandler for AppLauncher {
    fn id(&self) -> &str {
        "open"
    }

    fn description(&self) -> &str {
        "Launch a desktop application"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let query = args.trim();
        if query.is_empty() {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some("Usage: open <application name>".to_string()),
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
        let entries = Self::entries();

        // Use fuzzy match — take the best result
        let matches = Self::fuzzy_match(entries, query);
        let (entry, _) = matches
            .into_iter()
            .next()
            .ok_or_else(|| LychiError::AppNotFound(query.to_string()))?;

        // Split exec into command and args, spawn detached
        let parts: Vec<&str> = entry.exec.split_whitespace().collect();
        let (cmd, cmd_args) = parts.split_first().ok_or_else(|| {
            LychiError::ExecutionFailed(format!("Invalid exec line: {}", entry.exec))
        })?;

        Command::new(cmd)
            .args(cmd_args.iter())
            .spawn()
            .map_err(|e| {
                LychiError::ExecutionFailed(format!("Failed to launch {}: {e}", entry.name))
            })?;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(ActionResult {
            success: true,
            output: Some(format!("Launched {}", entry.name)),
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
        let entries = Self::entries();
        let matches = Self::fuzzy_match(entries, query);

        matches
            .into_iter()
            .take(8)
            .map(|(entry, _score)| {
                // Resolve icon once, cache the result
                let icon_path = entry
                    .icon_path
                    .get_or_init(|| entry.icon.as_deref().and_then(resolve_icon))
                    .clone();
                CompletionItem {
                    label: entry.name.clone(),
                    icon_path,
                    score: _score,
                }
            })
            .collect()
    }
}
