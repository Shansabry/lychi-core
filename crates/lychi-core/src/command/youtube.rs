use async_trait::async_trait;

use crate::command::{CommandHandler, CommandResult};
use crate::error::LychiError;

const YOUTUBE_SEARCH_URL: &str = "https://www.youtube.com/results?search_query=";

pub struct YouTube;

impl Default for YouTube {
    fn default() -> Self {
        Self::new()
    }
}

impl YouTube {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CommandHandler for YouTube {
    fn prefix(&self) -> &str {
        "yt"
    }

    fn description(&self) -> &str {
        "Search YouTube in your default browser"
    }

    async fn execute(&self, args: &str) -> Result<CommandResult, LychiError> {
        let query = args.trim();
        if query.is_empty() {
            return Ok(CommandResult {
                success: false,
                output: None,
                error: Some("Usage: yt <search query>".to_string()),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
            });
        }

        let url = format!("{}{}", YOUTUBE_SEARCH_URL, urlencoding::encode(query));

        Ok(CommandResult {
            success: true,
            output: Some(format!("YouTube: {query}")),
            error: None,
            duration_ms: 0,
            routed_by: None,
            open_url: Some(url),
        })
    }
}
