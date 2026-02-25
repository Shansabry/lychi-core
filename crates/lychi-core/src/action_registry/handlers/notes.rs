use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use redb::Database;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem, OutputType};
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
    fn id(&self) -> &str {
        "note"
    }

    fn description(&self) -> &str {
        "Notes — add, list, or delete notes (max 5). Usage: note <text> to add, note read to list, note delete <id> to remove"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let start = Instant::now();
        let text = args.trim();
        let store = NotesStore::new();

        // No args → open notes panel
        if text.is_empty() {
            return Ok(ActionResult {
                success: true,
                output: Some("__notes_panel__".to_string()),
                error: None,
                duration_ms: start.elapsed().as_millis() as u64,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            });
        }

        // "read" / "list" → list all notes
        if text.eq_ignore_ascii_case("read") || text.eq_ignore_ascii_case("list") {
            let notes = store.get_notes(&self.db)?;
            if notes.is_empty() {
                return Ok(ActionResult {
                    success: true,
                    output: Some("No notes saved".to_string()),
                    error: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: Some(OutputType::Text),
                    executed_args: None,
                });
            }
            let lines: Vec<String> = notes
                .iter()
                .enumerate()
                .map(|(i, n)| format!("{}. {} ({})", i + 1, Self::note_title(&n.text), n.id))
                .collect();
            return Ok(ActionResult {
                success: true,
                output: Some(format!(
                    "Notes ({}/{}):\n{}",
                    notes.len(),
                    MAX_NOTES,
                    lines.join("\n")
                )),
                error: None,
                duration_ms: start.elapsed().as_millis() as u64,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: Some(OutputType::Text),
                executed_args: None,
            });
        }

        // "delete <id>" → delete a note
        if let Some(rest) = text
            .strip_prefix("delete ")
            .or_else(|| text.strip_prefix("del "))
            .or_else(|| text.strip_prefix("rm "))
        {
            let id = rest.trim();
            store.delete_note(&self.db, id)?;
            return Ok(ActionResult {
                success: true,
                output: Some(format!("Note deleted: {id}")),
                error: None,
                duration_ms: start.elapsed().as_millis() as u64,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            });
        }

        // Add a new note
        match store.add_note(&self.db, text) {
            Ok(item) => Ok(ActionResult {
                success: true,
                output: Some(format!(
                    "Note saved ({} chars, {}/{})",
                    item.text.len(),
                    store.notes_count(&self.db)?,
                    MAX_NOTES
                )),
                error: None,
                duration_ms: start.elapsed().as_millis() as u64,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            }),
            Err(e) if e.to_string().contains("limit reached") => {
                // Return sentinel so frontend opens NotesPanel with pending note
                Ok(ActionResult {
                    success: true,
                    output: Some(format!("__notes_limit__:{text}")),
                    error: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: None,
                    executed_args: None,
                })
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
    fn id(&self) -> &str {
        "todo"
    }

    fn description(&self) -> &str {
        "Todo list — add, list, check off, or delete items. Usage: todo add <text>, todo list, todo done <id>, todo delete <id>"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let start = Instant::now();
        let trimmed = args.trim();
        let store = NotesStore::new();

        // No args → open notes panel
        if trimmed.is_empty() {
            return Ok(ActionResult {
                success: true,
                output: Some("__notes_panel__".to_string()),
                error: None,
                duration_ms: start.elapsed().as_millis() as u64,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            });
        }

        let (cmd, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
        let rest = rest.trim();

        match cmd.to_lowercase().as_str() {
            "add" => {
                if rest.is_empty() {
                    return Ok(ActionResult {
                        success: false,
                        output: None,
                        error: Some("Usage: todo add <text>".to_string()),
                        duration_ms: start.elapsed().as_millis() as u64,
                        routed_by: None,
                        open_url: None,
                        needs_confirmation: None,
                        risk_level: None,
                        output_type: None,
                        executed_args: None,
                    });
                }
                let item = store.add_todo(&self.db, rest)?;
                Ok(ActionResult {
                    success: true,
                    output: Some(format!("Added: {} ({})", item.text, item.id)),
                    error: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: None,
                    executed_args: None,
                })
            }
            "list" | "ls" => {
                let todos = store.get_todos(&self.db)?;
                if todos.is_empty() {
                    return Ok(ActionResult {
                        success: true,
                        output: Some("No todos".to_string()),
                        error: None,
                        duration_ms: start.elapsed().as_millis() as u64,
                        routed_by: None,
                        open_url: None,
                        needs_confirmation: None,
                        risk_level: None,
                        output_type: None,
                        executed_args: None,
                    });
                }
                let lines: Vec<String> = todos
                    .iter()
                    .map(|t| {
                        let check = if t.done { "x" } else { " " };
                        format!("[{check}] {} ({})", t.text, t.id)
                    })
                    .collect();
                Ok(ActionResult {
                    success: true,
                    output: Some(lines.join("\n")),
                    error: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: Some(OutputType::Text),
                    executed_args: None,
                })
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

                Ok(ActionResult {
                    success: true,
                    output: Some(lines.join("\n")),
                    error: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: Some(OutputType::Text),
                    executed_args: None,
                })
            }
            "done" | "check" | "toggle" => {
                if rest.is_empty() {
                    return Ok(ActionResult {
                        success: false,
                        output: None,
                        error: Some("Usage: todo done <id>".to_string()),
                        duration_ms: start.elapsed().as_millis() as u64,
                        routed_by: None,
                        open_url: None,
                        needs_confirmation: None,
                        risk_level: None,
                        output_type: None,
                        executed_args: None,
                    });
                }
                store.toggle_todo(&self.db, rest)?;
                Ok(ActionResult {
                    success: true,
                    output: Some(format!("Toggled: {rest}")),
                    error: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: None,
                    executed_args: None,
                })
            }
            "delete" | "del" | "rm" | "remove" => {
                if rest.is_empty() {
                    return Ok(ActionResult {
                        success: false,
                        output: None,
                        error: Some("Usage: todo delete <id>".to_string()),
                        duration_ms: start.elapsed().as_millis() as u64,
                        routed_by: None,
                        open_url: None,
                        needs_confirmation: None,
                        risk_level: None,
                        output_type: None,
                        executed_args: None,
                    });
                }
                store.delete_todo(&self.db, rest)?;
                Ok(ActionResult {
                    success: true,
                    output: Some(format!("Deleted: {rest}")),
                    error: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: None,
                    executed_args: None,
                })
            }
            // If the first word isn't a subcommand, treat the entire args as "add"
            _ => {
                let item = store.add_todo(&self.db, trimmed)?;
                Ok(ActionResult {
                    success: true,
                    output: Some(format!("Added: {} ({})", item.text, item.id)),
                    error: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: None,
                    executed_args: None,
                })
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
            })
            .collect()
    }
}
