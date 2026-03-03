use async_trait::async_trait;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem};
use crate::error::LychiError;

const DEFAULT_SEARCH_URL: &str = "https://www.google.com/search?q=";

pub struct WebSearch {
    search_url: String,
}

impl Default for WebSearch {
    fn default() -> Self {
        Self::new()
    }
}

impl WebSearch {
    pub fn new() -> Self {
        Self {
            search_url: DEFAULT_SEARCH_URL.to_string(),
        }
    }

    pub fn with_search_url(search_url: String) -> Self {
        Self { search_url }
    }
}

#[async_trait]
impl ActionHandler for WebSearch {
    fn id(&self) -> &str {
        "web"
    }

    fn description(&self) -> &str {
        "Search the web in your default browser"
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let query = partial.trim();
        if query.is_empty() {
            return Vec::new();
        }
        vec![CompletionItem {
            label: format!("Search web: {query}"),
            icon_path: Some("__none__".to_string()),
            score: 100,
            description: Some("Enter · Ctrl+Enter".to_string()),
            reason: None,
        }]
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let query = args.trim();
        if query.is_empty() {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some("Usage: web <search query>".to_string()),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
                launch_desktop: None,
                focus_app: None,
            });
        }

        let url = format!("{}{}", self.search_url, urlencoding::encode(query));

        Ok(ActionResult {
            success: true,
            output: None,
            error: None,
            duration_ms: 0,
            routed_by: None,
            open_url: Some(url),
            needs_confirmation: None,
            risk_level: None,
            output_type: None,
            executed_args: None,
            launch_desktop: None,
            focus_app: None,
        })
    }
}
