pub mod ai_router;
pub mod patterns;
pub mod prompt;
pub mod typo_suggest;

use crate::action_registry::registry::ActionRegistry;
use crate::providers::{AgentPlan, AiResponse};
use ai_router::AiRouter;
use patterns::{Confidence, PatternResult};

/// Whether an unmatched input is worth an AI routing call, or is clearly noise
/// that should skip straight to the web fallback (saves a network round-trip +
/// tokens on gibberish like "asdfghjkl").
///
/// DELIBERATELY CONSERVATIVE — AI is good at messy input, so we only skip when
/// there is genuinely nothing to route:
///   - Multi-word input always keeps AI (could be a real phrasing).
///   - Any app candidate (even weak) keeps AI.
///   - Only a LONE token with NO app candidate AND a random/unpronounceable
///     shape (no vowels, or mostly non-letters) is treated as noise.
/// False negatives just mean one wasted AI call — never a missed real request.
fn input_worth_ai(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Multi-token input: always worth AI.
    if trimmed.split_whitespace().count() > 1 {
        return true;
    }
    // A single token that the app index can place at all → worth routing.
    if crate::desktop_apps::app_index()
        .best_match(trimmed)
        .is_some()
    {
        return true;
    }
    // Lone token, no app candidate: worth AI only if it looks like a real word.
    token_looks_like_word(trimmed)
}

/// Whether a lone token looks like a pronounceable word (worth AI) versus random
/// noise (skip). Real words are mostly letters, have a reasonable VOWEL RATIO,
/// and don't contain long consonant runs — so keyboard-mash like "asdfghjkl"
/// (one vowel, a 7-consonant run) reads as noise even though it has a vowel.
/// Pure function — unit-testable without the app index.
fn token_looks_like_word(token: &str) -> bool {
    let lower = token.to_lowercase();
    let total = lower.chars().count();
    if total == 0 {
        return false;
    }
    let is_vowel = |c: char| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y');
    let letters = lower.chars().filter(|c| c.is_alphabetic()).count();
    // Short tokens (≤3) with a vowel are given the benefit of the doubt ("cat",
    // "os"); the ratio/run heuristics are unreliable at that length.
    if total <= 3 {
        return lower.chars().any(&is_vowel) && letters * 2 >= total;
    }
    // Mostly letters.
    if letters * 2 < total {
        return false;
    }
    // Vowel ratio: real words are ~30-60% vowels; noise is vowel-starved.
    let vowels = lower.chars().filter(|&c| is_vowel(c)).count();
    if vowels * 5 < total {
        // < 20% vowels → unpronounceable
        return false;
    }
    // No consonant run longer than 4 (English tops out ~"strengths").
    let mut run = 0;
    for c in lower.chars() {
        if c.is_alphabetic() && !is_vowel(c) {
            run += 1;
            if run > 4 {
                return false;
            }
        } else {
            run = 0;
        }
    }
    true
}

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
/// Combines deterministic pattern matching with optional AI routing.
pub struct IntentResolver {
    ai_router: Option<AiRouter>,
}

impl IntentResolver {
    pub fn new(ai_router: Option<AiRouter>) -> Self {
        Self { ai_router }
    }

    /// Whether AI routing is available.
    pub fn has_ai(&self) -> bool {
        self.ai_router.is_some()
    }

    /// Get the AI router reference (for health checks etc).
    pub fn ai_router(&self) -> Option<&AiRouter> {
        self.ai_router.as_ref()
    }

    /// Set or replace the AI router.
    pub fn set_ai_router(&mut self, router: AiRouter) {
        self.ai_router = Some(router);
    }

    /// Resolve raw input into a structured intent.
    ///
    /// Four-phase pipeline:
    /// 1. Explicit/Strong match → dispatch immediately
    /// 2. Weak match → try AI first, use weak match as fallback
    /// 3. No match + AI available → ask AI
    /// 4. No match + AI unavailable → AppIndex → web search
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

        // Phase 1b: A CONFIDENT local app match short-circuits AI. When the app
        // index resolves the input to an app at auto-launch confidence (≥0.90,
        // e.g. "spotify" or "can you open spotify" via token-set matching), we
        // KNOW the intent — launching it is instant and certain, so we never pay
        // a network round-trip to have AI re-derive it. AI still handles genuinely
        // fuzzy input below this bar (see Phase 2). Certainty beats a guess.
        {
            let app_match = crate::desktop_apps::app_index().best_match(&no_match_input);
            if let Some((id, score)) = app_match
                && score >= crate::desktop_apps::AUTO_LAUNCH_THRESHOLD
            {
                let index = crate::desktop_apps::app_index();
                let entry = index.entry(id);
                tracing::debug!(
                    phase = "app-confident",
                    action = "open",
                    app_score = score,
                    desktop = %entry.desktop_path,
                    "[resolve] phase=app-confident action=open score={score:.2} (pre-AI short-circuit)"
                );
                return ResolvedIntent {
                    action_id: "open".to_string(),
                    args: entry.desktop_path.clone(),
                    routing: RoutingMethod::Pattern,
                };
            }
        }

