use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem};
use crate::error::LychiError;
use crate::notes::store::NotesStore;

// ---- Notes handler ----

pub struct NotesHandler {
    store: Arc<RwLock<NotesStore>>,
}

impl NotesHandler {
    pub fn new(store: Arc<RwLock<NotesStore>>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl ActionHandler for NotesHandler {
    fn id(&self) -> &str {
        "note"
    }

    fn description(&self) -> &str {
        "Quick note — set or view a sticky note. Usage: note <text> to set, note to view"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let start = Instant::now();
        let text = args.trim();

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
            });
        }

        // "read" → return current note content
        if text.eq_ignore_ascii_case("read") {
            let store = self.store.read().await;
            let current = store.get_note();
            let output = if current.is_empty() {
                "No note saved".to_string()
            } else {
                current.to_string()
            };
            return Ok(ActionResult {
                success: true,
                output: Some(output),
                error: None,
                duration_ms: start.elapsed().as_millis() as u64,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
            });
        }

        // Set the note
        let mut store = self.store.write().await;
        store.set_note(text)?;

        Ok(ActionResult {
            success: true,
            output: Some(format!("Note saved ({} chars)", text.len())),
            error: None,
            duration_ms: start.elapsed().as_millis() as u64,
            routed_by: None,
            open_url: None,
            needs_confirmation: None,
            risk_level: None,
        })
    }
}

// ---- Todo handler ----

pub struct TodoHandler {
    store: Arc<RwLock<NotesStore>>,
}

impl TodoHandler {
    pub fn new(store: Arc<RwLock<NotesStore>>) -> Self {
        Self { store }
    }
}

const TODO_SUBCOMMANDS: &[&str] = &["add", "list", "done", "delete", "summary"];

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
            });
        }

        let (cmd, rest) = trimmed
            .split_once(' ')
            .unwrap_or((trimmed, ""));
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
                    });
                }
                let mut store = self.store.write().await;
                let item = store.add_todo(rest)?;
                Ok(ActionResult {
                    success: true,
                    output: Some(format!("Added: {} ({})", item.text, item.id)),
                    error: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                })
            }
            "list" | "ls" => {
                let store = self.store.read().await;
                let todos = store.get_todos();
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
                })
            }
            "summary" => {
                let store = self.store.read().await;
                let note = store.get_note();
                let todos = store.get_todos();

                let mut lines = Vec::new();

                // Note section
                if !note.is_empty() {
                    lines.push("📋 Note:".to_string());
                    lines.push(note.to_string());
                    lines.push(String::new());
                }

                // Todos section
                let pending: Vec<_> = todos.iter().filter(|t| !t.done).collect();
                let done: Vec<_> = todos.iter().filter(|t| t.done).collect();

                if !pending.is_empty() {
                    lines.push(format!("☐ Pending ({}):", pending.len()));
                    for t in &pending {
                        lines.push(format!("  • {}", t.text));
                    }
                }

                if !done.is_empty() {
                    if !pending.is_empty() {
                        lines.push(String::new());
                    }
                    lines.push(format!("☑ Done ({}):", done.len()));
                    for t in &done {
                        lines.push(format!("  • {}", t.text));
                    }
                }

                if lines.is_empty() {
                    lines.push("Nothing here — note is empty and no todos.".to_string());
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
                    });
                }
                let mut store = self.store.write().await;
                store.toggle_todo(rest)?;
                Ok(ActionResult {
                    success: true,
                    output: Some(format!("Toggled: {rest}")),
                    error: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
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
                    });
                }
                let mut store = self.store.write().await;
                store.delete_todo(rest)?;
                Ok(ActionResult {
                    success: true,
                    output: Some(format!("Deleted: {rest}")),
                    error: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                })
            }
            // If the first word isn't a subcommand, treat the entire args as "add"
            _ => {
                let mut store = self.store.write().await;
                let item = store.add_todo(trimmed)?;
                Ok(ActionResult {
                    success: true,
                    output: Some(format!("Added: {} ({})", item.text, item.id)),
                    error: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                    routed_by: None,
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                })
            }
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let lower = partial.to_lowercase();
        TODO_SUBCOMMANDS
            .iter()
            .filter(|s| s.contains(&lower) || lower.is_empty())
            .map(|s| CompletionItem {
                label: s.to_string(),
                icon_path: None,
                score: if s.starts_with(&lower) { 100 } else { 50 },
            })
            .collect()
    }
}
