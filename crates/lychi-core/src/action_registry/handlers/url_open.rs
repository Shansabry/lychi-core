use async_trait::async_trait;

use crate::action_registry::{ActionHandler, ActionResult, ExecContext};
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
impl ActionHandler for UrlOpen {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["url"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "url"
    }

    fn description(&self) -> &str {
        "Open a URL in the default browser"
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let url_str = args.trim();
        if url_str.is_empty() {
            return Ok(ActionResult::err(
                "Usage: url <address> or type a URL directly".to_string(),
            ));
        }

        let url = Self::normalize_url(url_str);

        Ok(ActionResult::navigate(url, true))
    }
}
