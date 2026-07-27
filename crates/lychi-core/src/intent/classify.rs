//! Input classification — the SINGLE source of truth for "what does Enter do?".
//!
//! Historically the frontend re-derived this from a hardcoded `KNOWN_PREFIXES`
//! list that drifted out of sync with the real backend handler set. This module
//! makes the backend the only classifier: given a raw string it returns a typed
//! [`RouteDecision`] the frontend actuates verbatim. No keyword list is
//! duplicated across the FE/BE boundary.
//!
//! Scope: this grades the input STRING. Keyboard/mode/selection outcomes
//! (Ctrl/Shift modifiers, `/`-search and `@`-browse selection, calc-preview
//! rows, tab-complete fills) stay in the frontend reducer — they're UI state the
//! core can't see. Everything that depends on knowing "is this a command, and
//! which one / is this a question / a preset / a panel / a typo" lives here.
//!
//! Reuses, never re-implements: [`patterns::route`] (+ the registry prefix index
//! and `COLON_TRIGGERS`), [`typo_suggest::suggest`], and the app-index fallback
//! logic mirrored from [`crate::intent::IntentResolver::resolve`]. The only new
//! tables are the panel-keyword map and the natural-language question grammar —
//! both MOVED here out of the frontend, so the total hardcoded-list count drops.

use crate::action_registry::registry::ActionRegistry;
use crate::intent::patterns::{self, PatternResult};

/// A typed routing decision for a raw input string. Actuated verbatim by the
/// frontend. The `kind` tag maps 1:1 onto the FE dispatch switch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RouteDecision {
    /// A deterministic handler command — run verbatim through the executor.
    Command { command: String },
    /// Natural language → AI. `confident` mirrors the old `isClearQuestion`:
    /// true → straight to the full agent; false → the quick-ai fork card.
    Nl { prompt: String, confident: bool },
    /// An AI preset invocation. When `input` is empty the FE first tries the
    /// PRIMARY selection (highlighted text) as `{input}`.
    Preset { template: String, input: String },
    /// A bare panel keyword (`settings`, `history`, `notes`, …). The FE actuates
    /// via `ui.openPanel`; the core carries no UI knowledge, just the tag.
    Panel {
        name: PanelKind,
        sub_tab: Option<String>,
    },
    /// A typo near-miss ("Did you mean: X?"). `corrected` is filled into the
    /// input for a confirming second Enter — never auto-run.
    Correct { corrected: String },
    /// An AI request with no provider configured. `command` is the web fallback
    /// to run; `explicit` (an `ask …`/preset ask) drives the toast wording.
    AiDisabled { command: String, explicit: bool },
}

/// Which panel a bare keyword opens. Actuated by the frontend `ui` store.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "kebab-case")]
pub enum PanelKind {
    Settings,
    History,
    Media,
    Notes,
    ChatHistory,
}

/// Bare panel keywords → (panel, optional notes sub-tab). The single home for
/// this table (it used to also live in the frontend `PANEL_KEYWORDS`).
fn panel_for(keyword: &str) -> Option<(PanelKind, Option<&'static str>)> {
    let p = match keyword {
        "settings" => (PanelKind::Settings, None),
        "history" => (PanelKind::History, None),
        "chat" | "chats" | "conversations" => (PanelKind::ChatHistory, None),
        "spotify" | "media" | "music" => (PanelKind::Media, None),
        "note" | "notes" => (PanelKind::Notes, Some("notes")),
        "todo" | "todos" => (PanelKind::Notes, Some("todos")),
        "reminder" | "reminders" => (PanelKind::Notes, Some("reminders")),
        "timer" | "timers" | "stopwatch" => (PanelKind::Notes, Some("timers")),
        "snip" | "snippet" | "snippets" => (PanelKind::Notes, Some("snippets")),
        _ => return None,
    };
    Some(p)
}

