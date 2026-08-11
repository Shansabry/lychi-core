pub mod ai_router;
pub mod classify;
pub mod patterns;
pub mod prompt;
pub mod typo_suggest;

use crate::action_registry::registry::ActionRegistry;
use ai_router::AiRouter;
use patterns::{Confidence, PatternResult};

/// How the intent was resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingMethod {
    Explicit,
    Pattern,
    Ai,
}

/// A resolved intent — the result of converting raw input into a structured action.
#[derive(Debug, Clone)]
pub struct ResolvedIntent {
    pub action_id: String,
    pub args: String,
    pub routing: RoutingMethod,
}

/// Intent Resolver — converts raw user input into structured intents.
///
/// Purely deterministic (pattern match → weak fallback → web). `ai_router` is
/// not consulted for routing any more — its presence is only the "AI is
/// configured" flag (`has_ai`) that switches natural language between the agent
/// and web. `Clone` shares it via `Arc`.
#[derive(Clone)]
pub struct IntentResolver {
    ai_router: Option<std::sync::Arc<AiRouter>>,
}

impl IntentResolver {
    pub fn new(ai_router: Option<AiRouter>) -> Self {
        Self {
            ai_router: ai_router.map(std::sync::Arc::new),
        }
    }

    /// Whether AI is available — its presence gates natural-language routing.
    pub fn has_ai(&self) -> bool {
        self.ai_router.is_some()
    }

    /// Set or replace the AI holder.
    pub fn set_ai_router(&mut self, router: AiRouter) {
        self.ai_router = Some(std::sync::Arc::new(router));
    }

    /// Remove the AI router (switch AI off at runtime).
    pub fn clear_ai_router(&mut self) {
        self.ai_router = None;
    }

