use async_trait::async_trait;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem};
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
impl ActionHandler for YouTube {
    fn id(&self) -> &str {
        "yt"
    }

    fn description(&self) -> &str {
        "Search YouTube in your default browser"
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let query = partial.trim();
        if query.is_empty() {
            return Vec::new();
        }
        vec![CompletionItem {
            label: format!("Search YouTube: {query}"),
            icon_path: Some("__none__".to_string()),
            score: 100,
            description: Some("Enter to search".to_string()),
            reason: None,
        }]
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let query = args.trim();
        if query.is_empty() {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some("Usage: yt <search query>".to_string()),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
                launch_desktop: None,
            });
        }

        let url = format!("{}{}", YOUTUBE_SEARCH_URL, urlencoding::encode(query));

        Ok(ActionResult {
            success: true,
            output: Some(format!("YouTube: {query}")),
            error: None,
            duration_ms: 0,
            routed_by: None,
            open_url: Some(url),
            needs_confirmation: None,
            risk_level: None,
            output_type: None,
            executed_args: None,
            launch_desktop: None,
        })
    }
}
