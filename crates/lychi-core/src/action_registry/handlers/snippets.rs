use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use redb::Database;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, Output, OutputType,
    RiskLevel, Row, Section,
};
use crate::error::LychiError;
use crate::snippets::store::SnippetsStore;

use super::clipboard::write_to_clipboard;

/// The snippet-name operand every targeting verb shares. ONE description for
/// all of them: the schema merger keeps the first operand's prose per field
/// name, so a per-verb variant would be silently dropped.
const SNIP_NAME: Operand = Operand {
    name: "name",
    desc: "The snippet's name. For `add`, ONE word with no spaces (the body \
           starts at the first space); elsewhere, the name exactly as saved.",
    required: true,
    kind: ArgKind::Text,
    prefix: None,
};

/// The body operand `add`/`edit` need. Free text with spaces — it is the
/// trailing field of the flat form, so it survives whole.
const SNIP_BODY: Operand = Operand {
    name: "body",
    desc: "The snippet's text content. May contain spaces.",
    required: true,
    kind: ArgKind::Text,
    prefix: None,
};

/// `snip`'s argument surface. Historically the flat `copy` form was the BARE
/// name (`snip <name>`), which `to_flat`'s verb-prefix rule cannot render from
/// a multi-verb grammar — so the parser learned the explicit `copy <name>`
/// verb (see `execute`), the bare-name fallthrough staying for humans. The
/// JSON Schema and the structured→flat adapter both derive from this.
const SNIP_GRAMMAR: Grammar = Grammar {
    verbs: &[
        Verb {
            name: "copy",
            desc: "Copy a saved snippet's body to the system clipboard, \
                   looked up by name. Overwrites the current clipboard \
                   contents.",
            mutates: true,
            operands: &[SNIP_NAME],
        },
        Verb {
            name: "add",
            desc: "Save a new snippet under a name.",
            mutates: true,
            operands: &[SNIP_NAME, SNIP_BODY],
        },
        Verb {
            name: "edit",
            desc: "Replace an existing snippet's body, looked up by name.",
            mutates: true,
            operands: &[SNIP_NAME, SNIP_BODY],
        },
        Verb {
            name: "delete",
            desc: "Delete a snippet by name (or by id).",
            mutates: true,
            operands: &[SNIP_NAME],
        },
        Verb {
            name: "list",
            desc: "List all saved snippets with a preview of each body.",
            mutates: false,
            operands: &[],
        },
    ],
};

/// Normalize the tool's `args` to the flat `"<verb> …"` string the parser
/// understands. A constrained model sends the structured JSON
/// (`{"action":"copy","name":"email-intro"}`); a human or legacy/flat caller
/// sends the string directly (a bare name still copies via the fallthrough),
/// and malformed JSON falls back to the raw string. Keeps `execute` on `&str`.
fn snip_args_to_flat(args: &str) -> String {
    SNIP_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
}

const SNIP_SUBCOMMANDS: &[(&str, &str)] = &[
    ("add", "Add a snippet (e.g. snip add email-intro Hello...)"),
    ("list", "List all saved snippets"),
    ("delete", "Delete a snippet by name or ID"),
    (
        "edit",
        "Edit a snippet (e.g. snip edit email-intro New body...)",
    ),
];

/// Resolve a snippet row action into the command it stands for.
///
/// Unlike `ssh::resolve_action` this cannot allowlist characters: snippet names
/// are user-authored free text and legitimately contain spaces, punctuation and
/// non-ASCII. Nothing here reaches a shell either — every snippet verb is a
/// store lookup by name or id.
///
/// The real hazard is argument splitting. `snip delete my note` parses as verb
/// `delete` + rest `my note`, which happens to work, but a name whose leading
/// word collides with a verb (a snippet literally called "delete foo") would
/// re-enter the parser as a different command. Names containing a newline are
/// worse: `rest` is matched verbatim, so the tail would be silently ignored.
///
/// So: reject control characters (a name can never legitimately contain one),
/// and pass everything else through unchanged.
pub fn resolve_action(id: &str, target: &str) -> Result<String, String> {
    if !matches!(id, "copy" | "delete") {
        return Err(format!("Unknown snippet action '{id}'"));
    }
    if target.trim().is_empty() {
        return Err("Snippet name is empty".to_string());
    }
    if target.chars().any(char::is_control) {
        return Err("Snippet name contains a control character".to_string());
    }
    // `copy` is the bare form — `snip <name>` copies to the clipboard.
    Ok(match id {
        "copy" => format!("snip {target}"),
        _ => format!("snip delete {target}"),
    })
}

pub struct SnippetsHandler {
    db: Arc<Database>,
}

