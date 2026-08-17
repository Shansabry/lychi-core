use async_trait::async_trait;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{ActionHandler, ActionResult, CommandCategory, ExecContext};
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

/// `url`'s argument surface: a single free-form action whose flat form IS the
/// address. The JSON Schema and the structured→flat adapter both derive from
/// this.
const URL_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Open a specific web address in the user's default browser. Use when \
               the user names or pastes a URL; for a topic to look up, use the web \
               search action instead. Opening the browser IS the result — nothing \
               comes back, and nothing is stored or changed.",
        mutates: false,
        operands: &[Operand {
            name: "url",
            desc: "The address to open, e.g. \"https://example.com\" or \
                   \"docs.rs/tokio\" — a missing scheme becomes https:// \
                   automatically. Must be a single URL, never a search phrase.",
            required: true,
            kind: ArgKind::Text,
            prefix: None,
        }],
    }],
};

/// Normalize the tool's `args` to the flat URL string `execute` reads. A
/// constrained model sends the structured JSON (`{"url":"example.com"}`); a
/// human or legacy/flat caller sends the address directly and passes through
/// unchanged. Malformed JSON falls back to the raw string.
fn url_args_to_flat(args: &str) -> String {
    URL_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
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
    fn category(&self) -> CommandCategory {
        CommandCategory::Web
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(URL_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Web
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // A constrained model sends `{"url":..}`; flatten it (a plain-string
        // caller passes through) to the bare address.
        let flat = url_args_to_flat(args);
        let url_str = flat.trim();
        if url_str.is_empty() {
            return Ok(ActionResult::err(
                "Usage: url <address> or type a URL directly".to_string(),
            ));
        }

        let url = Self::normalize_url(url_str);

        Ok(ActionResult::navigate(url, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_registry::Output;

    #[test]
    fn url_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; it flattens to the bare
        // address the normalizer reads.
        assert_eq!(url_args_to_flat(r#"{"url":"example.com"}"#), "example.com");
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(url_args_to_flat("https://docs.rs"), "https://docs.rs");
        // Malformed JSON falls back to the raw string.
        assert_eq!(url_args_to_flat("{not json"), "{not json");
    }

    /// Drift guard: the grammar's flat rendering must be accepted end-to-end by
    /// the parser — a structured call navigates exactly like the flat form.
    #[tokio::test]
    async fn structured_call_navigates_like_the_flat_form() {
        let r = UrlOpen::new()
            .execute(&ExecContext::default(), r#"{"url":"example.com"}"#)
            .await
            .unwrap();
        match r.output {
            Output::Navigate { url, auto_open } => {
                assert_eq!(url, "https://example.com");
                assert!(auto_open);
            }
            other => panic!("expected navigate, got {other:?}"),
        }
    }
}