/// First words that lead a clear question/request. When a query starts with one
/// of these (or ends in `?`), the user is asking, not launching — route straight
/// to the full agent. Bare/ambiguous text keeps the fork card so a stray
/// keystroke or app-name typo never silently spins up an AI turn. (Moved here
/// from the frontend `QUESTION_WORDS`.)
const QUESTION_WORDS: &[&str] = &[
    "what",
    "whats",
    "why",
    "how",
    "who",
    "when",
    "where",
    "which",
    "is",
    "are",
    "can",
    "could",
    "should",
    "would",
    "do",
    "does",
    "did",
    "will",
    "explain",
    "tell",
    "write",
    "give",
    "summarize",
    "translate",
    "define",
    "compare",
];

/// Does `text` read as a clear natural-language question/request (high-confidence
/// AI), vs. ambiguous short text that might be a mistyped command? Clear when it
/// ends in `?`, OR it's multi-word AND its first word is a question/request word.
/// A bare single word is always ambiguous (far likelier a fuzzy app/command match
/// than a one-word question).
pub fn is_clear_question(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if t.ends_with('?') {
        return true;
    }
    let Some((first, _rest)) = t.split_once(char::is_whitespace) else {
        return false; // single token — ambiguous, not a question
    };
    let first = first.to_lowercase();
    QUESTION_WORDS.contains(&first.as_str())
}

/// Classify a raw input string into a [`RouteDecision`], the pure string-grading
/// core. Order mirrors the old frontend decider's precedence for the string
/// cases (preset keyword → panel keyword → deterministic command → typo → NL),
/// but every table now lives on this side.
///
/// `preset_for(keyword)` resolves a user preset by its first-word keyword (the
/// caller injects it so this stays free of DB/IO). `has_ai` gates the NL vs.
/// web-fallback outcome. `explicit_ai` marks an input the user clearly meant for
/// AI (an `ask …` or a preset ask) so an AI-disabled fallback can warn louder.
pub fn classify_string(
    raw: &str,
    registry: &ActionRegistry,
    preset_for: impl Fn(&str) -> Option<(String, String)>,
    has_ai: bool,
) -> RouteDecision {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        // Nothing to classify — caller guards this, but be safe.
        return RouteDecision::Nl {
            prompt: String::new(),
            confident: false,
        };
    }
    let lower = trimmed.to_lowercase();

    // Explicit `ask <q>` → the full agent (or AI-disabled), skipping the fork.
    if lower == "ask" || lower.starts_with("ask ") {
        let q = trimmed[3..].trim();
        if !q.is_empty() {
            return nl_or_disabled(
                q, /* confident */ true, /* explicit */ true, has_ai,
            );
        }
        // bare "ask" falls through to normal handling
    }

    // AI preset invocation: `<keyword> <text>` (or a bare `<keyword>`).
    let first_word = lower.split_whitespace().next().unwrap_or(&lower);
    if let Some((template, _name)) = preset_for(first_word) {
        let rest = trimmed
            .split_once(char::is_whitespace)
            .map(|(_, r)| r.trim().to_string())
            .unwrap_or_default();
        // A preset is an explicit AI ask; when AI is off, warn + fall to web.
        if !has_ai {
            return RouteDecision::AiDisabled {
                command: format!("web {rest}"),
                explicit: true,
            };
        }
        return RouteDecision::Preset {
            template,
            input: rest,
        };
    }

    // Bare panel keyword.
    if let Some((name, sub_tab)) = panel_for(&lower) {
        return RouteDecision::Panel {
            name,
            sub_tab: sub_tab.map(str::to_string),
        };
    }

    // Deterministic command? Ask the ONE backend pattern router. Explicit and
    // Strong matches are commands; a Weak match is a soft hint we still run
    // (mirrors resolve()'s weak-fallback — there is no second AI router now).
    match patterns::route(trimmed, registry) {
        PatternResult::Match(route) => {
            let _ = route; // handler/args resolved downstream by the executor
            RouteDecision::Command {
                command: trimmed.to_string(),
            }
        }
        PatternResult::NoMatch { input } => {
            // Not a structural command. A confident app-index match is still a
            // command ("spotify" → open) — let the executor resolve it; classify
            // it as Command so Enter runs it rather than asking AI.
            if let Some((_, score)) = crate::desktop_apps::app_index().best_match(&input)
                && score >= crate::desktop_apps::AUTO_LAUNCH_THRESHOLD
            {
                return RouteDecision::Command {
                    command: trimmed.to_string(),
                };
            }

            // A near-miss TYPO → offer the correction (uniformly, multi-word too).
            //
            // `TypoOnly` deliberately: `Correct` rewrites the user's input, so
            // only an actual misspelling qualifies. A correctly-spelled command
            // word sitting inside a question ("how do i open a jar file") must
            // NOT be corrected — that would convert the question into a command.
            // The completions list still offers it as a clickable row; routing
            // stays match-first, AI-last, and the user decides.
            if let Some(corrected) = crate::intent::typo_suggest::suggest_kind(
                trimmed,
                registry,
                crate::intent::typo_suggest::Kind::TypoOnly,
            )
            .and_then(|item| item.description)
            {
                return RouteDecision::Correct { corrected };
            }

            // Genuine natural language → AI (confidence-graded), or web if AI off.
            let confident = is_clear_question(trimmed);
            nl_or_disabled(&input, confident, /* explicit */ false, has_ai)
        }
    }
}

