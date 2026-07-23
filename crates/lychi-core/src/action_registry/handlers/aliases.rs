use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use redb::Database;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::aliases::store::{AliasesStore, MAX_ALIASES};
use crate::error::LychiError;

pub struct AliasHandler {
    db: Arc<Database>,
}

impl AliasHandler {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
}

const ALIAS_SUBCOMMANDS: &[&str] = &["add", "list", "delete"];

#[async_trait]
impl ActionHandler for AliasHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["alias", "aliases"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "alias"
    }

    fn description(&self) -> &str {
        "Aliases — save command shortcuts. Usage: alias add <name> <command>, alias list, alias delete <name>"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let start = Instant::now();
        let trimmed = args.trim();
        let store = AliasesStore::new();

        // No args or "list" → list all aliases
        if trimmed.is_empty()
            || trimmed.eq_ignore_ascii_case("list")
            || trimmed.eq_ignore_ascii_case("ls")
        {
            let aliases = store.get_aliases(&self.db)?;
            if aliases.is_empty() {
                return Ok(ActionResult::ok(
                    "No aliases saved. Use: alias add <name> <command>".to_string(),
                    OutputType::Text,
                )
                .with_duration(start.elapsed().as_millis() as u64));
            }
            let lines: Vec<String> = aliases
                .iter()
                .map(|a| format!("  {} → {}", a.name, a.command))
                .collect();
            return Ok(ActionResult::ok(
                format!(
                    "Aliases ({}/{}):\n{}",
                    aliases.len(),
                    MAX_ALIASES,
                    lines.join("\n")
                ),
                OutputType::Text,
            )
            .with_duration(start.elapsed().as_millis() as u64));
        }

        // "add <name> <command>" → add alias
        if let Some(rest) = trimmed.strip_prefix("add ") {
            let rest = rest.trim();
            let (name, command) = rest
                .split_once(' ')
                .ok_or_else(|| LychiError::Alias("Usage: alias add <name> <command>".into()))?;
            let item = store.add_alias(&self.db, name, command)?;
            return Ok(ActionResult::ok(
                format!("Alias saved: {} → {}", item.name, item.command),
                OutputType::Status,
            )
            .with_duration(start.elapsed().as_millis() as u64));
        }

        // "delete <name>" / "del <name>" / "rm <name>" → delete alias
        if let Some(rest) = trimmed
            .strip_prefix("delete ")
            .or_else(|| trimmed.strip_prefix("del "))
            .or_else(|| trimmed.strip_prefix("rm "))
        {
            let name = rest.trim();
            store.delete_alias(&self.db, name)?;
            return Ok(
                ActionResult::ok(format!("Alias deleted: {name}"), OutputType::Status)
                    .with_duration(start.elapsed().as_millis() as u64),
            );
        }

        // "update <name> <command>" → update alias command
        if let Some(rest) = trimmed.strip_prefix("update ") {
            let rest = rest.trim();
            let (name, command) = rest
                .split_once(' ')
                .ok_or_else(|| LychiError::Alias("Usage: alias update <name> <command>".into()))?;
            store.update_alias(&self.db, name, command)?;
            return Ok(ActionResult::ok(
                format!("Alias updated: {name} → {command}"),
                OutputType::Status,
            )
            .with_duration(start.elapsed().as_millis() as u64));
        }

        // Unknown subcommand
        Ok(ActionResult::err(
            "Usage: alias add <name> <command> | alias list | alias delete <name>".to_string(),
        )
        .with_duration(start.elapsed().as_millis() as u64))
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let lower = partial.to_lowercase();

        // If empty or matches subcommands, show subcommands
        if lower.is_empty() || !lower.contains(' ') {
            let mut items: Vec<CompletionItem> = ALIAS_SUBCOMMANDS
                .iter()
                .filter(|s| lower.is_empty() || s.starts_with(&lower))
                .map(|s| CompletionItem {
                    label: s.to_string(),
                    icon_path: None,
                    score: 100,
                    description: None,
                    reason: None,
                    thumb_b64: None,
                    run: Some(format!("alias {s}")),
                    ..Default::default()
                })
                .collect();

            // Also show existing aliases as completions
            let store = AliasesStore::new();
            if let Ok(aliases) = store.get_aliases(&self.db) {
                for alias in &aliases {
                    if lower.is_empty() || alias.name.starts_with(&lower) {
                        items.push(CompletionItem {
                            label: alias.name.clone(),
                            icon_path: None,
                            score: 80,
                            description: Some(format!("→ {}", alias.command)),
                            reason: None,
                            thumb_b64: None,
                            // Selecting an alias runs it — the intent layer
                            // expands the alias name (first word) to its command.
                            run: Some(alias.name.clone()),
                            ..Default::default()
                        });
                    }
                }
            }

            return items;
        }

        // If typing "delete <partial>", show alias names for deletion
        if let Some(rest) = lower
            .strip_prefix("delete ")
            .or_else(|| lower.strip_prefix("del "))
            .or_else(|| lower.strip_prefix("rm "))
        {
            let rest = rest.trim();
            let store = AliasesStore::new();
            if let Ok(aliases) = store.get_aliases(&self.db) {
                return aliases
                    .iter()
                    .filter(|a| rest.is_empty() || a.name.contains(rest))
                    .map(|a| CompletionItem {
                        label: format!("delete {}", a.name),
                        icon_path: None,
                        score: 90,
                        description: Some(format!("→ {}", a.command)),
                        reason: None,
                        thumb_b64: None,
                        run: Some(format!("alias delete {}", a.name)),
                        ..Default::default()
                    })
                    .collect();
            }
        }

        Vec::new()
    }
}
