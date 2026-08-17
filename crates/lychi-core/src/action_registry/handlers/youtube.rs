use async_trait::async_trait;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext,
};
use crate::error::LychiError;

const YOUTUBE_SEARCH_URL: &str = "https://www.youtube.com/results?search_query=";

/// `yt`'s argument surface: a single free-form action whose flat form IS the
/// search phrase. The JSON Schema and the structured→flat adapter both derive
/// from this.
const YT_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Search YouTube in the user's default browser — opens the results \
               page for the query. Use when the user wants a video, music, a \
               channel, or anything else on YouTube specifically; use the general \
               web search otherwise. Opening the browser IS the result: no text \
               comes back, and nothing is stored or changed.",
        mutates: false,
        operands: &[Operand {
            name: "query",
            desc: "The YouTube search phrase, verbatim — it is URL-encoded \
                   automatically. Required: there is no empty search.",
            required: true,
            kind: ArgKind::Text,
            prefix: None,
        }],
    }],
};

/// Normalize the tool's `args` to the flat query string `execute` reads. A
/// constrained model sends the structured JSON (`{"query":"lofi"}`); a human
/// or legacy/flat caller sends the phrase directly and passes through
/// unchanged. Malformed JSON falls back to the raw string.
fn yt_args_to_flat(args: &str) -> String {
    YT_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
}

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
    fn grammar(&self) -> Option<Grammar> {
        Some(YT_GRAMMAR)
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
        // A constrained model sends `{"query":..}`; flatten it (a plain-string
        // caller passes through) to the bare phrase.
        let flat = yt_args_to_flat(args);
        let query = flat.trim();
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

    #[test]
    fn yt_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; it flattens to the bare
        // phrase `execute` reads.
        assert_eq!(
            yt_args_to_flat(r#"{"query":"lofi hip hop"}"#),
            "lofi hip hop"
        );
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(yt_args_to_flat("lofi hip hop"), "lofi hip hop");
        // Malformed JSON falls back to the raw string.
        assert_eq!(yt_args_to_flat("{not json"), "{not json");
    }

    /// Drift guard: the grammar's flat rendering must be accepted end-to-end by
    /// the parser — a structured call searches exactly like the flat form.
    #[tokio::test]
    async fn structured_call_searches_like_the_flat_form() {
        use crate::action_registry::Output;
        let r = YouTube::new()
            .execute(&ExecContext::default(), r#"{"query":"lofi hip hop"}"#)
            .await
            .unwrap();
        match r.output {
            Output::Navigate { url, auto_open } => {
                assert_eq!(url, format!("{YOUTUBE_SEARCH_URL}lofi%20hip%20hop"));
                assert!(auto_open);
            }
            other => panic!("expected navigate, got {other:?}"),
        }
    }
}
