use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use redb::Database;

use crate::action_registry::{
    ActionHandler, ActionResult, CompletionItem, ExecContext, OutputType,
};
use crate::db::frecency;
use crate::error::LychiError;

use super::app_control::{self, RunningWindow};
use super::icons;

pub struct WindowSwitcherHandler {
    db: Arc<Database>,
}

impl WindowSwitcherHandler {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
}

/// Look up a display name for a wm_class by checking the desktop app index.
fn display_name_for_class(wm_class: &str) -> String {
    let index = crate::desktop_apps::index::app_index();
    let wmc_lower = wm_class.to_lowercase();
    for entry in &index.entries {
        if let Some(ref eclass) = entry.wm_class {
            if eclass.to_lowercase() == wmc_lower {
                return entry.name.clone();
            }
        }
        if entry.exec_basename.to_lowercase() == wmc_lower {
            return entry.name.clone();
        }
    }
    // Capitalize first letter as fallback
    let mut chars = wm_class.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
        None => wm_class.to_string(),
    }
}

/// Resolve icon path for a wm_class from the desktop app index.
fn icon_for_class(wm_class: &str) -> Option<String> {
    let index = crate::desktop_apps::index::app_index();
    let wmc_lower = wm_class.to_lowercase();
    for entry in &index.entries {
        let matches = entry
            .wm_class
            .as_ref()
            .is_some_and(|c| c.to_lowercase() == wmc_lower)
            || entry.exec_basename.to_lowercase() == wmc_lower;
        if matches {
            if let Some(ref icon) = entry.icon {
                return icons::resolve_icon(icon);
            }
        }
    }
    None
}

/// Format desktop number for display.
fn desktop_label(desktop: Option<u32>, is_kwin: bool) -> Option<String> {
    desktop.map(|d| {
        // KWin: 1-indexed, X11: 0-indexed
        let num = if is_kwin { d } else { d + 1 };
        format!("Desktop {num}")
    })
}

/// Count how many windows share each wm_class.
fn class_counts(windows: &[RunningWindow]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for w in windows {
        *counts.entry(w.wm_class.clone()).or_insert(0) += 1;
    }
    counts
}

/// Build a completion label. If multiple windows share the same app, disambiguate with title.
fn completion_label(display_name: &str, title: &str, is_multi: bool, is_close: bool) -> String {
    let base = if is_multi {
        let short_title = truncate(title, 40);
        format!("{display_name} — {short_title}")
    } else {
        display_name.to_string()
    };
    if is_close {
        format!("close {base}")
    } else {
        base
    }
}

/// Find the exact window that matches a completion label.
/// When the label contains " — ", match by class + title substring.
fn find_window_by_label<'a>(
    windows: &'a [RunningWindow],
    label: &str,
) -> Option<&'a RunningWindow> {
    // Try "DisplayName — Title..." format first
    if let Some(dash_pos) = label.find(" — ") {
        let title_fragment = label[dash_pos + " — ".len()..].trim_end_matches("...");
        return windows.iter().find(|w| {
            let name = display_name_for_class(&w.wm_class);
            label.starts_with(&name) && w.title.starts_with(title_fragment)
        });
    }
    // Plain display name — find by class match
    let label_lower = label.to_lowercase();
    windows.iter().find(|w| {
        let name = display_name_for_class(&w.wm_class);
        name.to_lowercase() == label_lower || w.wm_class == label_lower
    })
}

