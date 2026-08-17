use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use redb::Database;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
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

/// The alias name operand, shared by every verb that targets one alias.
const ALIAS_NAME: Operand = Operand {
    name: "name",
    desc: "The alias name — the shortcut word the user will type (e.g. \"gs\"). \
           One word, no spaces.",
    required: true,
    kind: ArgKind::Text,
    prefix: None,
};

/// The expansion operand `add`/`update` need. Free text with spaces — it is the
/// trailing field of the flat form, so it survives whole.
const ALIAS_COMMAND: Operand = Operand {
    name: "command",
    desc: "The full command the alias expands to (e.g. \"git status\"). May \
           contain spaces.",
    required: true,
    kind: ArgKind::Text,
    prefix: None,
};

/// `alias`'s argument surface: the verbs `execute`'s prefix checks dispatch on
/// (canonical spellings only; the parser's aliases like `ls`/`del`/`rm` stay
/// accepted on the flat path). The JSON Schema and the structured→flat adapter
/// both derive from this.
const ALIAS_GRAMMAR: Grammar = Grammar {
    verbs: &[
        Verb {
            name: "add",
            desc: "Save a new command shortcut: typing the alias name in the \
                   launcher expands to the full command.",
            mutates: true,
            operands: &[ALIAS_NAME, ALIAS_COMMAND],
        },
        Verb {
            name: "update",
            desc: "Change what an existing alias expands to.",
            mutates: true,
            operands: &[ALIAS_NAME, ALIAS_COMMAND],
        },
        Verb {
            name: "delete",
            desc: "Delete a saved alias by name.",
            mutates: true,
            operands: &[ALIAS_NAME],
        },
        Verb {
            name: "list",
            desc: "List every saved alias and the command it expands to.",
            mutates: false,
            operands: &[],
        },
    ],
};

/// Normalize the tool's `args` to the flat `"<verb> <name> [<command>]"` string
/// the parser already understands. A constrained model sends the structured
/// JSON (`{"action":"add","name":"gs","command":"git status"}`); a human or
/// legacy/flat caller sends the string directly, and malformed JSON falls back
/// to the raw string (the parser answers with its usual usage message). Keeps
/// `execute` on `&str`.
fn alias_args_to_flat(args: &str) -> String {
    ALIAS_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
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
    fn grammar(&self) -> Option<Grammar> {
        Some(ALIAS_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Personal
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
        // Malformed JSON (and JSON naming a verb the grammar lacks) → raw
        // fallback; the parser answers with its usual usage message.
        assert_eq!(alias_args_to_flat("{not json"), "{not json");
        assert_eq!(
            alias_args_to_flat(r#"{"action":"frobnicate","name":"gs"}"#),
            r#"{"action":"frobnicate","name":"gs"}"#
        );
    }

    #[test]
    fn alias_schema_enum_matches_the_grammar_verbs() {
        // The derived schema's action enum must be exactly the grammar's verbs
        // — and those must stay the set `execute`'s prefix checks dispatch on.
        let names: Vec<&str> = ALIAS_GRAMMAR.verbs.iter().map(|v| v.name).collect();
        assert_eq!(names, vec!["add", "update", "delete", "list"]);
        let schema = ALIAS_GRAMMAR.handler_schema();
        let en = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert_eq!(en.len(), names.len());
        for v in &names {
            assert!(en.iter().any(|e| e == v), "enum missing {v}");
        }
    }

    /// Extract the text body from a result's output, for assertions.
    fn body(r: &ActionResult) -> Option<&str> {
        match &r.output {
            crate::action_registry::Output::Text { body, .. } => Some(body.as_str()),
            _ => None,
        }
    }

    /// Drift guard: every verb's flat rendering (via the grammar) must be
    /// accepted by the hand-written parser — end to end through `execute`.
    #[tokio::test]
    async fn grammar_flat_rendering_is_accepted_by_the_parser() {
        let db = crate::db::open_test_database();
        let handler = AliasHandler::new(db);
        let ctx = crate::action_registry::ExecContext::default();

        let r = handler
            .execute(
                &ctx,
                r#"{"action":"add","name":"gs","command":"git status"}"#,
            )
            .await
            .unwrap();
        assert!(r.success, "{:?}", body(&r));
        assert!(body(&r).unwrap().contains("gs → git status"));

        let r = handler
            .execute(
                &ctx,
                r#"{"action":"update","name":"gs","command":"git status -sb"}"#,
            )
            .await
            .unwrap();
        assert!(r.success);
        assert!(body(&r).unwrap().contains("gs → git status -sb"));

        let r = handler.execute(&ctx, r#"{"action":"list"}"#).await.unwrap();
        assert!(r.success);
        assert!(body(&r).unwrap().contains("gs → git status -sb"));

        let r = handler
            .execute(&ctx, r#"{"action":"delete","name":"gs"}"#)
            .await
            .unwrap();
        assert!(r.success);
        assert!(body(&r).unwrap().contains("Alias deleted: gs"));
    }
}
