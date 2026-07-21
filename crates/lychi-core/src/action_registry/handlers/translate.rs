//! Inline translation — `translate <text> to <language>`.
//!
//! Complements the clipboard-scoped `translate clipboard to <lang>`
//! (`clipboard_transform`): this one translates arbitrary inline text and shows
//! the result in the panel WITHOUT touching the clipboard. If the text is
//! omitted or the literal word `clipboard`, it falls back to reading the
//! clipboard, so `translate to spanish` still works on copied text.
//!
//! Uses the configured AI provider (BYO/Ollama/Cloud) — same provider the
//! clipboard transforms use. No provider → a clear "set up AI" message.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;
use crate::providers::AiProvider;

const TRANSLATE_TIMEOUT: Duration = Duration::from_secs(15);

pub struct TranslateHandler {
    ai_provider: Option<Arc<dyn AiProvider>>,
}

impl TranslateHandler {
    pub fn new(ai_provider: Option<Arc<dyn AiProvider>>) -> Self {
        Self { ai_provider }
    }
}

/// Split `<text> to <language>` on the LAST ` to ` so the language is the
/// trailing clause and the text may itself contain " to " (e.g. "I want to go
/// home to spanish"). Returns `(text, language)`. If there's no ` to `, the
/// whole input is treated as text with no language (caller errors).
fn parse_text_and_language(input: &str) -> (String, Option<String>) {
    let trimmed = input.trim();
    let lower = trimmed.to_lowercase();

    // Leading `to <lang>` (no text) → clipboard shorthand: empty text, the rest
    // is the language. Handled first because a leading "to" has no preceding
    // space for the ` to ` split to catch.
    if let Some(rest) = lower.strip_prefix("to ") {
        let language = trimmed[trimmed.len() - rest.len()..].trim().to_string();
        return (
            String::new(),
            if language.is_empty() {
                None
            } else {
                Some(language)
            },
        );
    }

    // Otherwise split on the LAST ` to ` so the language is the trailing clause
    // and the text may itself contain " to ".
    match lower.rfind(" to ") {
        Some(idx) => {
            let text = trimmed[..idx].trim().to_string();
            let language = trimmed[idx + 4..].trim().to_string();
            let language = if language.is_empty() {
                None
            } else {
                Some(language)
            };
            (text, language)
        }
        None => (trimmed.to_string(), None),
    }
}

#[async_trait]
impl ActionHandler for TranslateHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["translate"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "translate"
    }

    fn execution_mode(&self) -> crate::action_registry::ExecutionMode {
        crate::action_registry::ExecutionMode::ReplacePrevious
    }

    fn description(&self) -> &str {
        "Translate text to another language"
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let (mut text, language) = parse_text_and_language(args);

        let Some(language) = language else {
            return Ok(ActionResult::err("Usage: translate <text> to <language>"));
        };

        // Empty text or the literal `clipboard` → translate the clipboard.
        if text.is_empty() || text.eq_ignore_ascii_case("clipboard") {
            match crate::context::clipboard_detect::read_clipboard() {
                Some(t) if !t.trim().is_empty() => text = t,
                _ => {
                    return Ok(ActionResult::err(
                        "Nothing to translate — provide text or copy something first",
                    ));
                }
            }
        }

        let Some(provider) = &self.ai_provider else {
            return Ok(ActionResult::err(
                "AI not configured — set up a provider in Settings > AI",
            ));
        };

        let system_prompt = format!(
            "Translate the following text to {language}. Return only the translation, no preamble, no quotes."
        );

        let start = Instant::now();
        let result = tokio::time::timeout(
            TRANSLATE_TIMEOUT,
            provider.answer_question(&system_prompt, &text),
        )
        .await;
        let duration = start.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(translation)) => {
                Ok(ActionResult::ok(translation, OutputType::Text).with_duration(duration))
            }
            Ok(Err(e)) => {
                tracing::warn!("[translate] AI failed: {e}");
                Ok(ActionResult::err(format!("Translation failed: {e}")))
            }
            Err(_) => {
                tracing::warn!("[translate] AI timed out after {TRANSLATE_TIMEOUT:?}");
                Ok(ActionResult::err("Translation timed out"))
            }
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let p = partial.trim();
        // Once the input already names a language (`… to <lang>`), don't clutter
        // with hint rows — the user knows what they want; Enter translates.
        if parse_text_and_language(p).1.is_some() {
            return Vec::new();
        }
        // Otherwise offer the shape as a hint so the syntax is discoverable.
        vec![
            CompletionItem::new(
                "translate <text> to <language>",
                Some("__info__".into()),
                10,
            )
            .with_description("e.g. translate good morning to spanish"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_and_language() {
        let (t, l) = parse_text_and_language("good morning to spanish");
        assert_eq!(t, "good morning");
        assert_eq!(l.as_deref(), Some("spanish"));
    }

    #[test]
    fn splits_on_last_to() {
        // "to" inside the text must not be mistaken for the language delimiter.
        let (t, l) = parse_text_and_language("I want to go home to french");
        assert_eq!(t, "I want to go home");
        assert_eq!(l.as_deref(), Some("french"));
    }

    #[test]
    fn no_language_when_no_delimiter() {
        let (t, l) = parse_text_and_language("just some text");
        assert_eq!(t, "just some text");
        assert!(l.is_none());
    }

    #[test]
    fn clipboard_shorthand_leaves_empty_text() {
        // `translate to spanish` → empty text (clipboard fallback), lang spanish.
        let (t, l) = parse_text_and_language("to spanish");
        assert_eq!(t, "");
        assert_eq!(l.as_deref(), Some("spanish"));
    }
}