#[async_trait]
impl ActionHandler for WindowSwitcherHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["win", "window", "windows"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "win"
    }

    fn description(&self) -> &str {
        "Switch between open windows (focus or close)"
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let args = args.trim();

        // Check for "close <query>" mode
        let (close_mode, query) = if let Some(rest) = args.strip_prefix("close ") {
            (true, rest.trim())
        } else if args == "close" {
            return Ok(ActionResult::err("Usage: win close <app name>"));
        } else {
            (false, args)
        };

        if query.is_empty() {
            let windows = app_control::get_windows();
            if windows.is_empty() {
                return Ok(ActionResult::ok("No open windows", OutputType::Status));
            }
            let list: Vec<String> = windows
                .iter()
                .map(|w| {
                    let name = display_name_for_class(&w.wm_class);
                    format!("  {name} — {}", truncate(&w.title, 50))
                })
                .collect();
            return Ok(ActionResult::ok(
                format!("Open windows:\n{}", list.join("\n")),
                OutputType::Text,
            ));
        }

        let windows = app_control::get_windows();

        // Try per-window label match first (from completion selection),
        // then fall back to fuzzy find_window
        let window = find_window_by_label(&windows, query)
            .or_else(|| app_control::find_window(&windows, query));

        let window = match window {
            Some(w) => w,
            None => {
                return Ok(ActionResult::err(format!("No window matching '{query}'")));
            }
        };

        if close_mode {
            match app_control::do_close(window) {
                Ok(()) => Ok(ActionResult::ok(
                    format!("Closed: {}", window.title),
                    OutputType::Status,
                )),
                Err(e) => Ok(ActionResult::err(format!(
                    "Failed to close '{}': {e}",
                    window.title
                ))),
            }
        } else {
            // Record frecency for focus (keyed by class — titles are volatile)
            let _ = frecency::record(&self.db, &format!("win:{}", window.wm_class));

            match app_control::do_focus(window) {
                Ok(()) => Ok(ActionResult::ok(
                    format!("Focused: {}", window.title),
                    OutputType::Status,
                )),
                Err(e) => Ok(ActionResult::err(format!(
                    "Failed to focus '{}': {e}",
                    window.title
                ))),
            }
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let windows = app_control::get_windows();
        let partial = partial.trim();
        let is_kwin = crate::context::is_kde_wayland_session();

        // Strip "close " prefix for completion matching
        let (is_close, query) = if let Some(rest) = partial.strip_prefix("close ") {
            (true, rest.trim())
        } else if partial == "close" {
            (true, "")
        } else {
            (false, partial)
        };

        // Load frecency scores for "win:" prefix keys
        let frecency_scores: HashMap<String, f64> = frecency::get_scores(&self.db)
            .into_iter()
            .filter_map(|(key, score)| key.strip_prefix("win:").map(|k| (k.to_string(), score)))
            .collect();

        let counts = class_counts(&windows);

        // Pre-compute display names and icons per class (avoid repeated lookups)
        let mut class_names: HashMap<String, String> = HashMap::new();
        let mut class_icons: HashMap<String, Option<String>> = HashMap::new();
        for w in &windows {
            class_names
                .entry(w.wm_class.clone())
                .or_insert_with(|| display_name_for_class(&w.wm_class));
            class_icons
                .entry(w.wm_class.clone())
                .or_insert_with(|| icon_for_class(&w.wm_class));
        }

        let build_item = |w: &RunningWindow, score: f64| -> CompletionItem {
            let display_name = class_names.get(&w.wm_class).unwrap();
            let is_multi = counts.get(&w.wm_class).copied().unwrap_or(1) > 1;
            let icon_path = class_icons.get(&w.wm_class).unwrap().clone();
            let label = completion_label(display_name, &w.title, is_multi, is_close);

            let desk = desktop_label(w.desktop, is_kwin);
            let description = match desk {
                Some(d) => Some(format!("{d} · {}", truncate(&w.title, 40))),
                None => Some(truncate(&w.title, 50)),
            };

            let run = format!("win {label}");
            CompletionItem {
                label,
                icon_path,
                score: score.min(999.0) as u16,
                description,
                reason: None,
                thumb_b64: None,
                run: Some(run),
                ..Default::default()
            }
        };

        if query.is_empty() {
            // Show all windows, grouped by app, boosted by frecency
            // Group windows by class, sort groups by frecency, windows within by title
            let mut groups: HashMap<String, Vec<&RunningWindow>> = HashMap::new();
            for w in &windows {
                groups.entry(w.wm_class.clone()).or_default().push(w);
            }

            // Sort groups by frecency score (descending)
            let mut group_order: Vec<(String, f64)> = groups
                .keys()
                .map(|class| {
                    let freq = frecency_scores.get(class).copied().unwrap_or(0.0);
                    (class.clone(), freq)
                })
                .collect();
            group_order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            let mut items = Vec::new();
            let mut rank: f64 = 900.0;
            for (class, freq) in &group_order {
                let mut group_windows: Vec<&&RunningWindow> = groups[class].iter().collect();
                group_windows.sort_by(|a, b| a.title.cmp(&b.title));
                for w in group_windows {
                    let score = rank + freq * 100.0;
                    items.push(build_item(w, score));
                    rank -= 1.0;
                }
            }

            items.truncate(20);
            return items;
        }

        // Fuzzy match using nucleo
        let mut matcher = Matcher::new(Config::DEFAULT);
        let pattern = Pattern::new(
            query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );

        let mut scored: Vec<(f64, CompletionItem)> = windows
            .iter()
            .filter_map(|w| {
                let display_name = class_names.get(&w.wm_class).unwrap();
                let haystack = format!("{} {} {}", display_name, w.wm_class, w.title);
                let haystack_chars: Vec<char> = haystack.chars().collect();
                let utf32 = nucleo_matcher::Utf32Str::Unicode(&haystack_chars);
                let nucleo_score = pattern.score(utf32, &mut matcher)?;

                let freq_boost = frecency_scores.get(&w.wm_class).copied().unwrap_or(0.0);
                let combined = nucleo_score as f64 + freq_boost * 500.0;

                Some((combined, build_item(w, combined)))
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().map(|(_, item)| item).take(20).collect()
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_prefix_parsing() {
        let input = "close firefox";
        let (is_close, query) = if let Some(rest) = input.strip_prefix("close ") {
            (true, rest.trim())
        } else {
            (false, input)
        };
        assert!(is_close);
        assert_eq!(query, "firefox");
    }

    #[test]
    fn focus_is_default() {
        let input = "firefox";
        let (is_close, query) = if let Some(rest) = input.strip_prefix("close ") {
            (true, rest.trim())
        } else {
            (false, input)
        };
        assert!(!is_close);
        assert_eq!(query, "firefox");
    }

    #[test]
    fn display_name_capitalization() {
        // Fallback capitalization when app not in index
        let mut chars = "firefox".chars();
        let result = match chars.next() {
            Some(c) => c.to_uppercase().to_string() + chars.as_str(),
            None => String::new(),
        };
        assert_eq!(result, "Firefox");
    }

    #[test]
    fn completion_label_single_window() {
        let label = completion_label("Firefox", "GitHub - Mozilla Firefox", false, false);
        assert_eq!(label, "Firefox");
    }

    #[test]
    fn completion_label_multi_window() {
        let label = completion_label("Konsole", "sab@arch: ~/lychi", true, false);
        assert_eq!(label, "Konsole — sab@arch: ~/lychi");
    }

    #[test]
    fn completion_label_close_mode() {
        let label = completion_label("Firefox", "GitHub", false, true);
        assert_eq!(label, "close Firefox");
    }

    #[test]
    fn desktop_label_kwin() {
        assert_eq!(desktop_label(Some(1), true), Some("Desktop 1".to_string()));
        assert_eq!(desktop_label(Some(2), true), Some("Desktop 2".to_string()));
        assert_eq!(desktop_label(None, true), None);
    }

    #[test]
    fn desktop_label_x11() {
        // X11 desktops are 0-indexed, display as 1-indexed
        assert_eq!(desktop_label(Some(0), false), Some("Desktop 1".to_string()));
        assert_eq!(desktop_label(Some(1), false), Some("Desktop 2".to_string()));
    }

    #[test]
    fn class_counts_groups() {
        let windows = vec![
            RunningWindow {
                window_id: None,
                kwin_id: None,
                title: "a".into(),
                wm_class: "firefox".into(),
                pid: 1,
                desktop: None,
            },
            RunningWindow {
                window_id: None,
                kwin_id: None,
                title: "b".into(),
                wm_class: "firefox".into(),
                pid: 2,
                desktop: None,
            },
            RunningWindow {
                window_id: None,
                kwin_id: None,
                title: "c".into(),
                wm_class: "konsole".into(),
                pid: 3,
                desktop: None,
            },
        ];
        let counts = class_counts(&windows);
        assert_eq!(counts["firefox"], 2);
        assert_eq!(counts["konsole"], 1);
    }
}