impl SnippetsHandler {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Copy a snippet's body to the clipboard, looked up by `name` — the one
    /// copy path, shared by the explicit `copy` verb and the bare-name
    /// fallthrough.
    fn copy_snippet(
        &self,
        store: &SnippetsStore,
        name: &str,
        start: Instant,
    ) -> Result<ActionResult, LychiError> {
        if let Some(item) = store.get_snippet_by_name(&self.db, name)? {
            match write_to_clipboard(&item.body) {
                Ok(()) => Ok(ActionResult::ok(
                    format!("Copied: {} ({} chars)", item.name, item.body.len()),
                    OutputType::Status,
                )
                .with_duration(start.elapsed().as_millis() as u64)),
                Err(e) => Ok(ActionResult::err(format!("Clipboard error: {e}"))
                    .with_duration(start.elapsed().as_millis() as u64)),
            }
        } else {
            Ok(ActionResult::err(format!("Snippet not found: {name}"))
                .with_duration(start.elapsed().as_millis() as u64))
        }
    }

    /// First line of a snippet body, truncated to ~`max` bytes on a char
    /// boundary (a naive `[..max]` panics mid-UTF-8-char on emoji/CJK).
    fn truncate_body(body: &str, max: usize) -> &str {
        let first_line = body.lines().next().unwrap_or(body);
        if first_line.len() > max {
            let mut end = max;
            while end > 0 && !first_line.is_char_boundary(end) {
                end -= 1;
            }
            &first_line[..end]
        } else {
            first_line
        }
    }
}

