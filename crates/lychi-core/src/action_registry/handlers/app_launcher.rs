use async_trait::async_trait;
use std::sync::Arc;
use std::time::Instant;

use redb::Database;

use crate::action_registry::handlers::icons::resolve_icon;
use crate::action_registry::{ActionHandler, ActionResult, CompletionItem};
use crate::db::frecency;
use crate::desktop_apps::{AUTO_LAUNCH_THRESHOLD, app_index};
use crate::error::LychiError;

pub struct AppLauncher {
    db: Arc<Database>,
}

impl AppLauncher {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Pre-warm the AppIndex and resolve all icon paths.
    /// Call from `spawn_blocking` at startup so the first query is instant.
    pub fn warmup() {
        let t0 = Instant::now();
        super::icons::warmup_icons();
        let index = app_index();
        let t_index = t0.elapsed();

        // Pre-resolve icon paths
        for entry in &index.entries {
            let _ = entry
                .icon_path
                .get_or_init(|| entry.icon.as_deref().and_then(resolve_icon));
        }

        tracing::info!(
            "[app_launcher] warmup done: index={:.0}ms icons={:.0}ms total={:.0}ms ({} apps)",
            t_index.as_secs_f64() * 1000.0,
            (t0.elapsed() - t_index).as_secs_f64() * 1000.0,
            t0.elapsed().as_secs_f64() * 1000.0,
            index.entries.len()
        );
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
                error: Some("Usage: open <application name>".to_string()),
                ..Default::default()
            });
        }

        let start = Instant::now();
        let index = app_index();

        // Fast-path: Phase 3 passes an absolute .desktop path as the canonical ID.
        // Direct lookup — no fuzzy matching needed.
        let entry = if index.is_desktop_path(query) {
            index.by_path(query)
        } else {
            // Human query: use AppIndex scoring to find best match
            index
                .best_match(query)
                .filter(|(_, score)| *score >= AUTO_LAUNCH_THRESHOLD)
                .map(|(id, _)| index.entry(id))
        };

        let Some(entry) = entry else {
            // No confident match — soft failure so executor can fall back to web
            return Ok(ActionResult {
                duration_ms: start.elapsed().as_millis() as u64,
                ..Default::default()
            });
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        // Record frecency access (fire-and-forget)
        let key = entry.name.to_lowercase();
        let _ = frecency::record(&self.db, &key);

        // Smart open: focus if already running, launch if not.
        // Try StartupWMClass first (most precise), then exec basename, then display name.
        #[cfg(target_os = "linux")]
        let focus_target: Option<String> = {
            let windows = super::app_control::get_windows();
            [
                entry.wm_class.as_deref(),
                Some(entry.exec_basename.as_str()),
                Some(entry.name.as_str()),
            ]
            .into_iter()
            .flatten()
            .find_map(|candidate| {
                super::app_control::find_window(&windows, candidate).map(|w| w.wm_class.clone())
            })
        };
        #[cfg(not(target_os = "linux"))]
        let focus_target: Option<String> = None;

        if let Some(wm_class) = focus_target {
            tracing::info!(
                "[open] {} already running — focusing (wm_class={wm_class})",
                entry.name
            );
            Ok(ActionResult {
                success: true,
                output: Some(format!("Focused {}", entry.name)),
                focus_app: Some(wm_class),
                duration_ms,
                ..Default::default()
            })
        } else {
            Ok(ActionResult {
                success: true,
                output: Some(format!("Launched {}", entry.name)),
                launch_desktop: Some(entry.desktop_path.clone()),
                duration_ms,
                ..Default::default()
            })
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let query = partial.trim();
        if query.is_empty() {
            return Vec::new();
        }

        let index = app_index();
        let candidates = index.candidates(query, 12);

        // Load frecency scores (single read transaction)
        let frecency_scores = frecency::get_scores(&self.db);

        let mut items: Vec<CompletionItem> = candidates
            .into_iter()
            .map(|(id, app_score)| {
                let entry = index.entry(id);
                let icon_path = entry.icon_path.get().cloned().flatten();
                let key = entry.name.to_lowercase();
                let frecency_val = frecency_scores.get(&key).copied().unwrap_or(0.0);

                // Blend: 70% app score + 30% frecency boost
                // Convert app_score (0.0–1.0) to u16 range (~0–1000)
                let base = (app_score * 700.0) as u16;
                let frecency_boost = (frecency_val * 300.0) as u16;
                let blended = base.saturating_add(frecency_boost);

                let description = None;

                CompletionItem {
                    label: entry.name.clone(),
                    icon_path,
                    score: blended,
                    description,
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
