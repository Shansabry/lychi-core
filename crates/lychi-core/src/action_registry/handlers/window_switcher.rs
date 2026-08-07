use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use redb::Database;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::db::frecency;
use crate::error::LychiError;

use super::app_control::{self, RunningWindow};
use super::icons;
use crate::text::truncate_display;

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
        if let Some(ref eclass) = entry.wm_class
            && eclass.to_lowercase() == wmc_lower
        {
            return entry.name.clone();
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
        if matches && let Some(ref icon) = entry.icon {
            return icons::resolve_icon(icon);
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
        let short_title = truncate_display(title, 40);
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

/// The honest answer when no windows came back.
///
/// `WindowSupport` is the ONE decider (`context::capabilities`); this never
/// re-derives "unsupported" from an empty list, which is exactly the inference
/// that made the two cases indistinguishable.
fn unsupported_or_empty() -> String {
    use crate::context::capabilities::WindowSupport;
    match WindowSupport::detect() {
        WindowSupport::Available => "No open windows".to_string(),
        s @ WindowSupport::Unsupported => s.explain().to_string(),
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
        // The label may carry a truncation ellipsis; strip the one
        // `truncate_display` actually appends, never a hardcoded literal.
        let title_fragment = label[dash_pos + " — ".len()..]
            .trim_end_matches(crate::text::ELLIPSIS)
            .trim_end();
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
    fn category(&self) -> CommandCategory {
        CommandCategory::System
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
                // An empty list means two very different things, and saying
                // "No open windows" for both sent GNOME Wayland users hunting
                // a bug in Lychi. Ask the capability layer which one it is.
                return Ok(ActionResult::ok(unsupported_or_empty(), OutputType::Status));
            }
            let list: Vec<String> = windows
                .iter()
                .map(|w| {
                    let name = display_name_for_class(&w.wm_class);
                    format!("  {name} — {}", truncate_display(&w.title, 50))
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
                // Same distinction as the empty-list path above: on a
                // compositor with no window protocol, "no window matching X"
                // blames the query for a limitation of the session.
                use crate::context::capabilities::WindowSupport;
                let msg = match WindowSupport::detect() {
                    WindowSupport::Available => format!("No window matching '{query}'"),
                    s @ WindowSupport::Unsupported => s.explain().to_string(),
                };
                return Ok(ActionResult::err(msg));
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
                Some(d) => Some(format!("{d} · {}", truncate_display(&w.title, 40))),
                None => Some(truncate_display(&w.title, 50)),
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

#[cfg(test)]
mod truncate_regression {
    /// F1: window titles are arbitrary UTF-8 set by any application. The old
    /// `&s[..max_len - 3]` panicked mid-character on emoji/CJK titles.
    #[test]
    fn emoji_window_title_does_not_panic() {
        for title in [
            "🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵 Now Playing",
            "日本語のウィンドウタイトルです、とても長い",
            "café ☕ — editor",
        ] {
            for cap in [40, 50] {
                let out = crate::text::truncate_display(title, cap);
                assert!(out.chars().count() <= cap);
            }
        }
    }
}

#[cfg(test)]
mod label_roundtrip {
    use super::*;

    fn window(wm_class: &str, title: &str) -> RunningWindow {
        RunningWindow {
            window_id: Some(1),
            kwin_id: None,
            wm_class: wm_class.into(),
            title: title.into(),
            pid: 1,
            desktop: None,
        }
    }

    /// The label a completion shows must resolve back to the window it named.
    ///
    /// The display name comes from `display_name_for_class`, the same resolver
    /// `find_window_by_label` uses, rather than a hardcoded "Firefox". Naming
    /// a real app makes the assertion depend on that app being INSTALLED: on a
    /// CI runner with no .desktop files the index misses, the fallback
    /// capitalisation applies, and the round-trip fails for reasons that have
    /// nothing to do with truncation. Build host is not the target.
    #[test]
    fn a_truncated_label_still_finds_its_window() {
        let w = window(
            "firefox",
            "A very long window title that will certainly be truncated",
        );
        let display = display_name_for_class(&w.wm_class);
        let label = completion_label(&display, &w.title, true, false);
        assert!(
            find_window_by_label(std::slice::from_ref(&w), &label).is_some(),
            "label {label:?} did not resolve back to its window"
        );
    }

    /// The same round-trip for a title that forced a multibyte cut — the exact
    /// input class that used to panic before it ever got here.
    #[test]
    fn a_truncated_multibyte_label_still_finds_its_window() {
        for title in [
            "🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵 Now Playing Some Long Track",
            "日本語のウィンドウタイトルです、とても長いのでこれは必ず切られます",
        ] {
            let w = window("player", title);
            let display = display_name_for_class(&w.wm_class);
            let label = completion_label(&display, &w.title, true, false);
            assert!(
                find_window_by_label(std::slice::from_ref(&w), &label).is_some(),
                "label {label:?} did not resolve back to its window"
            );
        }
    }
}
