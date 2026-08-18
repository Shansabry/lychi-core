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
    /// Natural language → the full tool-calling agent. Every NL query (a clear
    /// question or an ambiguous phrase alike) goes straight to the agent, which
    /// answers and can act. (There was once a `confident` flag that sent vague
    /// input to a quick-answer "fork card" first; that path was removed.)
    Nl {
        prompt: String,
        /// The router demoted a preset CHAIN ("summarize and add it to
        /// notes") here: material is expected but none is inline, so the
        /// frontend should attach the PRIMARY selection as a `<pasted>` block
        /// — the same identifier the copied-text token expands into.
        #[serde(default)]
        wants_selection: bool,
    },
    /// An AI preset invocation. When `input` is empty the FE first tries the
    /// PRIMARY selection (highlighted text) as `{input}`, and prompts inline
    /// (re-filling `keyword `) when there is nothing to act on.
    Preset {
        keyword: String,
        template: String,
        input: String,
    },
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
            wants_selection: false,
        };
    }
    let lower = trimmed.to_lowercase();

    // Explicit `ask <q>` → the full agent (or AI-disabled).
    if lower == "ask" || lower.starts_with("ask ") {
        let q = trimmed[3..].trim();
        if !q.is_empty() {
            return nl_or_disabled(q, /* explicit */ true, has_ai);
        }
        // bare "ask" falls through to normal handling
    }

    // AI preset invocation: `<keyword> <text>` (or a bare `<keyword>`).
    //
    // EXCEPT chained requests: a preset is a single tool-free text transform,
    // so "summarize and add it to notes" must reach the AGENT (which can do
    // both). The structural tell — with <pasted> material blocks removed — is
    // the keyword followed IMMEDIATELY by a conjunction: there is no inline
    // material, only further instructions. Material merely CONTAINING "and"
    // ("summarize war and peace") still hits the preset.
    let first_word = lower.split_whitespace().next().unwrap_or(&lower);
    if let Some((template, _name)) = preset_for(first_word) {
        let rest = trimmed
            .split_once(char::is_whitespace)
            .map(|(_, r)| r.trim().to_string())
            .unwrap_or_default();
        let rest_structural = crate::coordinator::strip_material_blocks(&rest);
        let rest_structural = rest_structural.trim();
        let chained = rest_structural
            .strip_prefix("and")
            .is_some_and(|t| t.is_empty() || t.starts_with(char::is_whitespace));
        if chained && has_ai {
            // Material is implied by the keyword but none is inline — the FE
            // attaches the selection (the preset path's own fallback source).
            return RouteDecision::Nl {
                prompt: trimmed.to_string(),
                wants_selection: true,
            };
        }
        // A preset is an explicit AI ask; when AI is off, warn + fall to web.
        if !has_ai {
            return RouteDecision::AiDisabled {
                command: format!("web {rest}"),
                explicit: true,
            };
        }
        return RouteDecision::Preset {
            keyword: first_word.to_string(),
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
            //
            // The identity gate is NOT optional: it is the same guard
            // `resolve()` applies at this exact threshold. A token-subset
            // match scores 0.90, so without it "why is firefox slow"
            // classified as Command here while the executor's own guard
            // rejected it — the question then fell to a web search of the
            // literal text, with AI configured and willing. Two deciders
            // answering the same question differently is this codebase's
            // most-documented failure mode; the app row is still OFFERED in
            // the completions list either way.
            {
                let index = crate::desktop_apps::app_index();
                if let Some((id, score)) = index.best_match(&input)
                    && score >= crate::desktop_apps::AUTO_LAUNCH_THRESHOLD
                    && crate::intent::query_is_app_identity(&input, &index.entry(id).name_tokens)
                {
                    return RouteDecision::Command {
                        command: trimmed.to_string(),
                    };
                }
            }

            // A near-miss TYPO → offer the correction (uniformly, multi-word too).
            //
            // `TypoOnly` deliberately: `Correct` rewrites the user's input, so
            // only an actual misspelling qualifies. A correctly-spelled command
            // word sitting inside a question ("how do i open a jar file") must
            // NOT be corrected — that would convert the question into a command.
            // The completions list still offers it as a clickable row; routing
            // stays match-first, AI-last, and the user decides.
            if let Some(corrected) = crate::intent::typo_suggest::suggest(trimmed, registry)
                .and_then(|item| item.description)
            {
                return RouteDecision::Correct { corrected };
            }

            // Genuine natural language → the full agent, or web if AI is off.
            nl_or_disabled(&input, /* explicit */ false, has_ai)
        }
    }
}

