//! "Did you mean: open X?" — catch a mistyped APP NAME and offer to launch it.
//!
//! ## One job: a misspelled app name
//!
//! This module does exactly ONE thing — a single-word query that fuzzy-matches
//! an installed app is offered as a launch: "spoti" → open Spotify.
//!
//! That is the one case worth a local suggestion, because launching an app is
//! instant and deterministic — better than round-tripping a typo through the AI.
//! Everything else is deliberately NOT handled here:
//!
//!   - A **misspelled command keyword** ("weathr" → "weather") is left to the AI
//!     agent, which understands it fine — a correction row for it is redundant.
//!   - A **natural sentence** ("can you define gallop") is prose; it goes to the
//!     AI. This module never tries to extract a command out of a sentence — that
//!     guessing was inaccurate and is what this was cut down to remove.
//!
//! ## Suggestion, never a route
//!
//! This only ever produces a row the user must accept. It does not change
//! routing. Selecting the row fills the input, and Enter then goes through the
//! same single classifier as if the user had typed it — no second routing path
//! to drift.

use crate::action_registry::CompletionItem;
use crate::action_registry::registry::ActionRegistry;

/// Minimum word length to consider (skip very short inputs like "we").
const MIN_WORD_LEN: usize = 3;

/// Lower bound for app-name "Did you mean" suggestions. A fuzzy app score in
/// `[APP_SUGGEST_FLOOR, AUTO_LAUNCH_THRESHOLD)` is confident enough to OFFER the
/// app ("spoti" → Spotify) but not to auto-launch it. Above the threshold the
/// resolver already launches directly; below the floor it's too weak to suggest.
const APP_SUGGEST_FLOOR: f32 = 0.55;

/// Damerau-Levenshtein edit distance (optimal string alignment), single-row DP
/// plus one extra row to see transpositions.
///
/// Build the "Did you mean: open X?" row.
///
/// `kind: Correction` is what the frontend switches on — it must not have to
/// recognise this row by its label text.
fn row(suggestion: String) -> CompletionItem {
    CompletionItem {
        label: format!("Did you mean: {suggestion}?"),
        icon_path: Some("__none__".to_string()),
        score: 90,
        description: Some(suggestion),
        reason: None,
        thumb_b64: None,
        kind: Some(crate::action_registry::CompletionKind::Correction),
        ..Default::default()
    }
}

/// Offer to launch a MISSPELLED app name: a single-word query that fuzzy-matches
/// an installed app is surfaced as "Did you mean: open X?". Returns `None` for
/// everything else — a correctly-typed command (the pattern router handles it),
/// a misspelled *command* (the AI handles it), or a natural sentence (also AI).
///
/// `registry` is unused today but kept in the signature so the two call sites
/// (classifier + completions) stay uniform and the guard against suggesting a
/// real command word can move here without a signature churn.
pub fn suggest(raw: &str, _registry: &ActionRegistry) -> Option<CompletionItem> {
    let lower = raw.trim().to_lowercase();
    // Strip trailing/leading sentence punctuation; keep hyphens/underscores.
    let words: Vec<&str> = lower
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| c.is_ascii_punctuation() && c != '-' && c != '_'))
        .filter(|w| !w.is_empty())
        .collect();

    // Single word only — a sentence is a job for the AI, not app-matching.
    if words.len() != 1 {
        return None;
    }
    let first = words[0];
    if first.len() < MIN_WORD_LEN {
        return None;
    }

    // Fuzzy-match an installed app in the SUGGEST band — confident enough to
    // offer ("spoti" → Spotify), but below AUTO_LAUNCH_THRESHOLD so we never
    // silently launch the wrong app.
    let index = crate::desktop_apps::app_index();
    let (id, score) = index.best_match(first)?;
    if !(APP_SUGGEST_FLOOR..crate::desktop_apps::AUTO_LAUNCH_THRESHOLD).contains(&score) {
        return None;
    }
    let name = index.entry(id).name.clone();
    // Skip if the query already IS the app name — no typo to fix.
    if name.to_lowercase() == first {
        return None;
    }
    Some(row(format!("open {name}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    // `suggest` matches against the process-global app index (`app_index()`),
    // which these unit tests cannot seed — so they pin the deterministic half:
    // the ABSTENTION rules that run before any app lookup. A `Some(...)` result
    // requires a real installed app to fuzzy-match and is exercised by the
    // integration/manual path, not here. `registry` is unused by `suggest`, so a
    // fresh empty one is fine.
    fn suggest(raw: &str) -> Option<CompletionItem> {
        super::suggest(raw, &ActionRegistry::new())
    }

    #[test]
    fn multi_word_input_never_suggests_an_app() {
        // A sentence is a job for the AI, not app-matching — abstain before the
        // app index is ever consulted.
        for q in [
            "can you define gallop",
            "what is the meaning of life",
            "open the pod bay doors",
            "weathr in tokyo",
        ] {
            assert!(
                suggest(q).is_none(),
                "{q} is multi-word — no app suggestion"
            );
        }
    }

    #[test]
    fn short_input_is_ignored() {
        // Below MIN_WORD_LEN there is nothing to disambiguate.
        assert!(suggest("we").is_none());
        assert!(suggest("a").is_none());
        assert!(suggest("").is_none());
    }

    #[test]
    fn a_trailing_question_mark_does_not_split_the_word() {
        // "spoti?" must tokenise to the single word "spoti" (the "?" is sentence
        // punctuation), so it reaches the single-word app lookup rather than being
        // rejected by the multi-word guard. We can't assert the app result (the
        // index is machine-dependent), but we CAN assert it behaves identically to
        // the bare word — punctuation stripping is what this pins.
        assert_eq!(
            suggest("spoti?").is_some(),
            suggest("spoti").is_some(),
            "a trailing ? must not change whether a single word matches"
        );
    }
}