    /// Resolve raw input into a structured intent. Deterministic — the agent
    /// (not this resolver) owns natural language.
    ///
    /// 1. Explicit/Strong pattern match → dispatch immediately
    /// 2. Input that IS an app's name → launch it (identity, not mention)
    /// 3. Weak pattern match → use it as the fallback
    /// 4. Otherwise → web search (a safe, instant default)
    pub async fn resolve(&self, raw: &str, registry: &ActionRegistry) -> ResolvedIntent {
        // Phase 1: Deterministic match
        let (no_match_input, weak_fallback) = match patterns::route(raw, registry) {
            PatternResult::Match(route) if route.confidence != Confidence::Weak => {
                // Explicit or Strong — dispatch immediately, no AI needed
                tracing::debug!(
                    phase = "pattern",
                    action = %route.handler,
                    explicit = route.explicit,
                    confidence = ?route.confidence,
                    "[resolve] phase=pattern action={} explicit={} confidence={:?}",
                    route.handler,
                    route.explicit,
                    route.confidence
                );
                return ResolvedIntent {
                    action_id: route.handler.to_string(),
                    args: route.args,
                    routing: if route.explicit {
                        RoutingMethod::Explicit
                    } else {
                        RoutingMethod::Pattern
                    },
                };
            }
            PatternResult::Match(route) => {
                // Weak match — try AI first, keep this as fallback
                tracing::debug!(
                    phase = "pattern",
                    action = %route.handler,
                    confidence = ?route.confidence,
                    "[resolve] phase=pattern action={} confidence=Weak (deferring to AI)",
                    route.handler,
                );
                let fallback = ResolvedIntent {
                    action_id: route.handler.to_string(),
                    args: route.args.clone(),
                    routing: RoutingMethod::Pattern,
                };
                (raw.trim().to_string(), Some(fallback))
            }
            PatternResult::NoMatch { input } => (input, None),
        };

        // Phase 1b: launch an app when the input IS that app's name.
        //
        // "Is", not "mentions". The app index scores a token-SET match at ≥0.90
        // — `[spotify] ⊆ [can,you,open,spotify]` — which is the right signal for
        // RANKING a suggestion and the wrong one for deciding to execute. Used
        // as a decision it produced this:
        //
        //     input "dnf search firefox"
        //     → phase=app-confident action=open score=0.92
        //     → launched Firefox
        //
        // The user typed a three-word command; the launcher opened a browser.
        // Same shape as "email me the firefox logs", and no verb blocklist fixes
        // it, because the flaw is structural: a subset match says the app was
        // MENTIONED, never that it was ASKED FOR.
        //
        // So execution requires identity — the query is the app name and nothing
        // else. That is the rule the frontend already applies to completions
        // (auto-select only on a prefix extension, per Chromium's
        // `allowed_to_be_default_match`); this makes the backend agree instead
        // of quietly overruling it. Chromium's own note on why: matching
        // mid-string "will mislead the user into thinking the What You Typed
        // match is what's selected."
        //
        // A subset match still SUGGESTS the app, ranked first — "can you open
        // spotify" costs one Enter on a visible row instead of launching
        // unasked. Cheap, and the row can be seen before it acts.
        //
        // Note the original justification no longer applies: it argued for
        // avoiding "a network round-trip to have AI re-derive it", and the
        // comment directly below records that AI intent-routing was REMOVED.
        // The optimisation outlived the cost it was avoiding.
        {
            let index = crate::desktop_apps::app_index();
            if let Some((id, score)) = index.best_match(&no_match_input)
                && score >= crate::desktop_apps::AUTO_LAUNCH_THRESHOLD
                && query_is_app_identity(&no_match_input, &index.entry(id).name_tokens)
            {
                let entry = index.entry(id);
                tracing::debug!(
                    phase = "app-identity",
                    action = "open",
                    app_score = score,
                    desktop = %entry.desktop_path,
                    "[resolve] phase=app-identity action=open score={score:.2} (input is the app name)"
                );
                return ResolvedIntent {
                    action_id: "open".to_string(),
                    args: entry.desktop_path.clone(),
                    routing: RoutingMethod::Pattern,
                };
            }
        }

        // NOTE: The old AI intent-routing phase (route_intent/route_or_plan →
        // an `ask` handler) has been REMOVED. Natural language is now owned
        // entirely by the streaming tool-calling agent (coordinator/), invoked
        // from the launcher input box — NOT by the executor. The executor's
        // resolver is purely deterministic: pattern match → weak fallback →
        // web search. There is ONE AI path, and it is the agent. Anything that
        // reaches here without a deterministic match falls through to web
        // search (a safe, instant default), never a hidden second AI call.

        // Phase 2b: use a weak pattern match if we have one.
        if let Some(fallback) = weak_fallback {
            tracing::debug!(
                phase = "weak-fallback",
                action = %fallback.action_id,
                "[resolve] phase=weak-fallback action={} (AI unavailable/inconclusive)",
                fallback.action_id
            );
            return fallback;
        }

        // Phase 3: Web fallback. Anything reaching here matched no registered
        // trigger and was not an app-name identity, so there is nothing
        // deterministic left to do.
        //
        // Note a high score can still land here: `dnf search firefox` scores
        // 0.92 on a token-set match and is rejected by Phase 1b's identity check,
        // not by the threshold. The old message said "below threshold"
        // unconditionally and was therefore actively misleading — it named a
        // cause that was false whenever the identity guard was the real reason.
        // Command-shaped input goes to the shell, not to Google.
        //
        // `dnf search firefox`, `htop`, `kubectl get pods` — the first word is a
        // real executable on this machine, so the input is a COMMAND the user
        // typed, not a question they asked. Sending it to web search returns a
        // results page for the literal string, which is never what anyone wanted.
        //
        // The test is the SHAPE of the input, not a list of known tools: "does
        // the first word exist as an executable?" So it covers dnf on Fedora,
        // pacman on Arch, and every binary nobody has thought of, without a
        // table to keep current — the build host cannot decide this for the
        // target machine, and here it does not have to.
        //
        // Routed to `run`, which owns the decision of terminal-vs-inline and
        // applies the shell rules; this phase only says "this is a command".
        if let Some(head) = command_head(&no_match_input)
            && which::which(head).is_ok()
        {
            tracing::debug!(
                phase = "fallback",
                action = "run",
                head,
                "[resolve] phase=fallback action=run (first word `{head}` is an executable)"
            );
            return ResolvedIntent {
                action_id: "run".to_string(),
                args: no_match_input,
                routing: RoutingMethod::Pattern,
            };
        }

        let app_match = crate::desktop_apps::app_index().best_match(&no_match_input);
        match app_match {
            Some((_, score)) => {
                let why = if score >= crate::desktop_apps::AUTO_LAUNCH_THRESHOLD {
                    "not an app-name identity"
                } else {
                    "below threshold"
                };
                tracing::debug!(
                    phase = "fallback",
                    action = "web",
                    app_score = score,
                    "[resolve] phase=fallback action=web (best app score={:.2}, {why})",
                    score
                );
                ResolvedIntent {
                    action_id: "web".to_string(),
                    args: no_match_input,
                    routing: RoutingMethod::Pattern,
                }
            }
            None => {
                tracing::debug!(
                    phase = "fallback",
                    action = "web",
                    "[resolve] phase=fallback action=web (no app candidates)"
                );
                ResolvedIntent {
                    action_id: "web".to_string(),
                    args: no_match_input,
                    routing: RoutingMethod::Pattern,
                }
            }
        }
    }
}

