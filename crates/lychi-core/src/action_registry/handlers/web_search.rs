use async_trait::async_trait;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem, ExecContext};
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
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["web"])];
        TRIGGERS
    }

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
        vec![
            CompletionItem::new(format!("Search web: {query}"), Some("__none__".into()), 100)
                .with_run(format!("web {query}"))
                .with_description("Enter · Ctrl+Enter"),
        ]
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let query = args.trim();
        if query.is_empty() {
            return Ok(ActionResult::err("Usage: web <search query>".to_string()));
        }

        let url = format!("{}{}", self.search_url, urlencoding::encode(query));

        // Pure navigation: opening the browser IS the result.
        Ok(ActionResult::navigate(url, true))
    }
}
