use async_trait::async_trait;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use std::sync::Arc;

use redb::Database;

use crate::action_registry::handlers::icons::resolve_icon;
use crate::action_registry::{ActionHandler, ActionResult, CompletionItem};
use crate::db::frecency;
use crate::error::LychiError;

#[derive(Debug)]
struct DesktopEntry {
    name: String,
    #[allow(dead_code)] // kept to filter entries without Exec= during discovery
    exec: String,
    icon: Option<String>,
    /// Absolute path to the .desktop file (used by `gio launch`)
    desktop_path: String,
    /// Cached resolved icon filesystem path
    icon_path: OnceLock<Option<String>>,
}

/// Global cache for desktop entries — discovered once, reused for all queries.
static DESKTOP_ENTRIES: OnceLock<HashMap<String, DesktopEntry>> = OnceLock::new();

/// Cached nucleo matcher — reused across calls to avoid ~192ms cold-start on first invocation.
static MATCHER: Mutex<Option<Matcher>> = Mutex::new(None);

pub struct AppLauncher {
    db: Arc<Database>,
}

impl AppLauncher {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    fn entries() -> &'static HashMap<String, DesktopEntry> {
        DESKTOP_ENTRIES.get_or_init(Self::discover_entries)
    }

    /// Pre-warm desktop entries and icon paths.
    /// Call from `spawn_blocking` at startup so the first completions call is instant.
    pub fn warmup() {
        let t0 = std::time::Instant::now();
        // Build icon index first so resolve_icon() calls below are fast
        super::icons::warmup_icons();
        let entries = Self::entries();
        let t_entries = t0.elapsed();
        for entry in entries.values() {
            let _ = entry
                .icon_path
                .get_or_init(|| entry.icon.as_deref().and_then(resolve_icon));
        }
        // Pre-warm the nucleo matcher so first real query doesn't pay cold-start cost
        {
            let mut guard = MATCHER.lock().unwrap();
            guard.get_or_insert_with(|| Matcher::new(Config::DEFAULT));
        }

        tracing::info!(
            "[app_launcher] warmup done: entries={:.0}ms icons={:.0}ms total={:.0}ms ({} apps)",
            t_entries.as_secs_f64() * 1000.0,
            (t0.elapsed() - t_entries).as_secs_f64() * 1000.0,
            t0.elapsed().as_secs_f64() * 1000.0,
            entries.len()
        );
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
            desktop_path: path.to_string_lossy().into_owned(),
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

        let mut guard = MATCHER.lock().unwrap();
        let matcher = guard.get_or_insert_with(|| Matcher::new(Config::DEFAULT));
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
                let score = pattern.score(haystack, matcher)?;
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
                launch_desktop: None,
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

        let duration_ms = start.elapsed().as_millis() as u64;

        // Record frecency access (fire-and-forget, don't block response)
        let key = entry.name.to_lowercase();
        let _ = frecency::record(&self.db, &key);

        // Return launch_desktop so the Tauri side can launch via GIO DesktopAppInfo
        // with proper GDK AppLaunchContext (handles working directory, D-Bus activation,
        // quoted Exec args, Terminal=true, Wayland activation tokens, etc.).
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
            launch_desktop: Some(entry.desktop_path.clone()),
        })
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let query = partial.trim();
        if query.is_empty() {
            return Vec::new();
        }
        // Non-blocking: return empty if warmup hasn't populated entries yet
        let Some(entries) = DESKTOP_ENTRIES.get() else {
            return Vec::new();
        };
        let matches = Self::fuzzy_match(entries, query);

        // Load frecency scores (single read transaction)
        let frecency_scores = frecency::get_scores(&self.db);

        let mut items: Vec<CompletionItem> = matches
            .into_iter()
            .take(12) // Take more to allow re-ranking
            .map(|(entry, nucleo_score)| {
                let icon_path = entry.icon_path.get().cloned().flatten();
                let key = entry.name.to_lowercase();
                let frecency_val = frecency_scores.get(&key).copied().unwrap_or(0.0);

                // Blend: 70% nucleo + 30% frecency (frecency normalized to nucleo range)
                let frecency_boost = (frecency_val * 300.0) as u16; // max 300 points
                let blended = nucleo_score.saturating_add(frecency_boost);

                CompletionItem {
                    label: entry.name.clone(),
                    icon_path,
                    score: blended,
                    description: None,
                    reason: None,
                }
            })
            .collect();

        // Re-sort by blended score and take top 8
        items.sort_by(|a, b| b.score.cmp(&a.score));
        items.truncate(8);
        items
    }
}
