use async_trait::async_trait;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext,
};
use crate::error::LychiError;

const DEFAULT_SEARCH_URL: &str = "https://www.google.com/search?q=";

/// `web`'s argument surface: a single free-form action whose flat form IS the
/// search phrase. The JSON Schema and the structured→flat adapter both derive
/// from this.
const WEB_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Search the web in the user's default browser — opens the results \
               page for the query. Use for anything to look up online that is not \
               a specific URL. Opening the browser IS the result: no text comes \
               back, and nothing is stored or changed.",
        mutates: false,
        operands: &[Operand {
            name: "query",
            desc: "The search phrase, verbatim — it is URL-encoded automatically, \
                   so spaces and special characters are fine. Required: there is \
                   no empty search.",
            required: true,
            kind: ArgKind::Text,
            prefix: None,
        }],
    }],
};

/// Normalize the tool's `args` to the flat query string `execute` reads. A
/// constrained model sends the structured JSON (`{"query":"rust"}`); a human
/// or legacy/flat caller sends the phrase directly and passes through
/// unchanged. Malformed JSON falls back to the raw string.
fn web_args_to_flat(args: &str) -> String {
    WEB_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
}

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
    fn category(&self) -> CommandCategory {
        CommandCategory::Web
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(WEB_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Web
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
        // A constrained model sends `{"query":..}`; flatten it (a plain-string
        // caller passes through) to the bare phrase.
        let flat = web_args_to_flat(args);
        let query = flat.trim();
        if query.is_empty() {
            return Ok(ActionResult::err("Usage: web <search query>".to_string()));
        }

        let url = format!("{}{}", self.search_url, urlencoding::encode(query));

        // Pure navigation: opening the browser IS the result.
        Ok(ActionResult::navigate(url, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_registry::Output;

    #[test]
    fn web_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; it flattens to the bare
        // phrase `execute` reads.
        assert_eq!(
            web_args_to_flat(r#"{"query":"rust async traits"}"#),
            "rust async traits"
        );
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(web_args_to_flat("rust async traits"), "rust async traits");
        // Malformed JSON falls back to the raw string.
        assert_eq!(web_args_to_flat("{not json"), "{not json");
    }

    /// Drift guard: the grammar's flat rendering must be accepted end-to-end by
    /// the parser — a structured call searches exactly like the flat form.
    #[tokio::test]
    async fn structured_call_searches_like_the_flat_form() {
        let r = WebSearch::new()
            .execute(&ExecContext::default(), r#"{"query":"a b&c"}"#)
            .await
            .unwrap();
        match r.output {
            Output::Navigate { url, auto_open } => {
                assert_eq!(url, format!("{DEFAULT_SEARCH_URL}a%20b%26c"));
                assert!(auto_open);
            }
            other => panic!("expected navigate, got {other:?}"),
        }
    }
}
