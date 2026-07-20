use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use redb::Database;

use crate::action_registry::{
    ActionHandler, ActionResult, CompletionItem, ExecContext, OutputType,
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

pub struct SnippetsHandler {
    db: Arc<Database>,
}

impl SnippetsHandler {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    fn truncate_body(body: &str, max: usize) -> &str {
        let first_line = body.lines().next().unwrap_or(body);
        if first_line.len() > max {
            &first_line[..max]
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
                let lines: Vec<String> = snippets
                    .iter()
                    .map(|s| format!("  {} — {}", s.name, Self::truncate_body(&s.body, 50)))
                    .collect();
                Ok(ActionResult::ok(
                    format!("Snippets ({}):\n{}", snippets.len(), lines.join("\n")),
                    OutputType::Text,
                )
                .with_duration(start.elapsed().as_millis() as u64))
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
