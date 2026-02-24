use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;

use redb::Database;

use crate::action_registry::{ActionHandler, ActionResult};
use crate::db::frecency;
use crate::error::LychiError;

pub struct FileOpen {
    db: Arc<Database>,
}

impl FileOpen {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
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
impl ActionHandler for FileOpen {
    fn id(&self) -> &str {
        "file"
    }

    fn description(&self) -> &str {
        "Open a file or folder in the default application"
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let path_str = args.trim();
        if path_str.is_empty() {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some("Usage: file <path> or type a path starting with / or ~/".to_string()),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            });
        }

        let expanded = Self::expand_path(path_str);

        if !expanded.exists() {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some(format!("Path not found: {}", expanded.display())),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            });
        }

        // Record frecency access
        let _ = frecency::record(&self.db, &expanded.display().to_string());

        // Convert to file:// URI for GDK-based opening on the frontend
        let file_uri = format!("file://{}", expanded.display());

        Ok(ActionResult {
            success: true,
            output: Some(format!("Opened {}", expanded.display())),
            error: None,
            duration_ms: 0,
            routed_by: None,
            open_url: Some(file_uri),
            needs_confirmation: None,
            risk_level: None,
            output_type: None,
            executed_args: None,
        })
    }
}