        // Phase 2: Try AI routing (for both Weak matches and NoMatch).
        // Skip the network round-trip for input that is clearly noise — a lone
        // gibberish token AI can't map to any handler and that has no app
        // candidate. Conservative on purpose: multi-word input, a weak pattern
        // fallback, or any app candidate all keep AI (it's good at messy input);
        // we only skip when there's genuinely nothing to route. Saves tokens +
        // latency on "asdfghjkl" without ever starving a real request.
        let worth_asking_ai = weak_fallback.is_some() || input_worth_ai(&no_match_input);
        if let Some(ai) = &self.ai_router
            && worth_asking_ai
        {
            // Exclude "open" from known IDs — it's the no-match fallback, not a real intent.
            let known: Vec<&str> = registry
                .list_ids()
                .into_iter()
                .filter(|id| *id != "open")
                .collect();
            if let Ok(Some(ai_route)) = ai.try_route(raw, &known).await
                && registry.has(&ai_route.action_id)
                && ai_route.action_id != "open"
            {
                tracing::debug!(
                    phase = "ai",
                    action = %ai_route.action_id,
                    "[resolve] phase=ai action={}",
                    ai_route.action_id
                );
                return ResolvedIntent {
                    action_id: ai_route.action_id,
                    args: ai_route.args,
                    routing: RoutingMethod::Ai,
                };
            }
        }

        // Phase 2b: AI failed or unavailable — use weak pattern match if we have one
        if let Some(fallback) = weak_fallback {
            tracing::debug!(
                phase = "weak-fallback",
                action = %fallback.action_id,
                "[resolve] phase=weak-fallback action={} (AI unavailable/inconclusive)",
                fallback.action_id
            );
            return fallback;
        }

        // Phase 3: Web fallback. A confident app match (≥ AUTO_LAUNCH_THRESHOLD)
        // was already handled in Phase 1b before AI, so anything reaching here is
        // either a below-threshold app hint or no app at all — both go to web.
        let app_match = crate::desktop_apps::app_index().best_match(&no_match_input);
        match app_match {
            Some((_, score)) => {
                tracing::debug!(
                    phase = "fallback",
                    action = "web",
                    app_score = score,
                    "[resolve] phase=fallback action=web (best app score={:.2} below threshold)",
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

    /// Ask AI for a multi-step plan. Returns `None` if AI is unavailable
    /// or the input resolves to a single-shot route.
    pub async fn try_plan(&self, raw: &str, registry: &ActionRegistry) -> Option<AgentPlan> {
        // Only unmatched inputs can produce plans — deterministic matches are final
        if let PatternResult::Match(_) = patterns::route(raw, registry) {
            return None;
        }

        let ai = self.ai_router.as_ref()?;
        let known: Vec<&str> = registry.list_ids();

        match ai.try_route_or_plan(raw, &known).await {
            Ok(Some(AiResponse::Plan(plan))) => Some(plan),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::token_looks_like_word;

    #[test]
    fn real_words_are_worth_ai() {
        for w in ["spotify", "brighten", "louder", "sleep", "help", "code"] {
            assert!(token_looks_like_word(w), "{w} should look like a word");
        }
    }

    #[test]
    fn gibberish_is_noise() {
        // Keyboard-mash: vowel-starved and/or long consonant runs.
        for w in [
            "asdfghjkl",
            "xkcdq",
            "zxcvbn",
            "qwrtp",
            "1234",
            "@#$%",
            "hjkl",
        ] {
            assert!(!token_looks_like_word(w), "{w} should be noise");
        }
    }

    #[test]
    fn pronounceable_words_survive() {
        // Real single-word requests AI could map. (App acronyms like "vlc"/"npm"
        // don't rely on this shape check — the app-index branch in input_worth_ai
        // catches them first; this function is only the last-resort noise filter.)
        for w in ["gmail", "sync", "crypt", "sleep", "reboot", "screenshot"] {
            assert!(token_looks_like_word(w), "{w} should survive as a word");
        }
    }

    #[test]
    fn empty_is_noise() {
        assert!(!token_looks_like_word(""));
    }
}
