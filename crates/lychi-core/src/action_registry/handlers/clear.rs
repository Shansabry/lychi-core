use std::sync::Arc;

use async_trait::async_trait;
use redb::Database;

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

pub struct ClearHandler {
    db: Arc<Database>,
}

impl ClearHandler {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    fn clear_history(&self) -> Result<(), LychiError> {
        // Constructor args (limit/dedup) don't affect clearing.
        HistoryStore::new(0, false).clear()
    }

    fn clear_clipboard(&self) -> Result<(), LychiError> {
        ClipboardStore::new().clear(&self.db)
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
        let h = ClearHandler::new(open_test_database());
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
        let h = ClearHandler::new(open_test_database());
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
        let h = ClearHandler::new(db.clone());
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
        let h = ClearHandler::new(open_test_database());
        let result = h
            .execute(&crate::action_registry::ExecContext::default(), "")
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[test]
    fn clear_is_confirmed_before_running() {
        use crate::action_registry::RiskLevel;
        // Every clear is irreversible → Medium risk → Rules Engine confirms.
        let h = ClearHandler::new(open_test_database());
        assert_eq!(h.default_risk(), RiskLevel::Medium);
    }
}