#[async_trait]
impl ActionHandler for SnippetsHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["snip", "snippet", "snippets"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "snip"
    }

    fn description(&self) -> &str {
        "Snippets — save and paste text blocks. Usage: snip <name> to paste, snip add <name> <body>, snip list, snip delete <name>, snip edit <name> <body>"
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(SNIP_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Personal
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let start = Instant::now();
        // A constrained model sends `{"action":..,..}`; flatten it (and a
        // plain-string caller passes through) to the form the parser reads.
        let flat = snip_args_to_flat(args);
        let text = flat.trim();
        let store = SnippetsStore::new();

        // No args → open snippets panel
        if text.is_empty() {
            return Ok(
                ActionResult::ok("__snippets_panel__".to_string(), OutputType::Status)
                    .with_duration(start.elapsed().as_millis() as u64),
            );
        }

        let (cmd, rest) = text.split_once(' ').unwrap_or((text, ""));
        let rest = rest.trim();

        match cmd.to_lowercase().as_str() {
            "add" => {
                // snip add <name> <body>
                let (name, body) = rest.split_once(' ').unwrap_or((rest, ""));
                let name = name.trim();
                let body = body.trim();

                if name.is_empty() || body.is_empty() {
                    return Ok(
                        ActionResult::err("Usage: snip add <name> <body>".to_string())
                            .with_duration(start.elapsed().as_millis() as u64),
                    );
                }

                let item = store.add_snippet(&self.db, name, body)?;
                Ok(ActionResult::ok(
                    format!("Snippet saved: {} ({} chars)", item.name, item.body.len()),
                    OutputType::Status,
                )
                .with_duration(start.elapsed().as_millis() as u64))
            }
            "list" | "ls" => {
                let snippets = store.get_snippets(&self.db)?;
                if snippets.is_empty() {
                    return Ok(
                        ActionResult::ok("No snippets saved".to_string(), OutputType::Text)
                            .with_duration(start.elapsed().as_millis() as u64),
                    );
                }
                // Rows rather than a joined string. `updated_at` was being
                // dropped entirely by the text form — the frontend renders it
                // as a relative age, so "edited 2d ago" comes for free and
                // reads identically to every other aged list.
                let rows: Vec<Row> = snippets
                    .iter()
                    .map(|s| {
                        Row::new(&s.name)
                            .subtitle(Self::truncate_body(&s.body, 50))
                            .accessory_at(s.updated_at as i64)
                            .action("copy", "Copy", &s.name, None)
                            .action("delete", "Delete", &s.name, Some(RiskLevel::Medium))
                    })
                    .collect();
                Ok(ActionResult {
                    success: true,
                    output: Output::Rows {
                        sections: vec![Section {
                            title: Some(format!("Snippets ({})", rows.len())),
                            rows,
                            handler: "snippets".to_string(),
                        }],
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                    ..Default::default()
                })
            }
            "delete" | "del" | "rm" | "remove" => {
                if rest.is_empty() {
                    return Ok(
                        ActionResult::err("Usage: snip delete <name or id>".to_string())
                            .with_duration(start.elapsed().as_millis() as u64),
                    );
                }

                // Try by name first, then by ID
                if let Some(item) = store.get_snippet_by_name(&self.db, rest)? {
                    store.delete_snippet(&self.db, &item.id)?;
                    return Ok(ActionResult::ok(
                        format!("Snippet deleted: {}", item.name),
                        OutputType::Status,
                    )
                    .with_duration(start.elapsed().as_millis() as u64));
                }

                // Try as ID directly
                store.delete_snippet(&self.db, rest)?;
                Ok(
                    ActionResult::ok(format!("Snippet deleted: {rest}"), OutputType::Status)
                        .with_duration(start.elapsed().as_millis() as u64),
                )
            }
            "edit" | "update" => {
                // snip edit <name> <new-body>
                let (name, body) = rest.split_once(' ').unwrap_or((rest, ""));
                let name = name.trim();
                let body = body.trim();

                if name.is_empty() || body.is_empty() {
                    return Ok(
                        ActionResult::err("Usage: snip edit <name> <new body>".to_string())
                            .with_duration(start.elapsed().as_millis() as u64),
                    );
                }

                let item = store
                    .get_snippet_by_name(&self.db, name)?
                    .ok_or_else(|| LychiError::Snippet(format!("Snippet not found: {name}")))?;

                store.update_snippet(&self.db, &item.id, &item.name, body)?;
                Ok(ActionResult::ok(
                    format!("Snippet updated: {} ({} chars)", item.name, body.len()),
                    OutputType::Status,
                )
                .with_duration(start.elapsed().as_millis() as u64))
            }
            // Explicit `copy <name>` verb — the grammar's flat rendering (a
            // multi-verb grammar must render a verb; the bare name can't carry
            // one). The bare-name fallthrough below stays for humans; the one
            // behavior change is that a snippet whose name STARTS with the
            // word "copy" must now be pasted from the panel — the verb wins.
            "copy" => {
                if rest.is_empty() {
                    return Ok(ActionResult::err("Usage: snip copy <name>".to_string())
                        .with_duration(start.elapsed().as_millis() as u64));
                }
                self.copy_snippet(&store, rest, start)
            }
            // Default: treat the entire args as a snippet name and copy it.
            _ => self.copy_snippet(&store, text, start),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let lower = partial.to_lowercase();

        // Show subcommands when no input or matching a subcommand
        let mut items: Vec<CompletionItem> = SNIP_SUBCOMMANDS
            .iter()
            .filter(|(cmd, _)| cmd.contains(&lower) || lower.is_empty())
            .map(|(cmd, desc)| CompletionItem {
                label: cmd.to_string(),
                icon_path: None,
                score: if cmd.starts_with(&lower) { 100 } else { 50 },
                description: Some(desc.to_string()),
                reason: None,
                thumb_b64: None,
                run: Some(format!("snip {cmd}")),
                ..Default::default()
            })
            .collect();

        // Also show snippet names for quick paste
        if !lower.is_empty() {
            let store = SnippetsStore::new();
            if let Ok(snippets) = store.get_snippets(&self.db) {
                for s in snippets {
                    let name_lower = s.name.to_lowercase();
                    if name_lower.contains(&lower) {
                        items.push(CompletionItem {
                            label: s.name.clone(),
                            icon_path: None,
                            score: if name_lower.starts_with(&lower) {
                                90
                            } else {
                                40
                            },
                            description: Some(Self::truncate_body(&s.body, 40).to_string()),
                            reason: None,
                            thumb_b64: None,
                            run: Some(format!("snip {}", s.name)),
                            ..Default::default()
                        });
                    }
                }
            }
        }

        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_body_never_panics_on_multibyte() {
        let s = format!("{}最tail", "a".repeat(39));
        let t = SnippetsHandler::truncate_body(&s, 40);
        assert!(t.len() <= 40 && s.is_char_boundary(t.len()));
        assert_eq!(SnippetsHandler::truncate_body("one\ntwo", 50), "one");
    }

    #[test]
    fn snip_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; it flattens to the
        // `<verb> …` string the parser reads. Copy renders WITH its verb (a
        // multi-verb grammar must); the parser strips it — see the drift test.
        assert_eq!(
            snip_args_to_flat(r#"{"action":"copy","name":"email-intro"}"#),
            "copy email-intro"
        );
        assert_eq!(
            snip_args_to_flat(r#"{"action":"add","name":"greet","body":"Hello there!"}"#),
            "add greet Hello there!"
        );
        assert_eq!(
            snip_args_to_flat(r#"{"action":"edit","name":"greet","body":"Hi."}"#),
            "edit greet Hi."
        );
        assert_eq!(
            snip_args_to_flat(r#"{"action":"delete","name":"greet"}"#),
            "delete greet"
        );
        assert_eq!(snip_args_to_flat(r#"{"action":"list"}"#), "list");
        // A verb missing its operands flattens to the bare verb, so the
        // parser's own usage error answers.
        assert_eq!(
            snip_args_to_flat(r#"{"action":"add","name":"greet"}"#),
            "add greet"
        );
        // A plain-string caller (human, legacy) passes straight through — a
        // bare name still copies via the fallthrough.
        assert_eq!(snip_args_to_flat("email-intro"), "email-intro");
        assert_eq!(snip_args_to_flat("add greet Hello"), "add greet Hello");
        // Malformed JSON → raw fallback.
        assert_eq!(snip_args_to_flat("{not json"), "{not json");
    }

    #[test]
    fn snip_schema_enum_matches_the_grammar_verbs() {
        // The derived schema's action enum must be exactly the grammar's verbs
        // — and those must stay the set `execute`'s match dispatches on.
        let names: Vec<&str> = SNIP_GRAMMAR.verbs.iter().map(|v| v.name).collect();
        assert_eq!(names, vec!["copy", "add", "edit", "delete", "list"]);
        let schema = SNIP_GRAMMAR.handler_schema();
        let en = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert_eq!(en.len(), names.len());
        for v in &names {
            assert!(en.iter().any(|e| e == v), "enum missing {v}");
        }
    }

    /// Drift guard: every verb's flat rendering (via the grammar) must be
    /// accepted by the hand-written parser — end to end through `execute`.
    /// `copy` is probed against a MISSING name: "Snippet not found: nope"
    /// proves the verb was stripped and routed to the copy path without
    /// touching the real system clipboard in tests/CI.
    #[tokio::test]
    async fn grammar_flat_rendering_is_accepted_by_the_parser() {
        let db = crate::db::open_test_database();
        let handler = SnippetsHandler::new(db.clone());
        let ctx = crate::action_registry::ExecContext::default();

        let r = handler
            .execute(
                &ctx,
                r#"{"action":"add","name":"greet","body":"Hello there!"}"#,
            )
            .await
            .unwrap();
        assert!(r.success);

        let store = SnippetsStore::new();
        let saved = store.get_snippet_by_name(&db, "greet").unwrap().unwrap();
        assert_eq!(saved.body, "Hello there!");

        let r = handler
            .execute(&ctx, r#"{"action":"edit","name":"greet","body":"Hi."}"#)
            .await
            .unwrap();
        assert!(r.success);
        let edited = store.get_snippet_by_name(&db, "greet").unwrap().unwrap();
        assert_eq!(edited.body, "Hi.");

        let r = handler.execute(&ctx, r#"{"action":"list"}"#).await.unwrap();
        match &r.output {
            Output::Rows { sections } => {
                assert!(sections[0].rows.iter().any(|row| row.title == "greet"));
            }
            other => panic!("expected rows, got {other:?}"),
        }

        let r = handler
            .execute(&ctx, r#"{"action":"copy","name":"nope"}"#)
            .await
            .unwrap();
        assert!(!r.success);
        assert_eq!(r.error.as_deref(), Some("Snippet not found: nope"));

        let r = handler
            .execute(&ctx, r#"{"action":"delete","name":"greet"}"#)
            .await
            .unwrap();
        assert!(r.success);
        assert!(store.get_snippet_by_name(&db, "greet").unwrap().is_none());
    }

    mod row_actions {
        use super::super::resolve_action;

        #[test]
        fn copy_is_the_bare_form_and_delete_is_explicit() {
            assert_eq!(
                resolve_action("copy", "email-intro").unwrap(),
                "snip email-intro"
            );
            assert_eq!(
                resolve_action("delete", "email-intro").unwrap(),
                "snip delete email-intro"
            );
        }

        #[test]
        fn real_snippet_names_survive_unchanged() {
            // Names are user-authored free text. A validator that allowlisted
            // characters (as the ssh one does) would break these, which is why
            // this handler validates a different property.
            for name in ["my note", "café ☕", "TODO: fix (#42)", "a/b", "50% off"] {
                assert_eq!(
                    resolve_action("copy", name).unwrap(),
                    format!("snip {name}"),
                    "must pass through: {name}"
                );
            }
        }

        #[test]
        fn control_characters_are_refused() {
            // A newline is the one that actually bites: `rest` is matched
            // verbatim, so everything after it would be silently dropped.
            assert!(resolve_action("copy", "note\nrm -rf /").is_err());
            assert!(resolve_action("copy", "note\rx").is_err());
            assert!(resolve_action("copy", "note\0x").is_err());
        }

        #[test]
        fn empty_and_unknown_are_refused() {
            assert!(resolve_action("copy", "").is_err());
            assert!(resolve_action("copy", "   ").is_err());
            assert!(resolve_action("exec", "email-intro").is_err());
        }
    }
}
