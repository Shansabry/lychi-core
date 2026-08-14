use async_trait::async_trait;
use std::path::PathBuf;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, ExecContext, OutputType,
};
use crate::db::frecency;
use crate::error::LychiError;

#[derive(Default)]
pub struct FileOpen;

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
impl ActionHandler for FileOpen {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["file"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "file"
    }

    fn description(&self) -> &str {
        "Open a file or folder in the default application"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Files
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let path_str = args.trim();
        if path_str.is_empty() {
            return Ok(ActionResult::err(
                "Usage: file <path> or type a path starting with / or ~/".to_string(),
            ));
        }

        let expanded = Self::expand_path(path_str);

        if !expanded.exists() {
            return Ok(ActionResult::err(format!(
                "Path not found: {}",
                expanded.display()
            )));
        }

        // Record frecency access
        let _ = frecency::record(&expanded.display().to_string());

        // Convert to file:// URI for GDK-based opening on the frontend
        let file_uri = format!("file://{}", expanded.display());

        Ok(
            ActionResult::ok(format!("Opened {}", expanded.display()), OutputType::Status)
                .with_link(file_uri),
        )
    }
}
