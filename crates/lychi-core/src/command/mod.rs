pub mod app_launcher;
pub mod calc;
pub mod file_open;
pub mod icons;
pub mod parser;
pub mod project_open;
pub mod registry;
pub mod shell_exec;
pub mod spotify;
pub mod system;
pub mod url_open;
pub mod web_search;
pub mod youtube;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::LychiError;

#[derive(Debug, Clone)]
pub struct CommandInput {
    pub prefix: String,
    pub args: String,
    pub raw: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub success: bool,
    pub output: Option<String>,
    pub error: Option<String>,
    pub duration_ms: u64,
    /// Set to Some("ai") when AI routed this command (for ✦ indicator).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routed_by: Option<String>,
    /// If set, the frontend should open this URL (using GDK for proper Wayland focus).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionItem {
    pub label: String,
    pub icon_path: Option<String>,
    pub score: u16,
}

#[async_trait]
pub trait CommandHandler: Send + Sync {
    /// The prefix this handler responds to (e.g., "open", "web", "run").
    fn prefix(&self) -> &str;

    /// Human-readable description for help/discovery.
    fn description(&self) -> &str;

    /// Execute the command with the given arguments.
    async fn execute(&self, args: &str) -> Result<CommandResult, LychiError>;

    /// Provide completions for partial input. Default: empty.
    async fn completions(&self, _partial: &str) -> Vec<CompletionItem> {
        Vec::new()
    }
}
