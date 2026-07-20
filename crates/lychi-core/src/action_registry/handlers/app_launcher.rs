use async_trait::async_trait;
use std::sync::Arc;
use std::time::Instant;

use redb::Database;

use crate::action_registry::handlers::icons::resolve_icon_cached;
use crate::action_registry::{ActionHandler, ActionResult, CompletionItem, ExecContext};
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

    /// Pre-warm the AppIndex and icon theme metadata. Fast (a few ms) — this is
    /// the part that must be ready before the first query. Icon *path* resolution
    /// is NOT done here: it's lazy per-result on query (see `completions`) and
    /// bulk-primed off the critical path by `warmup_icons_background`.
    pub fn warmup() {
        let t0 = Instant::now();
        super::icons::warmup_icons();
        let index = app_index();
        tracing::info!(
            "[app_launcher] warmup done: {:.0}ms ({} apps, icons resolved lazily)",
            t0.elapsed().as_secs_f64() * 1000.0,
            index.entries.len()
        );
    }

    /// Resolve every app's icon path into its per-entry cache. Idempotent and
    /// cheap to re-run (each `OnceLock` fills once). Meant to run on a dedicated
    /// low-priority thread AFTER the window is ready — so the icons the user
    /// scrolls to are already primed, without this ~seconds-long pass ever
    /// blocking the window or starving the first IPC (the old `spawn_blocking`
    /// eager loop did both). Lazy per-query resolution is the correctness
    /// guarantee; this is just an optimization to keep first hits warm.
    pub fn warmup_icons_background() {
        let t0 = Instant::now();
        let index = app_index();
        for entry in &index.entries {
            let _ = entry
                .icon_path
                .get_or_init(|| entry.icon.as_deref().and_then(resolve_icon_cached));
        }
        // Persist the resolved paths so the next launch starts warm (turns the
        // repeat-launch cost of this pass from seconds into a file read).
        super::icons::save_icon_cache();
        tracing::info!(
            "[app_launcher] background icon prewarm done: {:.0}ms ({} apps)",
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

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let query = args.trim();
        if query.is_empty() {
            return Ok(ActionResult::err(
                "Usage: open <application name>".to_string(),
            ));
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
            return Ok(ActionResult::empty_ok().with_duration(start.elapsed().as_millis() as u64));
        };

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
            Ok(ActionResult::focus_app(wm_class))
        } else {
            Ok(ActionResult::launch_desktop(entry.desktop_path.clone()))
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
                // Resolve this result's icon lazily, on demand. Only the handful
                // of results actually shown for a query pay the cost, and each
                // entry's OnceLock caches it forever after the first resolve — so
                // we don't eagerly resolve all ~160 apps' icons at startup (that
                // was ~6.5s of the warmup, for icons the user mostly never sees).
                let icon_path = entry
                    .icon_path
                    .get_or_init(|| entry.icon.as_deref().and_then(resolve_icon_cached))
                    .clone();
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
                    thumb_b64: None,
                    run: Some(format!("open {}", entry.name)),
                    ..Default::default()
                }
            })
            .collect();

        // Re-sort by blended score; deterministic last-resort tiebreak
        // (shorter label, then name) only when blended scores are equal —
        // a determinism guarantee, not a ranking opinion.
        items.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.label.len().cmp(&b.label.len()))
                .then_with(|| a.label.cmp(&b.label))
        });
        items.truncate(8);
        items
    }
}
