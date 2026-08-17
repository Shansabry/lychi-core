use async_trait::async_trait;

use crate::action_registry::grammar::{Grammar, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::clipboard::store::ClipboardStore;
use crate::db::frecency;
use crate::error::LychiError;
use crate::history::HistoryStore;

/// The things `clear` can wipe. One entry point, sub-targets — mirrors
/// `clip clear` but unified across every store.
const TARGETS: &[(&str, &str)] = &[
    ("history", "Clear command history"),
    ("clipboard", "Clear clipboard history"),
    ("suggestions", "Reset learned ranking (frecency)"),
    ("all", "Clear history, clipboard, and suggestions"),
];

/// `clear`'s argument surface: one verb per wipe target, no operands — the
/// flat form is just the target name, exactly what `execute` matches on. Every
/// verb mutates (each wipe erases stored data irreversibly); the Medium risk
/// level additionally makes the Rules Engine confirm each one. A drift test
/// pins the verb names to [`TARGETS`], which `execute`'s match accepts.
const CLEAR_GRAMMAR: Grammar = Grammar {
    verbs: &[
        Verb {
            name: "history",
            desc: "Erase the user's entire launcher command history. Irreversible; the \
                   launcher asks the user to confirm before running.",
            mutates: true,
            operands: &[],
        },
        Verb {
            name: "clipboard",
            desc: "Erase the launcher's stored clipboard history. Irreversible; the \
                   launcher asks the user to confirm before running.",
            mutates: true,
            operands: &[],
        },
        Verb {
            name: "suggestions",
            desc: "Reset the learned suggestion ranking (frecency scores), returning \
                   result ordering to defaults. Irreversible; the launcher asks the \
                   user to confirm before running.",
            mutates: true,
            operands: &[],
        },
        Verb {
            name: "all",
            desc: "Erase command history, clipboard history, AND learned ranking in one \
                   step. Irreversible; the launcher asks the user to confirm before \
                   running. Use only when the user clearly wants everything wiped.",
            mutates: true,
            operands: &[],
        },
    ],
};

#[derive(Default)]
pub struct ClearHandler;

impl ClearHandler {
    pub fn new() -> Self {
        Self
    }

    fn clear_history(&self) -> Result<(), LychiError> {
        // Constructor args (limit/dedup) don't affect clearing.
        HistoryStore::new(0, false).clear()
    }

    fn clear_clipboard(&self) -> Result<(), LychiError> {
        ClipboardStore::new().clear()
    }

    fn clear_suggestions(&self) -> Result<usize, LychiError> {
        frecency::clear()
    }

    fn ok(message: impl Into<String>) -> ActionResult {
        ActionResult::ok(message.into(), OutputType::Status)
    }
}

#[async_trait]
impl ActionHandler for ClearHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["clear"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "clear"
    }

    fn description(&self) -> &str {
        "Clear history, clipboard, or learned suggestions"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(CLEAR_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Utils
    }

    /// Every clear is irreversible, so the Rules Engine asks for confirmation
    /// before any of them run (Medium risk → Confirm).
    fn default_risk(&self) -> crate::action_registry::RiskLevel {
        crate::action_registry::RiskLevel::Medium
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        match args.trim().to_lowercase().as_str() {
            "history" => {
                self.clear_history()?;
                Ok(Self::ok("Command history cleared"))
            }
            "clipboard" | "clip" => {
                self.clear_clipboard()?;
                Ok(Self::ok("Clipboard history cleared"))
            }
            "suggestions" | "frecency" => {
                let n = self.clear_suggestions()?;
                Ok(Self::ok(format!("Reset learned ranking ({n} entries)")))
            }
            "all" => {
                self.clear_history()?;
                self.clear_clipboard()?;
                let n = self.clear_suggestions()?;
                Ok(Self::ok(format!(
                    "Cleared history, clipboard, and suggestions ({n} ranking entries)"
                )))
            }
            // Bare `clear` (or an unknown target) is a usage prompt, not an
            // action — the completions guide the user to a target.
            "" => Ok(ActionResult::err(
                "Usage: clear history | clipboard | suggestions | all".to_string(),
            )),
            other => Ok(ActionResult::err(format!(
                "Unknown clear target '{other}'. Try: history, clipboard, suggestions, all"
            ))),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let query = partial.trim().to_lowercase();
        TARGETS
            .iter()
            .filter(|(name, _)| query.is_empty() || name.starts_with(&query))
            .enumerate()
            .map(|(i, (name, desc))| {
                CompletionItem::new(
                    format!("clear {name}"),
                    Some("__none__".into()),
                    (100 - i) as u16,
                )
                .with_run(format!("clear {name}"))
                .with_description((*desc).to_string())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_test_database;

    #[tokio::test]
    async fn completions_offer_all_targets() {
        let h = ClearHandler::new();
        let items = h.completions("").await;
        assert_eq!(items.len(), TARGETS.len());
        // Every completion carries an exact `clear <target>` run command.
        for item in &items {
            let run = item.run.as_deref().unwrap();
            assert!(run.starts_with("clear "));
        }
    }

    #[tokio::test]
    async fn completions_filter_by_prefix() {
        let h = ClearHandler::new();
        let items = h.completions("sug").await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].run.as_deref(), Some("clear suggestions"));
    }

    #[tokio::test]
    async fn clear_suggestions_wipes_frecency() {
        let db = open_test_database();
        frecency::set_store_for_test(db.clone());
        frecency::record("history:foo").unwrap();
        assert!(!frecency::get_scores().is_empty());
        let h = ClearHandler::new();
        let result = h
            .execute(
                &crate::action_registry::ExecContext::default(),
                "suggestions",
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(frecency::get_scores().is_empty());
    }

    #[tokio::test]
    async fn bare_clear_is_usage_error() {
        let h = ClearHandler::new();
        let result = h
            .execute(&crate::action_registry::ExecContext::default(), "")
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[test]
    fn grammar_verbs_match_the_real_targets() {
        // Each grammar verb renders flat to just its name (no operands), and
        // every name must be a target `execute`'s match accepts — TARGETS is
        // that same list (it drives the completions off the same match arms).
        assert_eq!(CLEAR_GRAMMAR.verbs.len(), TARGETS.len());
        for verb in CLEAR_GRAMMAR.verbs {
            let flat = CLEAR_GRAMMAR.to_flat(verb, &serde_json::Map::new());
            assert_eq!(flat, verb.name);
            assert!(
                TARGETS.iter().any(|(name, _)| *name == verb.name),
                "grammar verb '{}' is not a target execute accepts",
                verb.name
            );
            // Every wipe is destructive — the grammar must say so.
            assert!(verb.mutates, "'{}' must be declared mutating", verb.name);
        }
    }

    #[test]
    fn grammar_flatten_resolves_each_action() {
        // The group-tool JSON shape resolves through the grammar to the flat
        // target string.
        assert_eq!(
            CLEAR_GRAMMAR.flatten_json(r#"{"action":"history"}"#),
            Some("history".to_string())
        );
        assert_eq!(
            CLEAR_GRAMMAR.flatten_json(r#"{"action":"all"}"#),
            Some("all".to_string())
        );
        // Flat/legacy callers pass through untouched (caller keeps raw).
        assert_eq!(CLEAR_GRAMMAR.flatten_json("history"), None);
    }

    #[test]
    fn clear_is_confirmed_before_running() {
        use crate::action_registry::RiskLevel;
        // Every clear is irreversible → Medium risk → Rules Engine confirms.
        let h = ClearHandler::new();
        assert_eq!(h.default_risk(), RiskLevel::Medium);
    }
}
