use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use redb::Database;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, Output, OutputType,
    RiskLevel, Row, Section,
};
use crate::error::LychiError;
use crate::snippets::store::SnippetsStore;

use super::clipboard::write_to_clipboard;

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
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let start = Instant::now();
        let text = args.trim();
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
            // Default: search by name and copy to clipboard
            _ => {
                // Treat the entire args as a snippet name query
                if let Some(item) = store.get_snippet_by_name(&self.db, text)? {
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
                    Ok(ActionResult::err(format!("Snippet not found: {text}"))
                        .with_duration(start.elapsed().as_millis() as u64))
                }
            }
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
