use async_trait::async_trait;

use crate::command::{CommandHandler, CommandResult};
use crate::error::LychiError;

pub struct UrlOpen;

impl Default for UrlOpen {
    fn default() -> Self {
        Self::new()
    }
}

impl UrlOpen {
    pub fn new() -> Self {
        Self
    }

    /// Ensure URL has a scheme. Prepend https:// if missing.
    fn normalize_url(url: &str) -> String {
        if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else {
            format!("https://{url}")
        }
    }
}

#[async_trait]
impl CommandHandler for UrlOpen {
    fn prefix(&self) -> &str {
        "url"
    }

    fn description(&self) -> &str {
        "Open a URL in the default browser"
    }

    async fn execute(&self, args: &str) -> Result<CommandResult, LychiError> {
        let url_str = args.trim();
        if url_str.is_empty() {
            return Ok(CommandResult {
                success: false,
                output: None,
                error: Some("Usage: url <address> or type a URL directly".to_string()),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
            });
        }

        let url = Self::normalize_url(url_str);

        Ok(CommandResult {
            success: true,
            output: Some(format!("Opening {url}")),
            error: None,
            duration_ms: 0,
            routed_by: None,
            open_url: Some(url),
        })
    }
}
