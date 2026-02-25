use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use redb::Database;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem, OutputType};
use crate::error::LychiError;
use crate::reminders::store::RemindersStore;
use crate::reminders::time_parse;

pub struct RemindersHandler {
    db: Arc<Database>,
}

impl RemindersHandler {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
}

const REMINDER_SUBCOMMANDS: &[(&str, &str)] = &[
    ("add", "Add a reminder (e.g. reminder add buy milk in 30m)"),
    ("list", "List all active reminders"),
    ("delete", "Delete a reminder by ID"),
];

fn ok_result(start: Instant, output: String) -> ActionResult {
    ActionResult {
        success: true,
        output: Some(output),
        error: None,
        duration_ms: start.elapsed().as_millis() as u64,
        routed_by: None,
        open_url: None,
        needs_confirmation: None,
        risk_level: None,
        output_type: Some(OutputType::Status),
        executed_args: None,
    }
}

fn ok_text(start: Instant, output: String) -> ActionResult {
    ActionResult {
        success: true,
        output: Some(output),
        error: None,
        duration_ms: start.elapsed().as_millis() as u64,
        routed_by: None,
        open_url: None,
        needs_confirmation: None,
        risk_level: None,
        output_type: Some(OutputType::Text),
        executed_args: None,
    }
}

fn err_result(start: Instant, error: String) -> ActionResult {
    ActionResult {
        success: false,
        output: None,
        error: Some(error),
        duration_ms: start.elapsed().as_millis() as u64,
        routed_by: None,
        open_url: None,
        needs_confirmation: None,
        risk_level: None,
        output_type: None,
        executed_args: None,
    }
}

/// Try to split "buy milk in 30 minutes" into ("buy milk", "in 30 minutes").
/// Searches for time-indicator words from the end of the string.
fn split_text_and_time(input: &str) -> Option<(String, u64)> {
    let lower = input.to_lowercase();

    // Try splitting at "at <time>" — find last occurrence of " at "
    if let Some(pos) = lower.rfind(" at ") {
        let text = input[..pos].trim();
        let time_part = input[pos + 1..].trim(); // "at <time>"
        if let Some(due) = time_parse::parse_reminder_time(time_part).filter(|_| !text.is_empty()) {
            return Some((text.to_string(), due));
        }
    }

    // Try splitting at "in <duration>" — find last occurrence of " in "
    if let Some(pos) = lower.rfind(" in ") {
        let text = input[..pos].trim();
        let time_part = input[pos + 1..].trim(); // "in 30 minutes"
        if let Some(due) = time_parse::parse_reminder_time(time_part).filter(|_| !text.is_empty()) {
            return Some((text.to_string(), due));
        }
    }

    // Try splitting at "tomorrow" — "buy milk tomorrow 9am"
    if let Some(pos) = lower.rfind("tomorrow") {
        let text = input[..pos].trim();
        let time_part = input[pos..].trim(); // "tomorrow 9am"
        if let Some(due) = time_parse::parse_reminder_time(time_part).filter(|_| !text.is_empty()) {
            return Some((text.to_string(), due));
        }
    }

    None
}

#[async_trait]
impl ActionHandler for RemindersHandler {
    fn id(&self) -> &str {
        "reminder"
    }

