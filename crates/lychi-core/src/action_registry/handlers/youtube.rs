use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext,
};
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
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["yt"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "yt"
    }

    fn description(&self) -> &str {
        "Search YouTube in your default browser"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Web
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let query = partial.trim();
        if query.is_empty() {
            return Vec::new();
        }
        vec![
            CompletionItem::new(
                format!("Search YouTube: {query}"),
                Some("__none__".into()),
                100,
            )
            .with_run(format!("yt {query}"))
            .with_description("Enter to search"),
        ]
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let query = args.trim();
        if query.is_empty() {
            return Ok(ActionResult::err("Usage: yt <search query>".to_string()));
        }

        let url = format!("{}{}", YOUTUBE_SEARCH_URL, urlencoding::encode(query));

        // Pure navigation: opening the browser IS the result — no card, no
        // secondary keystroke. `auto_open` makes that intent explicit.
        Ok(ActionResult::navigate(url, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn completion_carries_clean_run_command() {
        let items = YouTube::new().completions("lofi hip hop").await;
        assert_eq!(items.len(), 1);
        // Label is the human display text…
        assert_eq!(items[0].label, "Search YouTube: lofi hip hop");
        // …but `run` is the exact command — no label leaks into the query.
        assert_eq!(items[0].run.as_deref(), Some("yt lofi hip hop"));
    }
}
