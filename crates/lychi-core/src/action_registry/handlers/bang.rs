//! Custom search-engine shortcuts ("bangs") — the no-code ecosystem primitive.
//!
//! A user maps a keyword to a URL template in `config.toml`:
//! ```toml
//! [commands.search_engines]
//! gh = "https://github.com/search?q="
//! npm = "https://www.npmjs.com/search?q="
//! ```
//! Then `gh tokio` opens a GitHub search. The query is URL-encoded and either
//! substituted for `{}` in the template (if present) or appended. Fully
//! user-extensible — this is how a user grows Lychi without writing code.

use std::collections::HashMap;

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext,
};
use crate::error::LychiError;

pub struct BangHandler {
    /// keyword → URL template.
    engines: HashMap<String, String>,
}

impl BangHandler {
    pub fn new(engines: HashMap<String, String>) -> Self {
        Self { engines }
    }

    /// The configured keywords (lowercased), for the router to recognise.
    pub fn keywords(&self) -> Vec<String> {
        self.engines.keys().map(|k| k.to_lowercase()).collect()
    }

    /// Build the target URL for `keyword query`, or `None` if the keyword isn't
    /// a configured engine. `{}` in the template is replaced by the encoded
    /// query; otherwise the encoded query is appended.
    fn resolve(&self, keyword: &str, query: &str) -> Option<String> {
        let template = self
            .engines
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(keyword))
            .map(|(_, v)| v.clone())?;
        let encoded = urlencoding::encode(query.trim());
        Some(if template.contains("{}") {
            template.replace("{}", &encoded)
        } else {
            format!("{template}{encoded}")
        })
    }
}

#[async_trait]
impl ActionHandler for BangHandler {
    fn id(&self) -> &str {
        "bang"
    }

    fn description(&self) -> &str {
        "Custom search-engine shortcuts (gh, npm, mdn, …)"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Web
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let partial = partial.trim();
        // `partial` is the args AFTER the keyword (the router strips the keyword),
        // so here we can only preview the pending query generically. The keyword
        // itself is echoed by the router's completion path.
        if partial.is_empty() {
            return Vec::new();
        }
        Vec::new()
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // args arrive as "keyword rest of query".
        let (keyword, query) = match args.trim().split_once(char::is_whitespace) {
            Some((k, q)) => (k, q.trim()),
            None => (args.trim(), ""),
        };
        let Some(url) = self.resolve(keyword, query) else {
            return Ok(ActionResult::err(format!(
                "Unknown search shortcut: {keyword}"
            )));
        };
        Ok(ActionResult::navigate(url, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_registry::Output;

    fn handler() -> BangHandler {
        BangHandler::new(
            [
                ("gh", "https://github.com/search?q="),
                ("map", "https://maps.google.com/?q={}&z=10"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        )
    }

    #[test]
    fn appends_when_no_placeholder() {
        let u = handler().resolve("gh", "tokio runtime").unwrap();
        assert_eq!(u, "https://github.com/search?q=tokio%20runtime");
    }

    #[test]
    fn substitutes_placeholder() {
        let u = handler().resolve("map", "eiffel tower").unwrap();
        assert_eq!(u, "https://maps.google.com/?q=eiffel%20tower&z=10");
    }

    #[test]
    fn case_insensitive_keyword() {
        assert!(handler().resolve("GH", "x").is_some());
    }

    #[test]
    fn unknown_keyword_is_none() {
        assert!(handler().resolve("zzz", "x").is_none());
    }

    #[tokio::test]
    async fn execute_opens_url() {
        let r = handler()
            .execute(&crate::action_registry::ExecContext::default(), "gh rust")
            .await
            .unwrap();
        assert!(r.success);
        match r.output {
            Output::Navigate { url, auto_open } => {
                assert!(auto_open);
                assert_eq!(url, "https://github.com/search?q=rust");
            }
            other => panic!("expected navigate output, got {other:?}"),
        }
    }
}