/// The first word of `input`, if it could plausibly be an executable name.
///
/// Deliberately conservative, because the caller uses this to route input to
/// the shell rather than to web search, and a false positive sends a question
/// to `run`. Two guards:
///
/// - the word must look like a program name (alphanumerics and `-_.+/`), so
///   "what's the weather?" and "how do I..." never qualify; and
/// - it must not be a lone short word — `ls` and `df` are real, but so is a
///   one-word question, and a bare `w` or `dd` typed into a launcher is far
///   more likely to be the start of a thought than an intent to run it.
///
/// Existence is checked by the CALLER via `which`, not here: this answers "is
/// this shaped like a command?", which is a pure string question and testable
/// without a filesystem.
fn command_head(input: &str) -> Option<&str> {
    let head = input.split_whitespace().next()?;
    let plausible = !head.is_empty()
        && head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/'))
        && head
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '/' || c == '.');
    if !plausible {
        return None;
    }
    // A bare one-word input is ambiguous; require either arguments or a name
    // long enough that it is unlikely to be a word someone was about to type
    // more of.
    let has_args = input.split_whitespace().count() > 1;
    if !has_args && head.len() < 3 {
        return None;
    }
    Some(head)
}

/// Whether `query` IS this app's name, as opposed to merely containing it.
///
/// The distinction that separates "spotify" (launch it) from "dnf search
/// firefox" (do not). A token-set match answers "is the app name a subset of
/// the query?", which is true for both; this answers "is the query nothing but
/// the app name?", which is true only for the first.
///
/// Compares token counts rather than strings so it stays punctuation- and
/// order-insensitive, and so it says nothing about which words are "commands" —
/// no verb list, nothing to keep up to date. `firefox`, `Firefox`, and
/// `firefox ` are identity; `open firefox` and `firefox logs` are not.
///
/// `open firefox` deliberately falls through: `open` is a registered trigger,
/// so it is handled by the pattern phase above as an explicit command rather
/// than by guessing here.
pub(crate) fn query_is_app_identity(query: &str, name_tokens: &[String]) -> bool {
    let query_tokens: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if query_tokens.is_empty() || query_tokens.len() != name_tokens.len() {
        return false;
    }
    name_tokens
        .iter()
        .all(|nt| query_tokens.contains(&nt.to_lowercase()))
}

#[cfg(test)]
mod identity_tests {
    use super::query_is_app_identity;

    /// The predicate reads only an app's name tokens, so the tests supply just
    /// those rather than constructing a whole `DesktopEntry` — narrower input,
    /// and the test cannot accidentally depend on unrelated fields.
    fn app(name: &str) -> Vec<String> {
        name.to_lowercase()
            .split_whitespace()
            .map(String::from)
            .collect()
    }

    /// THE BUG. `dnf search firefox` scored 0.92 on a token-set match and
    /// launched the browser. The app was mentioned, never asked for.
    #[test]
    fn a_command_that_merely_mentions_an_app_is_not_identity() {
        let firefox = app("Firefox");
        for query in [
            "dnf search firefox",
            "email me the firefox logs",
            "why is firefox slow",
            "firefox crashed again",
            "kill firefox",
        ] {
            assert!(
                !query_is_app_identity(query, &firefox),
                "should NOT auto-launch for: {query:?}"
            );
        }
    }