    fn description(&self) -> &str {
        "Reminders — timed desktop notifications. Usage: reminder add <text> in/at <time>, reminder list, reminder delete <id>"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let start = Instant::now();
        let trimmed = args.trim();
        let store = RemindersStore::new();

        // No args → open reminders panel
        if trimmed.is_empty() {
            return Ok(ok_result(start, "__reminders_panel__".into()));
        }

        let (cmd, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
        let rest = rest.trim();

        match cmd.to_lowercase().as_str() {
            "add" => {
                if rest.is_empty() {
                    return Ok(err_result(
                        start,
                        "Usage: reminder add <text> in/at <time>\nExamples: reminder add buy milk in 30m, reminder add standup at 9am".into(),
                    ));
                }

                // Parse: "buy milk in 30 minutes" or "standup at 9am" or "meeting tomorrow 2pm"
                match split_text_and_time(rest) {
                    Some((text, due_at)) => {
                        let item = store.add_reminder(&self.db, &text, due_at)?;
                        let rel = time_parse::format_relative(item.due_at);
                        let abs = time_parse::format_absolute(item.due_at);
                        Ok(ok_result(
                            start,
                            format!("Reminder set: \"{}\" — {abs} ({rel})", item.text),
                        ))
                    }
                    None => Ok(err_result(
                        start,
                        format!(
                            "Couldn't parse time from: \"{rest}\"\nTry: reminder add <text> in <duration> or reminder add <text> at <time>"
                        ),
                    )),
                }
            }

            "list" | "ls" => {
                let reminders = store.list_reminders(&self.db)?;
                if reminders.is_empty() {
                    return Ok(ok_text(start, "No reminders".into()));
                }

                let lines: Vec<String> = reminders
                    .iter()
                    .map(|r| {
                        let status = if r.fired {
                            "fired".to_string()
                        } else {
                            time_parse::format_relative(r.due_at)
                        };
                        format!("- {} ({}) [{}]", r.text, status, r.id)
                    })
                    .collect();

                let active = reminders.iter().filter(|r| !r.fired).count();
                let fired = reminders.iter().filter(|r| r.fired).count();
                let header = if fired > 0 {
                    format!("Reminders ({active} active, {fired} fired):")
                } else {
                    format!("Reminders ({active}):")
                };

                Ok(ok_text(start, format!("{header}\n{}", lines.join("\n"))))
            }

            "delete" | "del" | "rm" | "remove" => {
                if rest.is_empty() {
                    return Ok(err_result(start, "Usage: reminder delete <id>".into()));
                }
                store.delete_reminder(&self.db, rest)?;
                Ok(ok_result(start, format!("Reminder deleted: {rest}")))
            }

            "clear" => {
                let reminders = store.list_reminders(&self.db)?;
                let mut count = 0;
                for r in &reminders {
                    if r.fired {
                        let _ = store.delete_reminder(&self.db, &r.id);
                        count += 1;
                    }
                }
                if count == 0 {
                    Ok(ok_result(start, "No fired reminders to clear".into()))
                } else {
                    Ok(ok_result(
                        start,
                        format!("Cleared {count} fired reminder(s)"),
                    ))
                }
            }

            // If first word isn't a subcommand, treat entire args as "add"
            _ => match split_text_and_time(trimmed) {
                Some((text, due_at)) => {
                    let item = store.add_reminder(&self.db, &text, due_at)?;
                    let rel = time_parse::format_relative(item.due_at);
                    let abs = time_parse::format_absolute(item.due_at);
                    Ok(ok_result(
                        start,
                        format!("Reminder set: \"{}\" — {abs} ({rel})", item.text),
                    ))
                }
                None => Ok(err_result(
                    start,
                    format!(
                        "Couldn't parse time from: \"{trimmed}\"\nTry: reminder add <text> in <duration> or reminder add <text> at <time>"
                    ),
                )),
            },
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let lower = partial.to_lowercase();
        REMINDER_SUBCOMMANDS
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_text_and_time_in() {
        let (text, _due) = split_text_and_time("buy milk in 30 minutes").unwrap();
        assert_eq!(text, "buy milk");
    }

    #[test]
    fn split_text_and_time_at() {
        let (text, _due) = split_text_and_time("standup at 5pm").unwrap();
        assert_eq!(text, "standup");
    }

    #[test]
    fn split_text_and_time_compact() {
        let (text, _due) = split_text_and_time("buy milk in 30m").unwrap();
        assert_eq!(text, "buy milk");
    }

    #[test]
    fn split_text_and_time_tomorrow() {
        let (text, _due) = split_text_and_time("meeting tomorrow 9am").unwrap();
        assert_eq!(text, "meeting");
    }

    #[tokio::test]
    async fn handler_add_and_list() {
        let db = crate::db::open_test_database();
        let handler = RemindersHandler::new(db);

        let result = handler.execute("add buy milk in 30 minutes").await.unwrap();
        assert!(result.success);
        assert!(result.output.unwrap().contains("Reminder set"));

        let result = handler.execute("list").await.unwrap();
        assert!(result.success);
        assert!(result.output.unwrap().contains("buy milk"));
    }

    #[tokio::test]
    async fn handler_empty_opens_panel() {
        let db = crate::db::open_test_database();
        let handler = RemindersHandler::new(db);

        let result = handler.execute("").await.unwrap();
        assert!(result.success);
        assert_eq!(result.output.as_deref(), Some("__reminders_panel__"));
    }

    #[tokio::test]
    async fn handler_implicit_add() {
        let db = crate::db::open_test_database();
        let handler = RemindersHandler::new(db);

        // Without "add" subcommand — should still work
        let result = handler.execute("standup at 5pm").await.unwrap();
        assert!(result.success);
        assert!(result.output.unwrap().contains("Reminder set"));
    }
}
