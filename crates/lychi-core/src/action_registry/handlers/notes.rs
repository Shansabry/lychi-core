use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use redb::Database;

use crate::action_registry::{
    ActionHandler, ActionResult, BadgeTone, CommandCategory, CompletionItem, ExecContext, Output,
    OutputType, RiskLevel, Row, Section,
};
use crate::error::LychiError;
use crate::notes::store::{MAX_NOTES, NotesStore};

// ---- Notes handler ----

const NOTE_SUBCOMMANDS: &[(&str, &str)] = &[
    ("read", "List all saved notes"),
    ("delete", "Delete a note by ID (e.g. note delete abc)"),
];

/// Whether `s` is a store-generated item id.
///
/// Note and todo ids come from `db::new_id()` — a UUIDv7 — so unlike snippet
/// names (user-authored free text) this can be validated strictly. Anything
/// that is not a well-formed UUID did not come from the store, so it cannot
/// name a real row and is refused rather than passed to a lookup.
fn is_valid_item_id(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok()
}

/// Resolve a note row action into the command it stands for.
pub fn resolve_note_action(id: &str, target: &str) -> Result<String, String> {
    if id != "delete" {
        return Err(format!("Unknown note action '{id}'"));
    }
    if !is_valid_item_id(target) {
        return Err(format!("Invalid note id '{target}'"));
    }
    Ok(format!("note delete {target}"))
}

/// Resolve a todo row action into the command it stands for.
///
/// `toggle` maps to the `done` verb because the store operation IS a toggle —
/// there is no separate un-done path, and pretending otherwise in the action id
/// would invent a distinction the backend does not have.
pub fn resolve_todo_action(id: &str, target: &str) -> Result<String, String> {
    let verb = match id {
        "toggle" => "done",
        "delete" => "delete",
        other => return Err(format!("Unknown todo action '{other}'")),
    };
    if !is_valid_item_id(target) {
        return Err(format!("Invalid todo id '{target}'"));
    }
    Ok(format!("todo {verb} {target}"))
}

pub struct NotesHandler {
    db: Arc<Database>,
}

impl NotesHandler {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Format a note's first line as its title (truncated to ~40 bytes on a
    /// char boundary — a naive `[..40]` panics when byte 40 splits a multi-byte
    /// UTF-8 char, e.g. an emoji or CJK glyph in the note).
    fn note_title(text: &str) -> &str {
        let first_line = text.lines().next().unwrap_or(text);
        if first_line.len() > 40 {
            let mut end = 40;
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
        "Notes — add, list, or delete notes. Usage: note <text> to add, note read to list, note delete <id> to remove"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
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
            // The id was being printed as visible text purely so the user could
            // retype it into `note delete <id>`. A row carries it invisibly as
            // the action target, so the id leaves the display entirely and the
            // age it never showed takes that space instead.
            let rows: Vec<Row> = notes
                .iter()
                .map(|n| {
                    Row::new(Self::note_title(&n.text))
                        .accessory_at(n.updated_at as i64)
                        .action("delete", "Delete", &n.id, Some(RiskLevel::Medium))
                })
                .collect();
            return Ok(ActionResult {
                success: true,
                output: Output::Rows {
                    sections: vec![Section {
                        title: Some(format!("Notes ({}/{})", rows.len(), MAX_NOTES)),
                        rows,
                        handler: "notes".to_string(),
                    }],
                },
                duration_ms: start.elapsed().as_millis() as u64,
                ..Default::default()
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
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
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
                // `[x]`/`[ ]` was ASCII standing in for state a badge expresses
                // directly, and the id was shown only so it could be retyped
                // into `todo done <id>` — which is now the row's own action.
                let rows: Vec<Row> = todos
                    .iter()
                    .map(|t| {
                        let row = Row::new(&t.text)
                            // One id, not done/undone: the store verb is a
                            // TOGGLE, so inventing two action ids would imply a
                            // distinction the backend does not have. Only the
                            // label changes with current state.
                            .action(
                                "toggle",
                                if t.done { "Mark undone" } else { "Mark done" },
                                &t.id,
                                None,
                            )
                            .action("delete", "Delete", &t.id, Some(RiskLevel::Medium));
                        if t.done {
                            row.badge("done", BadgeTone::Ok)
                        } else {
                            row
                        }
                    })
                    .collect();
                Ok(ActionResult {
                    success: true,
                    output: Output::Rows {
                        sections: vec![Section {
                            title: Some(format!("Todos ({})", rows.len())),
                            rows,
                            handler: "todos".to_string(),
                        }],
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                    ..Default::default()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_title_never_panics_on_multibyte() {
        // 40th byte lands inside a multi-byte char — must not panic, must return
        // valid UTF-8 truncated before the char.
        let s = format!("{}最", "a".repeat(39)); // 39 ASCII + 3-byte char at 39..42
        let t = NotesHandler::note_title(&s);
        assert!(t.len() <= 40 && s.is_char_boundary(t.len()));
        // Emoji straddling the boundary.
        let e = format!("{}😀ok", "x".repeat(38));
        let t = NotesHandler::note_title(&e);
        assert!(t.chars().all(|c| c == 'x'));
        // First line only.
        assert_eq!(NotesHandler::note_title("hello\nworld"), "hello");
    }

    mod row_actions {
        use super::super::{resolve_note_action, resolve_todo_action};

        /// A real store id, so the tests exercise the same shape production does.
        fn an_id() -> String {
            crate::db::new_id()
        }

        #[test]
        fn note_delete_resolves() {
            let id = an_id();
            assert_eq!(
                resolve_note_action("delete", &id).unwrap(),
                format!("note delete {id}")
            );
        }

        #[test]
        fn todo_toggle_maps_to_the_done_verb() {
            // The store operation is a toggle; `done` is the verb that performs
            // it. If this ever splits into two verbs, this test is the thing
            // that should fail.
            let id = an_id();
            assert_eq!(
                resolve_todo_action("toggle", &id).unwrap(),
                format!("todo done {id}")
            );
            assert_eq!(
                resolve_todo_action("delete", &id).unwrap(),
                format!("todo delete {id}")
            );
        }

        #[test]
        fn a_non_uuid_target_is_refused() {
            // Ids are store-generated UUIDs. Anything else did not come from a
            // row, so it must not reach a store lookup — this is what stops the
            // row-action channel being a way to name arbitrary strings.
            for bad in ["", "1", "../../x", "note delete x", "'; drop --"] {
                assert!(resolve_note_action("delete", bad).is_err(), "note: {bad}");
                assert!(resolve_todo_action("toggle", bad).is_err(), "todo: {bad}");
            }
        }

        #[test]
        fn unknown_action_ids_are_refused() {
            let id = an_id();
            assert!(resolve_note_action("toggle", &id).is_err());
            assert!(resolve_todo_action("archive", &id).is_err());
        }
    }
}
