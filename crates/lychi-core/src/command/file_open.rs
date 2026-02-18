use async_trait::async_trait;
use std::path::PathBuf;

use crate::command::{CommandHandler, CommandResult};
use crate::error::LychiError;

pub struct FileOpen;

impl Default for FileOpen {
    fn default() -> Self {
        Self::new()
    }
}

impl FileOpen {
    pub fn new() -> Self {
        Self
    }

    /// Expand ~ to home directory.
    fn expand_path(path: &str) -> PathBuf {
        if let Some(rest) = path.strip_prefix("~/") {
            dirs::home_dir()
                .map(|h| h.join(rest))
                .unwrap_or_else(|| PathBuf::from(path))
        } else {
            PathBuf::from(path)
        }
    }
}

#[async_trait]
impl CommandHandler for FileOpen {
    fn prefix(&self) -> &str {
        "file"
    }

    fn description(&self) -> &str {
        "Open a file or folder in the default application"
    }

    async fn execute(&self, args: &str) -> Result<CommandResult, LychiError> {
        let path_str = args.trim();
        if path_str.is_empty() {
            return Ok(CommandResult {
                success: false,
                output: None,
                error: Some("Usage: file <path> or type a path starting with / or ~/".to_string()),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
            });
        }

        let expanded = Self::expand_path(path_str);

        if !expanded.exists() {
            return Ok(CommandResult {
                success: false,
                output: None,
                error: Some(format!("Path not found: {}", expanded.display())),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
            });
        }

        // Convert to file:// URI for GDK-based opening on the frontend
        let file_uri = format!("file://{}", expanded.display());

        Ok(CommandResult {
            success: true,
            output: Some(format!("Opened {}", expanded.display())),
            error: None,
            duration_ms: 0,
            routed_by: None,
            open_url: Some(file_uri),
        })
    }
}
