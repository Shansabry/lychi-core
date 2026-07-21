use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::action_registry::{ActionHandler, ActionResult, ExecContext, OutputType};
use crate::error::LychiError;
use crate::providers::AiProvider;

const ASK_SYSTEM_PROMPT: &str = r#"You are a concise knowledge assistant in a desktop launcher. Answer the user's question in 2-3 sentences maximum. Be direct and factual. Do not use markdown formatting. If you're unsure, say so briefly."#;

/// Fallback answer timeout when none is provided (matches the old constant).
const DEFAULT_ASK_TIMEOUT: Duration = Duration::from_secs(10);

pub struct AskHandler {
    ai_provider: Option<Arc<dyn AiProvider>>,
    search_url: String,
    timeout: Duration,
}

impl AskHandler {
    pub fn new(ai_provider: Option<Arc<dyn AiProvider>>, search_url: String) -> Self {
        Self::with_timeout(ai_provider, search_url, DEFAULT_ASK_TIMEOUT)
    }

    pub fn with_timeout(
        ai_provider: Option<Arc<dyn AiProvider>>,
        search_url: String,
        timeout: Duration,
    ) -> Self {
        Self {
            ai_provider,
            search_url,
            timeout,
        }
    }
}

#[async_trait]
impl ActionHandler for AskHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["ask"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "ask"
    }

    // A long AI call; if the user retypes and re-runs, the newer answer wins (G4).
    fn execution_mode(&self) -> crate::action_registry::ExecutionMode {
        crate::action_registry::ExecutionMode::ReplacePrevious
    }

    fn description(&self) -> &str {
        "Ask a question and get an AI-powered answer inline"
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let query = args.trim();
        if query.is_empty() {
            return Ok(ActionResult::err("Usage: ask <question>".to_string()));
        }

        let search_url = format!("{}{}", self.search_url, urlencoding::encode(query));

        // A pure web-search result: opens the browser immediately, no card.
        // Used whenever AI can't answer (not configured, error, timeout) — no
        // apology, identical to typing `web {query}`. Sab's rule: no AI →
        // rely on keywords/shortcuts (here: web).
        let web_search = || ActionResult::navigate(search_url.clone(), true);

        // No AI configured → silent web search.
        let Some(provider) = &self.ai_provider else {
            return Ok(web_search());
        };

        let start = Instant::now();
        let answer = tokio::time::timeout(
            self.timeout,
            provider.answer_question(ASK_SYSTEM_PROMPT, query),
        )
        .await;
        let duration = start.elapsed().as_millis() as u64;

        match answer {
            // Answer succeeded → show it. `open_url` stays set so the answer
            // card offers a one-key "search the web" escape hatch (via the
            // user's open_inline_url binding). auto_open stays false — the
            // answer IS the value, don't jump to the browser.
            Ok(Ok(text)) => Ok(ActionResult::ok(text, OutputType::Text)
                .with_link(search_url)
                .with_duration(duration)),
            // Provider error or timeout → transparent web search. Log why;
            // the user gets their search immediately rather than an apology.
            Ok(Err(e)) => {
                tracing::warn!("[ask] AI failed ({e}) — searching the web instead");
                Ok(web_search())
            }
            Err(_) => {
                tracing::warn!(
                    "[ask] AI timed out after {:?} — searching the web instead",
                    self.timeout
                );
                Ok(web_search())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_registry::Output;

    const SEARCH: &str = "https://www.google.com/search?q=";

    #[tokio::test]
    async fn no_ai_silently_web_searches() {
        let handler = AskHandler::new(None, SEARCH.into());
        let r = handler
            .execute(
                &crate::action_registry::ExecContext::default(),
                "what is a monad",
            )
            .await
            .unwrap();
        // Pure navigation: opens the browser, no apology card.
        match r.output {
            Output::Navigate { url, auto_open } => {
                assert!(auto_open, "should auto-open the web search");
                assert_eq!(url, "https://www.google.com/search?q=what%20is%20a%20monad");
            }
            other => panic!("expected navigate output, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_query_is_usage_error() {
        let handler = AskHandler::new(None, SEARCH.into());
        let r = handler
            .execute(&crate::action_registry::ExecContext::default(), "   ")
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.error.is_some());
    }
}
