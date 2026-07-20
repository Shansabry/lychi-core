use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use redb::Database;

use crate::action_registry::{
    ActionHandler, ActionResult, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;
use crate::notes::store::{MAX_NOTES, NotesStore};

// ---- Notes handler ----

const NOTE_SUBCOMMANDS: &[(&str, &str)] = &[
    ("read", "List all saved notes"),
    ("delete", "Delete a note by ID (e.g. note delete abc)"),
];

pub struct NotesHandler {
    db: Arc<Database>,
}

impl NotesHandler {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Format a note's first line as its title (truncated to 40 chars).
    fn note_title(text: &str) -> &str {
        let first_line = text.lines().next().unwrap_or(text);
        if first_line.len() > 40 {
            &first_line[..40]
        } else {
            first_line
        }
    }
}

#[async_trait]
impl ActionHandler for NotesHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["note", "notes"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "note"
    }

    fn description(&self) -> &str {
        "Notes — add, list, or delete notes (max 5). Usage: note <text> to add, note read to list, note delete <id> to remove"
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let start = Instant::now();
        let text = args.trim();
        let store = NotesStore::new();

        // No args → open notes panel
        if text.is_empty() {
            return Ok(
                ActionResult::ok("__notes_panel__".to_string(), OutputType::Status)
                    .with_duration(start.elapsed().as_millis() as u64),
            );
        }

        // "read" / "list" → list all notes
        if text.eq_ignore_ascii_case("read") || text.eq_ignore_ascii_case("list") {
            let notes = store.get_notes(&self.db)?;
            if notes.is_empty() {
                return Ok(
                    ActionResult::ok("No notes saved".to_string(), OutputType::Text)
                        .with_duration(start.elapsed().as_millis() as u64),
                );
            }
            let lines: Vec<String> = notes
                .iter()
                .enumerate()
                .map(|(i, n)| format!("{}. {} ({})", i + 1, Self::note_title(&n.text), n.id))
                .collect();
            return Ok(ActionResult::ok(
                format!(
                    "Notes ({}/{}):\n{}",
                    notes.len(),
                    MAX_NOTES,
                    lines.join("\n")
                ),
                OutputType::Text,
            )
            .with_duration(start.elapsed().as_millis() as u64));
        }

        // "delete <id>" → delete a note
        if let Some(rest) = text
            .strip_prefix("delete ")
            .or_else(|| text.strip_prefix("del "))
            .or_else(|| text.strip_prefix("rm "))
        {
            let id = rest.trim();
            store.delete_note(&self.db, id)?;
            return Ok(
                ActionResult::ok(format!("Note deleted: {id}"), OutputType::Status)
                    .with_duration(start.elapsed().as_millis() as u64),
            );
        }

        // Add a new note
        match store.add_note(&self.db, text) {
            Ok(item) => Ok(ActionResult::ok(
                format!(
                    "Note saved ({} chars, {}/{})",
                    item.text.len(),
                    store.notes_count(&self.db)?,
                    MAX_NOTES
                ),
                OutputType::Status,
            )
            .with_duration(start.elapsed().as_millis() as u64)),
            Err(e) if e.to_string().contains("limit reached") => {
                // Return sentinel so frontend opens NotesPanel with pending note
                Ok(
                    ActionResult::ok(format!("__notes_limit__:{text}"), OutputType::Status)
                        .with_duration(start.elapsed().as_millis() as u64),
                )
            }
            Err(e) => Err(e),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let lower = partial.to_lowercase();
        NOTE_SUBCOMMANDS
            .iter()
            .filter(|(cmd, _)| cmd.contains(&lower) || lower.is_empty())
            .map(|(cmd, desc)| CompletionItem {
                label: cmd.to_string(),
                icon_path: None,
                score: if cmd.starts_with(&lower) { 100 } else { 50 },
                description: Some(desc.to_string()),
                reason: None,
                thumb_b64: None,
                run: Some(format!("note {cmd}")),
                ..Default::default()
            })
            .collect()
    }
}

// ---- Todo handler ----

pub struct TodoHandler {
    db: Arc<Database>,
}

impl TodoHandler {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
}

const TODO_SUBCOMMANDS: &[(&str, &str)] = &[
    ("add", "Add a todo item (e.g. todo add buy milk)"),
    ("list", "List all todo items"),
    ("done", "Mark a todo as done by ID"),
    ("delete", "Delete a todo by ID"),
    ("summary", "Show notes and todos summary"),
];

#[async_trait]
impl ActionHandler for TodoHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["todo", "todos"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "todo"
    }

    fn description(&self) -> &str {
        "Todo list — add, list, check off, or delete items. Usage: todo add <text>, todo list, todo done <id>, todo delete <id>"
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let start = Instant::now();
        let trimmed = args.trim();
        let store = NotesStore::new();

        // No args → open notes panel
        if trimmed.is_empty() {
            return Ok(
                ActionResult::ok("__notes_panel__".to_string(), OutputType::Status)
                    .with_duration(start.elapsed().as_millis() as u64),
            );
        }

        let (cmd, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
        let rest = rest.trim();

        match cmd.to_lowercase().as_str() {
            "add" => {
                if rest.is_empty() {
                    return Ok(ActionResult::err("Usage: todo add <text>".to_string())
                        .with_duration(start.elapsed().as_millis() as u64));
                }
                let item = store.add_todo(&self.db, rest)?;
                Ok(ActionResult::ok(
                    format!("Added: {} ({})", item.text, item.id),
                    OutputType::Status,
                )
                .with_duration(start.elapsed().as_millis() as u64))
            }
            "list" | "ls" => {
                let todos = store.get_todos(&self.db)?;
                if todos.is_empty() {
                    return Ok(ActionResult::ok("No todos".to_string(), OutputType::Status)
                        .with_duration(start.elapsed().as_millis() as u64));
                }
                let lines: Vec<String> = todos
                    .iter()
                    .map(|t| {
                        let check = if t.done { "x" } else { " " };
                        format!("[{check}] {} ({})", t.text, t.id)
                    })
                    .collect();
                Ok(ActionResult::ok(lines.join("\n"), OutputType::Text)
                    .with_duration(start.elapsed().as_millis() as u64))
            }
            "summary" => {
                let notes = store.get_notes(&self.db)?;
                let todos = store.get_todos(&self.db)?;

                let mut lines = Vec::new();

                // Notes section
                if !notes.is_empty() {
                    lines.push(format!("Notes ({}/{}):", notes.len(), MAX_NOTES));
                    for (i, n) in notes.iter().enumerate() {
                        let title = n.text.lines().next().unwrap_or(&n.text);
                        lines.push(format!("  {}. {}", i + 1, title));
                    }
                    lines.push(String::new());
                }

                // Todos section
                let pending: Vec<_> = todos.iter().filter(|t| !t.done).collect();
                let done: Vec<_> = todos.iter().filter(|t| t.done).collect();

                if !pending.is_empty() {
                    lines.push(format!("Pending ({}):", pending.len()));
                    for t in &pending {
                        lines.push(format!("  - {}", t.text));
                    }
                }

                if !done.is_empty() {
                    if !pending.is_empty() {
                        lines.push(String::new());
                    }
                    lines.push(format!("Done ({}):", done.len()));
                    for t in &done {
                        lines.push(format!("  - {}", t.text));
                    }
                }

                if lines.is_empty() {
                    lines.push("Nothing here — no notes and no todos.".to_string());
                }

                Ok(ActionResult::ok(lines.join("\n"), OutputType::Text)
                    .with_duration(start.elapsed().as_millis() as u64))
            }
            "done" | "check" | "toggle" => {
                if rest.is_empty() {
                    return Ok(ActionResult::err("Usage: todo done <id>".to_string())
                        .with_duration(start.elapsed().as_millis() as u64));
                }
                store.toggle_todo(&self.db, rest)?;
                Ok(
                    ActionResult::ok(format!("Toggled: {rest}"), OutputType::Status)
                        .with_duration(start.elapsed().as_millis() as u64),
                )
            }
            "delete" | "del" | "rm" | "remove" => {
                if rest.is_empty() {
                    return Ok(ActionResult::err("Usage: todo delete <id>".to_string())
                        .with_duration(start.elapsed().as_millis() as u64));
                }
                store.delete_todo(&self.db, rest)?;
                Ok(
                    ActionResult::ok(format!("Deleted: {rest}"), OutputType::Status)
                        .with_duration(start.elapsed().as_millis() as u64),
                )
            }
            // If the first word isn't a subcommand, treat the entire args as "add"
            _ => {
                let item = store.add_todo(&self.db, trimmed)?;
                Ok(ActionResult::ok(
                    format!("Added: {} ({})", item.text, item.id),
                    OutputType::Status,
                )
                .with_duration(start.elapsed().as_millis() as u64))
            }
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let lower = partial.to_lowercase();
        TODO_SUBCOMMANDS
            .iter()
            .filter(|(cmd, _)| cmd.contains(&lower) || lower.is_empty())
            .map(|(cmd, desc)| CompletionItem {
                label: cmd.to_string(),
                icon_path: None,
                score: if cmd.starts_with(&lower) { 100 } else { 50 },
                description: Some(desc.to_string()),
                reason: None,
                thumb_b64: None,
                run: Some(format!("todo {cmd}")),
                ..Default::default()
            })
            .collect()
    }
}
