use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use redb::Database;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
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
    ActionResult::ok(output, OutputType::Status).with_duration(start.elapsed().as_millis() as u64)
}

fn ok_text(start: Instant, output: String) -> ActionResult {
    ActionResult::ok(output, OutputType::Text).with_duration(start.elapsed().as_millis() as u64)
}

fn err_result(start: Instant, error: String) -> ActionResult {
    ActionResult::err(error).with_duration(start.elapsed().as_millis() as u64)
}

/// Case-insensitively find the last occurrence of an ASCII `needle` in `haystack`,
/// returning a byte offset that is ALWAYS valid in `haystack`. We can't search a
/// `to_lowercase()` copy and slice the original: `to_lowercase()` can change byte
/// length (e.g. `İ` → `i̇`, 2→3 bytes), so offsets in the lowercased string may land
/// mid-UTF-8-char in the original and panic. Since our needles are all ASCII, we
/// walk `haystack`'s own byte indices and compare case-insensitively.
fn rfind_ci_ascii(haystack: &str, needle: &str) -> Option<usize> {
    debug_assert!(needle.is_ascii());
    let hb = haystack.as_bytes();
    let nb = needle.as_bytes();
    if nb.is_empty() || hb.len() < nb.len() {
        return None;
    }
    // Scan from the end so we return the LAST match (matches the old rfind).
    (0..=hb.len() - nb.len()).rev().find(|&i| {
        hb[i..i + nb.len()]
            .iter()
            .zip(nb)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    })
}

/// Try to split "buy milk in 30 minutes" into ("buy milk", "in 30 minutes").
/// Searches for time-indicator words from the end of the string.
fn split_text_and_time(input: &str) -> Option<(String, u64)> {
    // Offsets come from a case-insensitive scan of `input` ITSELF, so slicing
    // `input` at them is always on a char boundary (no lowercased-copy skew).

    // Try splitting at "at <time>" — find last occurrence of " at "
    if let Some(pos) = rfind_ci_ascii(input, " at ") {
        let text = input[..pos].trim();
        let time_part = input[pos + 1..].trim(); // "at <time>"
        if let Some(due) = time_parse::parse_reminder_time(time_part).filter(|_| !text.is_empty()) {
            return Some((text.to_string(), due));
        }
    }

    // Try splitting at "in <duration>" — find last occurrence of " in "
    if let Some(pos) = rfind_ci_ascii(input, " in ") {
        let text = input[..pos].trim();
        let time_part = input[pos + 1..].trim(); // "in 30 minutes"
        if let Some(due) = time_parse::parse_reminder_time(time_part).filter(|_| !text.is_empty()) {
            return Some((text.to_string(), due));
        }
    }

    // Try splitting at "tomorrow" — "buy milk tomorrow 9am"
    if let Some(pos) = rfind_ci_ascii(input, "tomorrow") {
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
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["reminder"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "reminder"
    }

    fn description(&self) -> &str {
        "Reminders — timed desktop notifications. Usage: reminder add <text> in/at <time>, reminder list, reminder delete <id>"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
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
                reason: None,
                thumb_b64: None,
                run: Some(format!("reminder {cmd}")),
                ..Default::default()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_registry::Output;

    /// Extract the text body from a result's output, for assertions.
    fn body(r: &ActionResult) -> Option<&str> {
        match &r.output {
            Output::Text { body, .. } => Some(body.as_str()),
            _ => None,
        }
    }

    #[test]
    fn split_text_and_time_in() {
        let (text, _due) = split_text_and_time("buy milk in 30 minutes").unwrap();
        assert_eq!(text, "buy milk");
    }

    #[test]
    fn split_text_and_time_non_ascii_no_panic() {
        // `İ` (U+0130) lowercases to 2 chars / 3 bytes, so a lowercased-copy
        // offset would misalign into the original and panic on a char boundary.
        // These must simply not panic (result content is unimportant here).
        let _ = split_text_and_time("İ buy milk in 5 minutes");
        let _ = split_text_and_time("çalış at 9am");
        let _ = split_text_and_time("straße tomorrow 9am");
        let _ = split_text_and_time("İİİ at İ");
        let _ = rfind_ci_ascii("İ AT noon", " at ");
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

        let result = handler
            .execute(
                &crate::action_registry::ExecContext::default(),
                "add buy milk in 30 minutes",
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(body(&result).unwrap().contains("Reminder set"));

        let result = handler
            .execute(&crate::action_registry::ExecContext::default(), "list")
            .await
            .unwrap();
        assert!(result.success);
        assert!(body(&result).unwrap().contains("buy milk"));
    }

    #[tokio::test]
    async fn handler_empty_opens_panel() {
        let db = crate::db::open_test_database();
        let handler = RemindersHandler::new(db);

        let result = handler
            .execute(&crate::action_registry::ExecContext::default(), "")
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(body(&result), Some("__reminders_panel__"));
    }

    #[tokio::test]
    async fn handler_implicit_add() {
        let db = crate::db::open_test_database();
        let handler = RemindersHandler::new(db);

        // Without "add" subcommand — should still work
        let result = handler
            .execute(
                &crate::action_registry::ExecContext::default(),
                "standup at 5pm",
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(body(&result).unwrap().contains("Reminder set"));
    }
}
