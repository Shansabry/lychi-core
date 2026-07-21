use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;
use crate::providers::AiProvider;

const TRANSFORM_TIMEOUT: Duration = Duration::from_secs(15);

pub struct ClipboardTransformHandler {
    ai_provider: Option<Arc<dyn AiProvider>>,
}

impl ClipboardTransformHandler {
    pub fn new(ai_provider: Option<Arc<dyn AiProvider>>) -> Self {
        Self { ai_provider }
    }
}

#[async_trait]
impl ActionHandler for ClipboardTransformHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::{ArgTransform, Trigger};
        static TRIGGERS: &[Trigger] = &[
            Trigger::new(&["summarize"], ArgTransform::Prepend("summarize")),
            Trigger::new(&["rewrite"], ArgTransform::Prepend("rewrite")),
        ];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "clipboard_transform"
    }

    fn execution_mode(&self) -> crate::action_registry::ExecutionMode {
        crate::action_registry::ExecutionMode::ReplacePrevious
    }

    fn description(&self) -> &str {
        "AI-powered clipboard transformations (summarize, rewrite, convert, translate)"
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let args = args.trim();

        // Parse subcommand
        let (subcommand, sub_args) = match args.split_once(' ') {
            Some((cmd, rest)) => (cmd.to_lowercase(), rest.trim().to_string()),
            None => (args.to_lowercase(), String::new()),
        };

        // Strip optional "clipboard" word from sub_args
        let sub_args = sub_args
            .strip_prefix("clipboard ")
            .or_else(|| sub_args.strip_prefix("clipboard"))
            .unwrap_or(&sub_args)
            .trim()
            .to_string();

        // `summarize`/`rewrite` operate on arbitrary text, so they accept the
        // text INLINE (paste a paragraph after the verb) and fall back to the
        // clipboard when none is given. `inline_source` holds that inline text
        // (empty = use clipboard). `convert`/`translate` treat their args as the
        // target format/language, so they always read the clipboard.
        let mut inline_source = String::new();

        let system_prompt = match subcommand.as_str() {
            "summarize" => {
                // Everything after "summarize" is the text to summarize.
                inline_source = sub_args.clone();
                "Summarize the following text concisely in 2-3 sentences. Return only the summary, no preamble.".to_string()
            }
            "rewrite" => {
                // `rewrite to <tone> <text>` or `rewrite <text>`. If the args
                // start with "to <tone>", split the tone from the inline text;
                // otherwise the whole thing is inline text at the default tone.
                let (tone, text) = if let Some(rest) = sub_args.strip_prefix("to ") {
                    match rest.split_once(' ') {
                        Some((t, body)) => (t.to_string(), body.trim().to_string()),
                        None => (rest.to_string(), String::new()),
                    }
                } else {
                    ("professional".to_string(), sub_args.clone())
                };
                inline_source = text;
                let tone = if tone.is_empty() {
                    "professional".to_string()
                } else {
                    tone
                };
                format!(
                    "Rewrite the following text in a {tone} tone. Return only the rewritten text, no preamble."
                )
            }
            "convert" => {
                let format = sub_args.strip_prefix("to ").unwrap_or(&sub_args).trim();
                if format.is_empty() {
                    return Ok(ActionResult::err(
                        "Usage: convert clipboard to <format> (json, markdown, csv, yaml, etc.)",
                    ));
                }
                format!(
                    "Convert the following text to {format} format. Return only the converted output, no explanation."
                )
            }
            "translate" => {
                let language = sub_args.strip_prefix("to ").unwrap_or(&sub_args).trim();
                if language.is_empty() {
                    return Ok(ActionResult::err(
                        "Usage: translate clipboard to <language>",
                    ));
                }
                format!(
                    "Translate the following text to {language}. Return only the translation, no preamble."
                )
            }
            _ => {
                return Ok(ActionResult::err(
                    "Unknown transform. Try: summarize, rewrite, convert, translate",
                ));
            }
        };

        // Source text: inline text (typed/pasted after the verb) wins; otherwise
        // fall back to the clipboard. So `summarize <paragraph>` summarizes what
        // you typed, while a bare `summarize` still summarizes the clipboard.
        let source_text = if !inline_source.trim().is_empty() {
            inline_source
        } else {
            match crate::context::clipboard_detect::read_clipboard() {
                Some(text) if !text.trim().is_empty() => text,
                _ => {
                    return Ok(ActionResult::err(
                        "Nothing to transform — type text after the command, or copy something first",
                    ));
                }
            }
        };

        // Check AI provider
        let Some(provider) = &self.ai_provider else {
            return Ok(ActionResult::err(
                "AI not configured — set up a provider in Settings > AI",
            ));
        };

        let start = Instant::now();
        let result = tokio::time::timeout(
            TRANSFORM_TIMEOUT,
            provider.answer_question(&system_prompt, &source_text),
        )
        .await;
        let duration = start.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(transformed)) => {
                // Write result back to clipboard
                if let Err(e) = super::clipboard::write_to_clipboard(&transformed) {
                    return Ok(ActionResult::err(format!(
                        "Transform succeeded but clipboard write failed: {e}"
                    )));
                }

                Ok(ActionResult::ok(transformed, OutputType::Text).with_duration(duration))
            }
            Ok(Err(e)) => {
                tracing::warn!("[clipboard_transform] AI failed: {e}");
                Ok(ActionResult::err(format!("AI transform failed: {e}")))
            }
            Err(_) => {
                tracing::warn!("[clipboard_transform] AI timed out after {TRANSFORM_TIMEOUT:?}");
                Ok(ActionResult::err("AI transform timed out"))
            }
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let partial = partial.trim().to_lowercase();

        let all = vec![
            ("summarize clipboard", "Summarize clipboard content"),
            ("rewrite clipboard formal", "Rewrite in formal tone"),
            ("rewrite clipboard casual", "Rewrite in casual tone"),
            ("rewrite clipboard concise", "Rewrite concisely"),
            ("convert clipboard to json", "Convert to JSON"),
            ("convert clipboard to markdown", "Convert to Markdown"),
            ("convert clipboard to csv", "Convert to CSV"),
            ("translate clipboard to spanish", "Translate to Spanish"),
            ("translate clipboard to french", "Translate to French"),
            ("translate clipboard to german", "Translate to German"),
        ];

        let filtered: Vec<_> = if partial.is_empty() {
            all.iter().collect()
        } else {
            all.iter()
                .filter(|(label, _)| label.contains(&partial))
                .collect()
        };

        filtered
            .iter()
            .enumerate()
            .map(|(i, (label, desc))| CompletionItem {
                label: label.to_string(),
                icon_path: None,
                score: (1000 - i as u16).max(1),
                description: Some(desc.to_string()),
                reason: None,
                thumb_b64: None,
                run: Some(format!("clipboard_transform {label}")),
                ..Default::default()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn completions_carry_prefixed_run_command() {
        let items = ClipboardTransformHandler::new(None).completions("").await;
        assert!(!items.is_empty());
        // The handler id ("clipboard_transform") differs from every label's
        // first word ("summarize"/"convert"/…), so `run` must carry the id
        // prefix or the executor would misroute.
        for item in &items {
            let run = item.run.as_deref().expect("every item has a run command");
            assert!(
                run.starts_with("clipboard_transform "),
                "run must be prefixed with the handler id: {run}"
            );
            assert!(run.ends_with(&item.label), "run must carry the label args");
        }
    }

    #[test]
    fn test_subcommand_parsing() {
        // Verify the parsing logic works correctly
        let input = "summarize clipboard";
        let (cmd, rest) = input.split_once(' ').unwrap();
        assert_eq!(cmd, "summarize");
        assert_eq!(rest.trim(), "clipboard");
    }

    #[test]
    fn test_strip_clipboard_prefix() {
        let args = "clipboard to json";
        let stripped = args.strip_prefix("clipboard ").unwrap_or(args).trim();
        assert_eq!(stripped, "to json");
    }

    #[test]
    fn test_strip_to_prefix() {
        let sub_args = "to spanish";
        let language = sub_args.strip_prefix("to ").unwrap_or(sub_args).trim();
        assert_eq!(language, "spanish");
    }

    // Inline-text extraction: the source text resolution that lets a user paste
    // a paragraph after `summarize`/`rewrite` instead of relying on the clipboard.
    // Mirrors the parsing in `execute` (summarize: all args are the text; rewrite:
    // optional "to <tone>" then the text).

    /// Reproduce the summarize/rewrite inline-source parse from `execute`.
    fn parse_inline(subcommand: &str, sub_args: &str) -> (String, String) {
        match subcommand {
            "summarize" => (String::new(), sub_args.to_string()),
            "rewrite" => {
                let (tone, text) = if let Some(rest) = sub_args.strip_prefix("to ") {
                    match rest.split_once(' ') {
                        Some((t, body)) => (t.to_string(), body.trim().to_string()),
                        None => (rest.to_string(), String::new()),
                    }
                } else {
                    ("professional".to_string(), sub_args.to_string())
                };
                (tone, text)
            }
            _ => (String::new(), String::new()),
        }
    }

    #[test]
    fn summarize_uses_all_args_as_inline_text() {
        let (_, text) = parse_inline("summarize", "the quick brown fox jumped over the lazy dog");
        assert_eq!(text, "the quick brown fox jumped over the lazy dog");
    }

    #[test]
    fn summarize_empty_args_falls_back_to_clipboard() {
        // Empty inline text → execute() reads the clipboard instead.
        let (_, text) = parse_inline("summarize", "");
        assert!(text.trim().is_empty());
    }

    #[test]
    fn rewrite_splits_tone_from_inline_text() {
        let (tone, text) = parse_inline("rewrite", "to casual hello there sir");
        assert_eq!(tone, "casual");
        assert_eq!(text, "hello there sir");
    }

    #[test]
    fn rewrite_without_tone_is_all_text_default_tone() {
        let (tone, text) = parse_inline("rewrite", "make this nicer please");
        assert_eq!(tone, "professional");
        assert_eq!(text, "make this nicer please");
    }
}
