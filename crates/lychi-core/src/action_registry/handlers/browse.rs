use std::path::PathBuf;

use async_trait::async_trait;

use crate::action_registry::{ActionHandler, ActionResult, ExecContext, OutputType};
use crate::error::LychiError;

#[derive(Default)]
pub struct BrowseHandler;

impl BrowseHandler {
    pub fn new() -> Self {
        Self
    }

    fn expand_path(path: &str) -> PathBuf {
        if let Some(rest) = path.strip_prefix("~/") {
            dirs::home_dir()
                .map(|h| h.join(rest))
                .unwrap_or_else(|| PathBuf::from(path))
        } else if path == "~" {
            dirs::home_dir().unwrap_or_else(|| PathBuf::from(path))
        } else {
            PathBuf::from(path)
        }
    }
}

#[async_trait]
impl ActionHandler for BrowseHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["browse"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "browse"
    }

    fn description(&self) -> &str {
        "Browse files in a directory interactively"
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let path_str = args.trim();
        let dir = if path_str.is_empty() { "~/" } else { path_str };

        let expanded = Self::expand_path(dir);

        if !expanded.exists() || !expanded.is_dir() {
            return Ok(ActionResult::err(format!("Directory not found: {dir}")));
        }

        // Ensure the path ends with / for the frontend @ mode
        let browse_path = if dir.ends_with('/') {
            dir.to_string()
        } else {
            format!("{dir}/")
        };

        Ok(ActionResult::ok(
            format!("__browse_panel__:{browse_path}"),
            OutputType::Status,
        ))
    }
}