/// NL → agent/quick-ai when AI is on; otherwise the web-search fallback.
fn nl_or_disabled(prompt: &str, confident: bool, explicit: bool, has_ai: bool) -> RouteDecision {
    if has_ai {
        RouteDecision::Nl {
            prompt: prompt.to_string(),
            confident,
        }
    } else {
        RouteDecision::AiDisabled {
            command: format!("web {prompt}"),
            explicit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_registry::registry::ActionRegistry;
    use crate::action_registry::{ActionHandler, ActionResult, ExecContext, Trigger};
    use crate::error::LychiError;
    use async_trait::async_trait;

    /// Minimal handler carrying just an id + trigger declaration — enough to drive
    /// the router's prefix index without real handler dependencies. Mirrors the
    /// helper in patterns.rs (kept local to avoid cross-test-module coupling).
    struct TestHandler {
        id: &'static str,
        triggers: &'static [Trigger],
    }

    #[async_trait]
    impl ActionHandler for TestHandler {
        fn id(&self) -> &str {
            self.id
        }
        fn description(&self) -> &str {
            "test"
        }
        fn triggers(&self) -> &'static [Trigger] {
            self.triggers
        }
        async fn execute(
            &self,
            _ctx: &ExecContext,
            _args: &str,
        ) -> Result<ActionResult, LychiError> {
            Ok(ActionResult::default())
        }
    }

    /// A registry with the command prefixes the classify tests reference, so
    /// command-detection exercises the real prefix index.
    fn test_registry() -> ActionRegistry {
        static WEB: &[Trigger] = &[Trigger::keywords(&["web"])];
        static RUN: &[Trigger] = &[Trigger::keywords(&["run"])];
        static OPEN: &[Trigger] = &[Trigger::keywords(&["open"])];
        // File-utility verbs (Phase 1) — routed purely by their triggers.
        static CONVERT: &[Trigger] = &[Trigger::keywords(&["convert"])];
        static ZIP: &[Trigger] = &[Trigger::keywords(&["zip", "compress"])];
        static EXTRACT: &[Trigger] = &[Trigger::keywords(&["extract", "unzip"])];
        let mut r = ActionRegistry::new();
        r.register(Box::new(TestHandler {
            id: "web",
            triggers: WEB,
        }));
        r.register(Box::new(TestHandler {
            id: "run",
            triggers: RUN,
        }));
        r.register(Box::new(TestHandler {
            id: "open",
            triggers: OPEN,
        }));
        r.register(Box::new(TestHandler {
            id: "convert",
            triggers: CONVERT,
        }));
        r.register(Box::new(TestHandler {
            id: "zip",
            triggers: ZIP,
        }));
        r.register(Box::new(TestHandler {
            id: "extract",
            triggers: EXTRACT,
        }));
        r
    }

    /// No user presets configured.
    fn no_presets(_kw: &str) -> Option<(String, String)> {
        None
    }

    #[test]
    fn is_clear_question_grammar() {
        assert!(is_clear_question("what is rust?"));
        assert!(is_clear_question("why is the sky blue"));
        assert!(is_clear_question("explain closures"));
        assert!(is_clear_question("anything ending in a question?"));
        assert!(!is_clear_question("rust"));
        assert!(!is_clear_question("docker logs"));
        assert!(!is_clear_question("pasta recipe"));
    }

    #[test]
    fn explicit_ask_goes_to_agent() {
        let r = test_registry();
        assert_eq!(
            classify_string("ask what is rust?", &r, no_presets, true),
            RouteDecision::Nl {
                prompt: "what is rust?".into(),
                confident: true
            }
        );
    }

    #[test]
    fn panel_keywords_classify_as_panel() {
        let r = test_registry();
        assert_eq!(
            classify_string("settings", &r, no_presets, true),
            RouteDecision::Panel {
                name: PanelKind::Settings,
                sub_tab: None
            }
        );
        assert_eq!(
            classify_string("todos", &r, no_presets, true),
            RouteDecision::Panel {
                name: PanelKind::Notes,
                sub_tab: Some("todos".into())
            }
        );
    }

    #[test]
    fn file_verbs_route_to_command() {
        let r = test_registry();
        for input in [
            "convert ~/a/img.png to webp",
            "zip a.txt b.txt to out.zip",
            "compress a.txt",
            "extract bundle.tar.gz",
            "unzip archive.zip",
        ] {
            match classify_string(input, &r, no_presets, true) {
                RouteDecision::Command { command } => assert_eq!(command, input),
                other => panic!("{input:?} should route to Command, got {other:?}"),
            }
        }
    }

    #[test]
    fn clear_question_goes_to_agent_ambiguous_to_fork() {
        let r = test_registry();
        assert_eq!(
            classify_string("what is rust?", &r, no_presets, true),
            RouteDecision::Nl {
                prompt: "what is rust?".into(),
                confident: true
            }
        );
        // A bare ambiguous word: not a command, not a question → fork card.
        assert_eq!(
            classify_string("pastarecipe", &r, no_presets, true),
            RouteDecision::Nl {
                prompt: "pastarecipe".into(),
                confident: false
            }
        );
    }

    #[test]
    fn a_natural_question_containing_a_command_word_still_reaches_ai() {
        // ARCHITECTURE GUARD. `typo_suggest` now finds a command word anywhere
        // in a sentence, which is what powers the "Did you mean" row. But this
        // classifier must NOT turn that into a `Correct` decision: rewriting the
        // user's input mid-question is a routing change, and the suggestion is
        // meant to be an OFFER shown in the completions list, not a reroute.
        //
        // "how do i open a jar file" mentions `open`, but it is a question.
        let r = test_registry();
        let d = classify_string("how do i open a jar file", &r, no_presets, true);
        assert!(
            matches!(d, RouteDecision::Nl { .. }),
            "a question must reach AI, got {d:?}"
        );
    }

    #[test]
    fn nl_falls_to_web_when_ai_off() {
        let r = test_registry();
        // A bare NL query is NOT explicit (the user didn't invoke AI via `ask`),
        // even though it's a confident question → web fallback, quieter toast.
        assert_eq!(
            classify_string("what is rust?", &r, no_presets, false),
            RouteDecision::AiDisabled {
                command: "web what is rust?".into(),
                explicit: false
            }
        );
        // An explicit `ask …` with AI off IS explicit → louder toast.
        assert_eq!(
            classify_string("ask what is rust?", &r, no_presets, false),
            RouteDecision::AiDisabled {
                command: "web what is rust?".into(),
                explicit: true
            }
        );
    }

    #[test]
    fn preset_keyword_renders() {
        let r = test_registry();
        let presets = |kw: &str| {
            (kw == "translate").then(|| ("Translate: {input}".to_string(), "Translate".to_string()))
        };
        assert_eq!(
            classify_string("translate hola", &r, presets, true),
            RouteDecision::Preset {
                template: "Translate: {input}".into(),
                input: "hola".into()
            }
        );
    }
}
