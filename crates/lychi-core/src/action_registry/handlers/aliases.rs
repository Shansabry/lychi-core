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

/// The alias verbs the tool schema constrains the model to — the same set
/// `execute`'s prefix checks dispatch on (canonical spellings only; the
/// parser's aliases like `ls`/`del`/`rm` stay accepted on the flat path).
const ALIAS_ACTION_VERBS: &[&str] = &["add", "update", "delete", "list"];

/// The JSON Schema for `alias`'s args: a required `action` (constrained to
/// [`ALIAS_ACTION_VERBS`]) plus the `name`/`command` operands the mutating
/// verbs need. Emitted as the tool's `input_schema` so the model is constrained
/// to a real verb.
fn alias_input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ALIAS_ACTION_VERBS,
                        "description": "What to do: \"add\" a new shortcut, \"update\" an existing one's command, \"delete\" one, or \"list\" all saved aliases." },
            "name": { "type": "string",
                      "description": "The alias name — the shortcut word the user will type (e.g. \"gs\"). One word, no spaces. Required for \"add\", \"update\" and \"delete\"; omit for \"list\"." },
            "command": { "type": "string",
                         "description": "The full command the alias expands to (e.g. \"git status\"). Required for \"add\" and \"update\"; omit otherwise." }
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

/// Normalize the tool's `args` to the flat `"<verb> <name> [<command>]"` string
/// the parser already understands. A constrained model sends the structured
/// JSON (`{"action":"add","name":"gs","command":"git status"}`); a human or
/// legacy/flat caller sends the string directly. Keeps `execute` on `&str`.
fn alias_args_to_flat(args: &str) -> String {
    let t = args.trim();
    if !t.starts_with('{') {
        return t.to_string();
    }
    match serde_json::from_str::<serde_json::Value>(t) {
        Ok(v) => {
            let field = |k: &str| v.get(k).and_then(|a| a.as_str()).unwrap_or("").trim();
            let action = field("action");
            let name = field("name");
            let command = field("command");
            [action, name, command]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" ")
        }
        // Not the JSON we expected — fall back to the raw string; the parser
        // answers with its usual usage message.
        Err(_) => t.to_string(),
    }
}

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
    fn usage(&self) -> &str {
        "'add <name> <command>', 'update <name> <command>', 'delete <name>', 'list'"
    }
    fn input_schema(&self) -> Option<serde_json::Value> {
        Some(alias_input_schema())
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let start = Instant::now();
        // A constrained model sends `{"action":..,"name":..,"command":..}`;
        // flatten it (and a plain-string caller passes through) to the form the
        // prefix checks below read.
        let flat = alias_args_to_flat(args);
        let trimmed = flat.trim();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; it flattens to the
        // `<verb> <name> [<command>]` string the prefix checks read. The
        // command may itself contain spaces — it is the trailing field.
        assert_eq!(
            alias_args_to_flat(r#"{"action":"add","name":"gs","command":"git status"}"#),
            "add gs git status"
        );
        assert_eq!(
            alias_args_to_flat(r#"{"action":"update","name":"gs","command":"git status -sb"}"#),
            "update gs git status -sb"
        );
        assert_eq!(
            alias_args_to_flat(r#"{"action":"delete","name":"gs"}"#),
            "delete gs"
        );
        assert_eq!(alias_args_to_flat(r#"{"action":"list"}"#), "list");
        // A verb missing its operands flattens to the bare verb, so the
        // parser's own usage error answers.
        assert_eq!(
            alias_args_to_flat(r#"{"action":"add","name":"gs"}"#),
            "add gs"
        );
        assert_eq!(alias_args_to_flat(r#"{"action":"delete"}"#), "delete");
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(alias_args_to_flat("add gs git status"), "add gs git status");
        assert_eq!(alias_args_to_flat("list"), "list");
        // Malformed JSON → raw fallback.
        assert_eq!(alias_args_to_flat("{not json"), "{not json");
    }

    #[test]
    fn alias_schema_enum_matches_the_real_verbs() {
        // The schema's action enum must be exactly ALIAS_ACTION_VERBS, so the
        // model is constrained to verbs the parser actually handles.
        let schema = alias_input_schema();
        let en = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert_eq!(en.len(), ALIAS_ACTION_VERBS.len());
        for v in ALIAS_ACTION_VERBS {
            assert!(en.iter().any(|e| e == v), "enum missing {v}");
        }
    }
}
