//! Suggestion-learning state (CTR latch + impression debounce), extracted from
//! the Executor so the orchestrator doesn't carry the two ad-hoc mutexes and
//! their bookkeeping inline. The Executor still owns the policy (what to record,
//! read from `context`/`db`); this collaborator owns only the small mutable state
//! those decisions latch against.

use std::sync::Mutex;

/// Tracks the last speculative suggestions shown (for acceptance detection) and
/// the last impression panel (for debounced recording). Both are process-local,
/// read-mostly latches.
#[derive(Default)]
pub struct SuggestionTracker {
    /// Labels of the context (`__context__`) suggestions shown in the most recent
    /// completions pass. Executing one of these counts as acceptance.
    last_suggestions: Mutex<Vec<String>>,
    /// (context_key, commands, ts_ms) of the last zero-state panel counted, so the
    /// same panel settling over successive keystrokes is only recorded once.
    last_impression: Mutex<Option<(String, Vec<String>, u64)>>,
}

impl SuggestionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the remembered set of shown context-suggestion labels.
    pub fn set_shown(&self, labels: Vec<String>) {
        if let Ok(mut guard) = self.last_suggestions.lock() {
            *guard = labels;
        }
    }

    /// Whether `candidate` was among the most recently shown context suggestions
    /// (i.e. the user accepted a suggestion rather than typing something new).
    pub fn was_shown(&self, candidate: &str) -> bool {
        self.last_suggestions
            .lock()
            .map(|g| g.iter().any(|s| s == candidate))
            .unwrap_or(false)
    }

    /// Debounced impression latch: returns `true` if this (key, commands) panel
    /// should be recorded now, `false` if the same panel is still settling within
    /// `debounce_ms`. On `true`, the latch is updated to this panel + `now`.
    pub fn should_record_impression(
        &self,
        key: &str,
        commands: &[String],
        now: u64,
        debounce_ms: u64,
    ) -> bool {
        if let Ok(mut guard) = self.last_impression.lock() {
            if let Some((prev_key, prev_cmds, prev_ts)) = guard.as_ref()
                && prev_key == key
                && prev_cmds == commands
                && now.saturating_sub(*prev_ts) < debounce_ms
            {
                return false; // same panel still settling — already counted
            }
            *guard = Some((key.to_string(), commands.to_vec(), now));
            true
        } else {
            // Lock poisoned — fail open (record). Impression over-counting is
            // harmless relative to losing the signal entirely.
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn was_shown_matches_only_recorded_labels() {
        let t = SuggestionTracker::new();
        t.set_shown(vec!["open spotify".into(), "web cats".into()]);
        assert!(t.was_shown("open spotify"));
        assert!(!t.was_shown("open firefox"));
    }

    #[test]
    fn set_shown_replaces_previous() {
        let t = SuggestionTracker::new();
        t.set_shown(vec!["a".into()]);
        t.set_shown(vec!["b".into()]);
        assert!(!t.was_shown("a"));
        assert!(t.was_shown("b"));
    }

    #[test]
    fn impression_debounce_suppresses_same_panel_within_window() {
        let t = SuggestionTracker::new();
        let cmds = vec!["x".to_string()];
        // First settle records.
        assert!(t.should_record_impression("ctx", &cmds, 1000, 750));
        // Same panel 200ms later — suppressed.
        assert!(!t.should_record_impression("ctx", &cmds, 1200, 750));
        // Same panel after the window — records again.
        assert!(t.should_record_impression("ctx", &cmds, 2000, 750));
    }

    #[test]
    fn impression_records_when_context_or_commands_change() {
        let t = SuggestionTracker::new();
        assert!(t.should_record_impression("ctx-a", &["x".into()], 1000, 750));
        // Different context within window — records.
        assert!(t.should_record_impression("ctx-b", &["x".into()], 1100, 750));
        // Different commands within window — records.
        assert!(t.should_record_impression("ctx-b", &["y".into()], 1150, 750));
    }
}
