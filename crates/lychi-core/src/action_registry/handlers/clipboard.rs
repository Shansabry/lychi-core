use std::sync::Arc;

use async_trait::async_trait;
use redb::Database;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem, OutputType};
use crate::clipboard::store::ClipboardStore;
use crate::error::LychiError;

pub struct ClipboardHandler {
    store: ClipboardStore,
    db: Arc<Database>,
}

impl ClipboardHandler {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            store: ClipboardStore::new(),
            db,
        }
    }

    fn truncate_label(text: &str, max_len: usize) -> String {
        let single_line = text.lines().next().unwrap_or(text);
        if single_line.len() <= max_len {
            single_line.to_string()
        } else {
            format!("{}...", &single_line[..max_len - 3])
        }
    }

    fn format_age(created_at: u64) -> String {
        let now = crate::db::now_millis();
        let elapsed_secs = now.saturating_sub(created_at) / 1000;

        if elapsed_secs < 60 {
            "just now".to_string()
        } else if elapsed_secs < 3600 {
            let mins = elapsed_secs / 60;
            format!("{mins}m ago")
        } else if elapsed_secs < 86400 {
            let hours = elapsed_secs / 3600;
            format!("{hours}h ago")
        } else {
            let days = elapsed_secs / 86400;
            format!("{days}d ago")
        }
    }
}

#[async_trait]
impl ActionHandler for ClipboardHandler {
    fn id(&self) -> &str {
        "clip"
    }

    fn description(&self) -> &str {
        "Browse and paste from clipboard history"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let args = args.trim();

        // "clear" subcommand
        if args == "clear" {
            self.store.clear(&self.db)?;
            return Ok(ActionResult {
                success: true,
                output: Some("Clipboard history cleared".to_string()),
                error: None,
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: Some(OutputType::Status),
                executed_args: None,
            });
        }

        // Selection from completions — find matching entry and write it back to clipboard
        if !args.is_empty() && args != "clear" {
            let entries = self.store.get_entries(&self.db, 100)?;
            // Try UUID match first, then text prefix match (completions show truncated text)
            if let Some(entry) = entries.iter().find(|e| e.id == args).or_else(|| {
                entries.iter().find(|e| {
                    e.text.starts_with(args) || args.starts_with(&Self::truncate_label(&e.text, 80))
                })
            }) {
                match write_to_clipboard(&entry.text) {
                    Ok(()) => {
                        let preview = Self::truncate_label(&entry.text, 60);
                        return Ok(ActionResult {
                            success: true,
                            output: Some(format!("Copied: {preview}")),
                            error: None,
                            duration_ms: 0,
                            routed_by: None,
                            open_url: None,
                            needs_confirmation: None,
                            risk_level: None,
                            output_type: Some(OutputType::Status),
                            executed_args: None,
                        });
                    }
                    Err(e) => {
                        return Ok(ActionResult {
                            success: false,
                            output: None,
                            error: Some(format!("Clipboard write failed: {e}")),
                            duration_ms: 0,
                            routed_by: None,
                            open_url: None,
                            needs_confirmation: None,
                            risk_level: None,
                            output_type: None,
                            executed_args: None,
                        });
                    }
                }
            }
        }

        // Default: show clipboard count
        let count = self.store.count(&self.db)?;
        Ok(ActionResult {
            success: true,
            output: Some(format!(
                "{count} clipboard entries. Type 'clip' and browse, or 'clip clear' to erase."
            )),
            error: None,
            duration_ms: 0,
            routed_by: None,
            open_url: None,
            needs_confirmation: None,
            risk_level: None,
            output_type: Some(OutputType::Status),
            executed_args: None,
        })
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let partial = partial.trim();
        let entries = match self.store.get_entries(&self.db, 20) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        if entries.is_empty() {
            return vec![CompletionItem {
                label: "No clipboard history".to_string(),
                icon_path: None,
                score: 0,
                description: None,
            }];
        }

        // If partial is empty or "clear", show entries as-is (most recent first)
        if partial.is_empty() {
            return entries
                .iter()
                .enumerate()
                .map(|(i, entry)| CompletionItem {
                    label: Self::truncate_label(&entry.text, 80),
                    icon_path: None,
                    score: (1000 - i as u16).max(1),
                    description: Some(Self::format_age(entry.created_at)),
                })
                .collect();
        }

        // Fuzzy search through entries
        let lower_partial = partial.to_lowercase();
        let mut scored: Vec<(usize, &crate::clipboard::ClipboardItem)> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.text.to_lowercase().contains(&lower_partial))
            .collect();
        scored.sort_by(|(a, _), (b, _)| a.cmp(b)); // Keep recency order

        scored
            .iter()
            .enumerate()
            .map(|(rank, (_, entry))| CompletionItem {
                label: Self::truncate_label(&entry.text, 80),
                icon_path: None,
                score: (1000 - rank as u16).max(1),
                description: Some(Self::format_age(entry.created_at)),
            })
            .collect()
    }
}

/// Write text to the system clipboard in a way that persists after the call returns.
/// On Wayland, uses `wl-copy` (daemonizes, content survives process exit).
/// On X11, uses arboard directly.
pub(crate) fn write_to_clipboard(text: &str) -> Result<(), arboard::Error> {
    // On Wayland, wl-copy is the reliable way — it forks a background process
    // that serves the clipboard content until another app replaces it.
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("wl-copy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| arboard::Error::Unknown {
                description: format!("wl-copy: {e}"),
            })?;
        if let Some(ref mut stdin) = child.stdin {
            stdin
                .write_all(text.as_bytes())
                .map_err(|e| arboard::Error::Unknown {
                    description: format!("wl-copy stdin: {e}"),
                })?;
        }
        // Don't wait — wl-copy daemonizes itself
        drop(child);
        Ok(())
    } else {
        let mut cb = arboard::Clipboard::new()?;
        cb.set_text(text)
    }
}