/// NL → the full agent when AI is on; otherwise the web-search fallback.
fn nl_or_disabled(prompt: &str, explicit: bool, has_ai: bool) -> RouteDecision {
    if has_ai {
        RouteDecision::Nl {
            prompt: prompt.to_string(),
            wants_selection: false,
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
    fn explicit_ask_goes_to_agent() {
        let r = test_registry();
        assert_eq!(
            classify_string("ask what is rust?", &r, no_presets, true),
            RouteDecision::Nl {
                prompt: "what is rust?".into(),
                wants_selection: false,
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
    fn all_natural_language_goes_to_the_agent() {
        let r = test_registry();
        // A clear question and a bare ambiguous phrase now route identically —
        // straight to the full agent (the quick-answer fork card was removed).
        for q in ["what is rust?", "pastarecipe"] {
            assert_eq!(
                classify_string(q, &r, no_presets, true),
                RouteDecision::Nl {
                    prompt: q.into(),
                    wants_selection: false,
                },
                "{q:?} should route to the agent",
            );
        }
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
                keyword: "translate".into(),
                template: "Translate: {input}".into(),
                input: "hola".into()
            }
        );
    }

    /// THE SUBSET SHAPE (ROUTE-2): a question that merely CONTAINS an app name
    /// scores 0.90 as a token-subset match — without the identity gate,
    /// classify_string called it a Command while resolve()'s own guard
    /// rejected it, and the question fell to a web search of the literal text
    /// with AI configured. The gate here must be the SAME one resolve()
    /// applies: identity launches, mentions reach AI.
    #[test]
    fn a_question_naming_an_app_reaches_ai_not_launch() {
        use crate::desktop_apps::index;
        // Serialise on the global-index lock and restore on drop, mirroring
        // the executor tests that swap the same process-wide index.
        let _guard = index::test_index_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        index::set_app_index_for_test(vec![index::tests::make_entry(
            "Firefox",
            "firefox",
            &[],
            None,
            Some("firefox"),
        )]);

        let r = test_registry();

        // Identity → Command (the "spotify" fast path stays).
        assert_eq!(
            classify_string("firefox", &r, no_presets, true),
            RouteDecision::Command {
                command: "firefox".into()
            }
        );

        // Subset — the question mentions the app but is not the app.
        let decision = classify_string("why is firefox slow", &r, no_presets, true);
        assert!(
            matches!(decision, RouteDecision::Nl { .. }),
            "a question naming an app must reach AI, not launch it"
        );

        index::rebuild_app_index();
    }

    #[test]
    fn a_chained_preset_request_reaches_the_agent_not_the_preset() {
        let r = test_registry();
        let summarize = |w: &str| {
            (w == "summarize").then(|| ("Summarize: {input}".to_string(), "Summarize".to_string()))
        };
        // Keyword followed immediately by a conjunction = a chain the tool-free
        // preset cannot honor — the agent gets it.
        match classify_string("summarize and add it to the notes", &r, summarize, true) {
            RouteDecision::Nl { .. } => {}
            other => panic!("expected Nl, got {other:?}"),
        }
        // Same with the pasted-material block between keyword and conjunction.
        match classify_string(
            "summarize <pasted>\nlong article body here\n</pasted> and add it to the notes",
            &r,
            summarize,
            true,
        ) {
            RouteDecision::Nl { .. } => {}
            other => panic!("expected Nl, got {other:?}"),
        }
        // Material merely CONTAINING "and" stays a preset.
        match classify_string("summarize war and peace in two lines", &r, summarize, true) {
            RouteDecision::Preset { keyword, .. } => assert_eq!(keyword, "summarize"),
            other => panic!("expected Preset, got {other:?}"),
        }
    }
}
