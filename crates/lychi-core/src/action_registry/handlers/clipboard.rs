use std::sync::Arc;

use async_trait::async_trait;
use redb::Database;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
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
        crate::text::truncate_first_line(text, max_len)
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
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["clip", "clipboard"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "clip"
    }

    fn description(&self) -> &str {
        "Browse and paste from clipboard history"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let args = args.trim();

        // "clear" subcommand
        if args == "clear" {
            self.store.clear(&self.db)?;
            return Ok(ActionResult::ok(
                "Clipboard history cleared",
                OutputType::Status,
            ));
        }

        // Selection from completions — find matching entry and write it back to clipboard
        if !args.is_empty() {
            let entries = self.store.get_entries(&self.db, 100)?;
            // Try UUID match first, then text prefix match (completions show truncated text)
            if let Some(entry) = entries.iter().find(|e| e.id == args).or_else(|| {
                entries.iter().find(|e| {
                    e.text.starts_with(args) || args.starts_with(&Self::truncate_label(&e.text, 80))
                })
            }) {
                // Image entry — re-paste the image file
                if let Some(ref img) = entry.image {
                    if let Some(path) = self.store.get_image_path(&self.db, &entry.id)? {
                        return match write_image_to_clipboard(&path) {
                            Ok(()) => Ok(ActionResult::ok(
                                format!("Copied image ({}x{})", img.width, img.height),
                                OutputType::Status,
                            )),
                            Err(e) => Ok(ActionResult::err(format!(
                                "Image clipboard write failed: {e}"
                            ))),
                        };
                    }
                    return Ok(ActionResult::err("Image file not found"));
                }

                // Text entry
                return match write_to_clipboard(&entry.text) {
                    Ok(()) => {
                        let preview = Self::truncate_label(&entry.text, 60);
                        Ok(ActionResult::ok(
                            format!("Copied: {preview}"),
                            OutputType::Status,
                        ))
                    }
                    Err(e) => Ok(ActionResult::err(format!("Clipboard write failed: {e}"))),
                };
            }
        }

        // Default: show clipboard count
        let count = self.store.count(&self.db)?;
        Ok(ActionResult::ok(
            format!("{count} clipboard entries. Type 'clip' and browse, or 'clip clear' to erase."),
            OutputType::Status,
        ))
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
                reason: None,
                thumb_b64: None,
                // Nothing to copy — selecting just shows clipboard status.
                run: Some("clip".to_string()),
                ..Default::default()
            }];
        }

        // If partial is empty, show entries as-is (most recent first)
        if partial.is_empty() {
            return entries
                .iter()
                .enumerate()
                .map(|(i, entry)| {
                    let is_image = entry.image.is_some();
                    CompletionItem {
                        label: Self::truncate_label(&entry.text, 80),
                        icon_path: if is_image {
                            Some("__clipboard_image__".into())
                        } else {
                            None
                        },
                        score: (1000 - i as u16).max(1),
                        description: Some(Self::format_age(entry.created_at)),
                        reason: None,
                        thumb_b64: entry.image.as_ref().map(|img| img.thumb_b64.clone()),
                        run: Some(format!("clip {}", entry.id)),
                        ..Default::default()
                    }
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
        scored.sort_by_key(|(a, _)| *a); // Keep recency order

        scored
            .iter()
            .enumerate()
            .map(|(rank, (_, entry))| {
                let is_image = entry.image.is_some();
                CompletionItem {
                    label: Self::truncate_label(&entry.text, 80),
                    icon_path: if is_image {
                        Some("__clipboard_image__".into())
                    } else {
                        None
                    },
                    score: (1000 - rank as u16).max(1),
                    description: Some(Self::format_age(entry.created_at)),
                    reason: None,
                    thumb_b64: entry.image.as_ref().map(|img| img.thumb_b64.clone()),
                    run: Some(format!("clip {}", entry.id)),
                    ..Default::default()
                }
            })
            .collect()
    }
}

/// Write text to the system clipboard in a way that persists after the call returns.
/// On Wayland, uses `wl-copy` (daemonizes, content survives process exit).
/// On X11, uses arboard directly.
pub(crate) fn write_to_clipboard(text: &str) -> Result<(), arboard::Error> {
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
        drop(child);
        Ok(())
    } else {
        let mut cb = arboard::Clipboard::new()?;
        cb.set_text(text)
    }
}

/// Write an image file to the system clipboard.
/// On Wayland, uses `wl-copy --type image/png`.
/// On X11, uses arboard `set_image()`.
fn write_image_to_clipboard(path: &str) -> Result<(), arboard::Error> {
    let png_bytes = std::fs::read(path).map_err(|e| arboard::Error::Unknown {
        description: format!("read image: {e}"),
    })?;

    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("wl-copy")
            .args(["--type", "image/png"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| arboard::Error::Unknown {
                description: format!("wl-copy: {e}"),
            })?;
        if let Some(ref mut stdin) = child.stdin {
            stdin
                .write_all(&png_bytes)
                .map_err(|e| arboard::Error::Unknown {
                    description: format!("wl-copy stdin: {e}"),
                })?;
        }
        drop(child);
        Ok(())
    } else {
        // X11: decode PNG to RGBA, then use arboard
        let (rgba, w, h) =
            crate::clipboard::image_utils::decode_png_to_rgba(&png_bytes).map_err(|e| {
                arboard::Error::Unknown {
                    description: format!("PNG decode: {e}"),
                }
            })?;
        let mut cb = arboard::Clipboard::new()?;
        let img = arboard::ImageData {
            width: w as usize,
            height: h as usize,
            bytes: std::borrow::Cow::Owned(rgba),
        };
        cb.set_image(img)
    }
}