    /// The case that must keep working — the typed text IS the app.
    #[test]
    fn the_bare_app_name_is_identity() {
        let firefox = app("Firefox");
        for query in ["firefox", "Firefox", "  firefox  ", "FIREFOX"] {
            assert!(query_is_app_identity(query, &firefox), "{query:?}");
        }
    }

    /// Multi-word app names work the same way: all tokens, nothing extra.
    #[test]
    fn multi_token_app_names_are_identity_when_complete() {
        let vscode = app("Visual Studio Code");
        assert!(query_is_app_identity("visual studio code", &vscode));
        assert!(query_is_app_identity("Visual Studio Code", &vscode));
        // A subset is not identity — it is a prefix at best, which ranks but
        // does not execute.
        assert!(!query_is_app_identity("visual studio", &vscode));
        // Nor is the full name plus anything else.
        assert!(!query_is_app_identity("open visual studio code", &vscode));
    }

    /// Punctuation should not decide whether something launches.
    #[test]
    fn punctuation_is_ignored() {
        assert!(query_is_app_identity("firefox!", &app("Firefox")));
        assert!(query_is_app_identity("(firefox)", &app("Firefox")));
    }

    #[test]
    fn empty_input_is_never_identity() {
        assert!(!query_is_app_identity("", &app("Firefox")));
        assert!(!query_is_app_identity("   ", &app("Firefox")));
    }
}

#[cfg(test)]
mod command_shape_tests {
    use super::command_head;

    /// The case that motivated this: a real command was being sent to web
    /// search, which returns a results page for the literal string.
    #[test]
    fn multi_word_commands_are_command_shaped() {
        assert_eq!(command_head("dnf search firefox"), Some("dnf"));
        assert_eq!(command_head("kubectl get pods"), Some("kubectl"));
        assert_eq!(command_head("git status"), Some("git"));
        assert_eq!(command_head("docker ps -a"), Some("docker"));
    }

    /// The dangerous direction. A false positive here routes a QUESTION to the
    /// shell, which is worse than the bug being fixed — so prose must never
    /// qualify, whatever its first word is.
    #[test]
    fn prose_is_not_command_shaped() {
        for q in [
            "what's the weather today?",
            "how do I center a div",
            "why is my laptop slow",
            "remind me to call mum",
            "email the team about friday",
        ] {
            let head = command_head(q);
            // Either rejected outright, or the head is a word that will not
            // exist as an executable — the caller's `which` check is the second
            // gate. Only apostrophes/punctuation are rejected at this stage.
            if let Some(h) = head {
                assert!(
                    !h.contains(|c: char| !c.is_ascii_alphanumeric()
                        && !matches!(c, '-' | '_' | '.' | '+' | '/')),
                    "{q:?} → {h:?}"
                );
            }
        }
        // Punctuation in the first word disqualifies immediately.
        assert_eq!(command_head("what's the weather?"), None);
    }

    /// A bare short word is more likely the start of a thought than an intent
    /// to run `w` or `dd`.
    #[test]
    fn bare_short_words_are_not_commands() {
        assert_eq!(command_head("w"), None);
        assert_eq!(command_head("dd"), None);
        assert_eq!(command_head("ls"), None);
        // …but with arguments the intent is unambiguous.
        assert_eq!(command_head("ls -la"), Some("ls"));
        assert_eq!(command_head("dd if=/dev/zero"), Some("dd"));
    }

    /// A long bare name is unambiguous enough on its own.
    #[test]
    fn bare_long_names_are_command_shaped() {
        assert_eq!(command_head("htop"), Some("htop"));
        assert_eq!(command_head("kubectl"), Some("kubectl"));
    }

    #[test]
    fn paths_are_command_shaped() {
        assert_eq!(command_head("/usr/bin/env python"), Some("/usr/bin/env"));
        assert_eq!(
            command_head("./configure --prefix=/usr"),
            Some("./configure")
        );
    }

    #[test]
    fn empty_input_has_no_head() {
        assert_eq!(command_head(""), None);
        assert_eq!(command_head("   "), None);
    }
}
