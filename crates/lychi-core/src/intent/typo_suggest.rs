//! "Did you mean: X?" — the one place a near-miss input is turned into an
//! offer the user can accept.
//!
//! Two kinds of near-miss reach this module, and they used to be handled by
//! three separate mechanisms (a first-word Levenshtein pass, a hardcoded
//! `PHRASES` table of multi-word patterns, and an app-name fuzzy pass):
//!
//!   1. **Misspelled** — "weathr tokyo". The word is *meant* to be a command.
//!   2. **Naturally phrased** — "can you define gallop". The command word is
//!      right there, just not in first position, so the first-word router
//!      never sees it and the query falls through to a slow AI call.
//!
//! Both now resolve through ONE matcher over an *invocation vocabulary* built
//! from the live registry.
//!
//! ## Why there is no stop-word list
//!
//! The obvious fix for case 2 is to strip filler ("can", "you", "please") and
//! retry. That means maintaining a list of words that mean nothing — which goes
//! stale, is English-only, and is exactly the kind of hardcoded table this
//! codebase avoids.
//!
//! Instead the vocabulary IS the registry's trigger set. A word either names a
//! command or it doesn't, and that's a lookup, not a judgement:
//!
//! ```text
//!   define  → in vocabulary → command word
//!   can     → not in vocabulary → not a command word
//!   you     → not in vocabulary → not a command word
//!   gallop  → not in vocabulary → not a command word (so: an argument)
//! ```
//!
//! Filler and arguments are indistinguishable by vocabulary alone — both are
//! simply "not a command". They're told apart by POSITION: everything after the
//! command word is its argument. That's why no list is needed. A launcher
//! shipping a new handler tomorrow extends this automatically; a stop-word list
//! would not.
//!
//! (Measured against the real 37-handler corpus: every trigger scores as a
//! command word, and "the", "a", "in", "can", "you", "please" all score zero —
//! without any of them being enumerated anywhere.)
//!
//! ## Suggestion, never a route
//!
//! This module only ever produces a row the user must accept. It does not
//! change routing. Selecting the row fills the input, and Enter then goes
//! through the same single classifier as if the user had typed it — so there is
//! no second routing path to drift.

use crate::action_registry::CompletionItem;
use crate::action_registry::registry::ActionRegistry;

/// Maximum Levenshtein distance accepted as a typo.
/// 1 = single typo ("tmie" → "time"); 2 = two ("weathr" → "weather").
const MAX_DISTANCE: usize = 2;

/// Minimum word length to consider for typo matching (skip very short inputs).
const MIN_WORD_LEN: usize = 3;

/// Lower bound for app-name "Did you mean" suggestions. A fuzzy app score in
/// `[APP_SUGGEST_FLOOR, AUTO_LAUNCH_THRESHOLD)` is confident enough to OFFER the
/// app ("spoti" → Spotify) but not to auto-launch it. Above the threshold the
/// resolver already launches directly; below the floor it's too weak to suggest.
const APP_SUGGEST_FLOOR: f32 = 0.55;

/// Damerau-Levenshtein edit distance (optimal string alignment), single-row DP
/// plus one extra row to see transpositions.
///
/// Transposition counts as ONE edit, not two — and that distinction is what
/// makes this usable as a typo filter. Under plain Levenshtein a real
/// fat-finger typo and an unrelated word are indistinguishable:
///
/// ```text
///   tmie → time   2   (a genuine transposition typo)
///   the  → time   2   (an ordinary English word)
/// ```
///
/// Counting the transposition as 1 separates them, so a budget of 1 accepts
/// real typos while rejecting words that merely happen to be nearby.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    // Three rows: i-2, i-1, i. The i-2 row is what makes transposition visible.
    let mut prev2: Vec<usize> = vec![0; n + 1];
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            let mut best = (prev[j] + 1) // deletion
                .min(curr[j - 1] + 1) // insertion
                .min(prev[j - 1] + cost); // substitution
            // Transposition: the previous two chars are swapped.
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(prev2[j - 2] + 1);
            }
            curr[j] = best;
        }
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

/// Build the row every branch returns, so the shape is defined once.
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

/// Where a command word was found in the query, and how.
struct Hit {
    /// The registry keyword (already corrected if it was a typo).
    keyword: String,
    /// Index of the matching query word.
    pos: usize,
    /// True when the query word was misspelled rather than exact.
    was_typo: bool,
}

/// Find the command word in a query, wherever it sits and however it's spelled.
///
/// Scans left to right and prefers an EXACT vocabulary hit over a fuzzy one —
/// a correctly spelled command word later in the sentence beats a typo-ish word
/// earlier, so "can you define gallop" resolves on `define` rather than
/// mangling "can" into some 2-edit neighbour.
fn find_command_word(words: &[&str], registry: &ActionRegistry) -> Option<Hit> {
    // Pass 1: exact. A word that IS a command keyword, anywhere in the query.
    for (pos, w) in words.iter().enumerate() {
        if registry.is_known_prefix(w) {
            return Some(Hit {
                keyword: (*w).to_string(),
                pos,
                was_typo: false,
            });
        }
    }

    // Pass 2: fuzzy — the word is MEANT to be a command but is misspelled.
    //
    // Scoped to the FIRST word only, deliberately. Edit distance is a weak
    // signal at short lengths: "the" is 2 edits from "time", "life" is 2 from
    // "time". Scanning a whole sentence for near-misses turns any prose into a
    // command suggestion ("what is the meaning of life" → "time meaning of
    // life"). The first word is where a mistyped command actually appears, and
    // limiting to it is what keeps abstention correct.
    //
    // Natural phrasing is NOT affected: a correctly-spelled command word
    // anywhere in the sentence is already found by pass 1. We only give up on
    // the combination of "misspelled AND not in first position", which is rare
    // and not worth the false positives.
    let first = *words.first()?;
    if first.len() < MIN_WORD_LEN {
        return None;
    }
    // Require the typo to be proportionate to the word: a 3-4 char word gets
    // one edit, longer words two. Length-derived, not a per-word exception.
    let allowed = if first.len() <= 4 { 1 } else { MAX_DISTANCE };
    let mut best: Option<(String, usize)> = None; // (keyword, distance)
    for kw in registry.known_prefixes() {
        let dist = levenshtein(first, kw);
        if dist > 0 && dist <= allowed && best.as_ref().is_none_or(|(_, d)| dist < *d) {
            best = Some((kw.to_string(), dist));
        }
    }
    best.map(|(keyword, _)| Hit {
        keyword,
        pos: 0,
        was_typo: true,
    })
}

/// What kind of near-miss a caller is willing to act on.
///
/// This distinction is load-bearing, and it exists because the two callers want
/// genuinely different things:
///
/// - The COMPLETIONS list shows an offer the user may click. Surfacing
///   "Did you mean: open a jar file?" beside a question is helpful; the user
///   ignores it and presses Enter to reach AI as usual. → [`Kind::Any`]
///
/// - The CLASSIFIER decides what Enter does. A `Correct` decision REWRITES the
///   user's input, so acting on a naturally-phrased sentence there would turn
///   "how do i open a jar file" into the command `open a jar file` — silently
///   converting a question into an action. Only an actual misspelling is safe
///   to correct at that level. → [`Kind::TypoOnly`]
///
/// Keeping this a parameter (rather than two near-identical functions) means
/// there is still ONE matcher; callers declare their tolerance, they don't
/// re-implement the rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Only a misspelled command word. Safe to rewrite input with.
    TypoOnly,
    /// Also a correctly-spelled command word found mid-sentence. Display only.
    Any,
}

/// Try to find a "Did you mean: X?" suggestion for a near-miss input.
///
/// Returns `None` when the query contains no command word at all — that's a
/// genuine natural-language question, and it should reach the AI/web fallback
/// untouched rather than being forced into a command.
pub fn suggest(raw: &str, registry: &ActionRegistry) -> Option<CompletionItem> {
    suggest_kind(raw, registry, Kind::Any)
}

/// [`suggest`] with an explicit tolerance — see [`Kind`].
pub fn suggest_kind(raw: &str, registry: &ActionRegistry, kind: Kind) -> Option<CompletionItem> {
    let lower = raw.trim().to_lowercase();
    // Strip sentence punctuation from each word. A natural request ends in "?"
    // or ".", and that mark belongs to the SENTENCE, not to the command word or
    // its argument: "can you define gallop?" must look up `gallop`, not
    // `gallop?`. Done at tokenization so both matching and argument extraction
    // see the same clean words — stripping in only one of them is how the
    // trailing "?" reached the dictionary.
    let words: Vec<&str> = lower
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| c.is_ascii_punctuation() && c != '-' && c != '_'))
        .filter(|w| !w.is_empty())
        .collect();

    if words.is_empty() || lower.len() < MIN_WORD_LEN {
        return None;
    }

    // A correctly-spelled command in FIRST position is already routed by the
    // pattern matcher — there is nothing to suggest.
    let already_routed = registry.is_known_prefix(words[0]);

    if !already_routed
        && let Some(hit) = find_command_word(&words, registry)
        // A correctly-spelled command found mid-sentence is a DISPLAY-only
        // suggestion: acting on it would rewrite a question into a command.
        && (hit.was_typo || kind == Kind::Any)
    {
        // The argument is everything after the command word. Words BEFORE it are
        // dropped: in "can you define gallop" they're filler, and in "weathr
        // tokyo" the command word is first so there's nothing before it anyway.
        let args = words[hit.pos + 1..].join(" ");
        let suggestion = if args.is_empty() {
            hit.keyword.clone()
        } else {
            format!("{} {args}", hit.keyword)
        };
        // Guard: an exact hit at position 0 with no rewrite would suggest the
        // input back verbatim. (Reachable only if `already_routed` disagrees
        // with `is_known_prefix`, but cheap to rule out.)
        if !hit.was_typo && hit.pos == 0 && suggestion == lower {
            return None;
        }
        return Some(row(suggestion));
    }

    // App-name near-miss ("spoti" → Spotify). A single-word query that fuzzy-
    // matches an installed app in the SUGGEST band — confident enough to offer,
    // but below AUTO_LAUNCH_THRESHOLD so we never silently launch the wrong app.
    let first = words[0];
    if words.len() == 1
        && first.len() >= MIN_WORD_LEN
        && !already_routed
        && let Some((id, score)) = crate::desktop_apps::app_index().best_match(first)
        && (APP_SUGGEST_FLOOR..crate::desktop_apps::AUTO_LAUNCH_THRESHOLD).contains(&score)
    {
        let name = crate::desktop_apps::app_index().entry(id).name.clone();
        // Skip if the query already IS the app name — no typo to fix.
        if name.to_lowercase() != first {
            return Some(row(format!("open {name}")));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_registry::{ActionHandler, ActionResult, Trigger};
    use crate::error::LychiError;
    use async_trait::async_trait;

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
            _ctx: &crate::action_registry::ExecContext,
            _args: &str,
        ) -> Result<ActionResult, LychiError> {
            Ok(ActionResult::default())
        }
    }

    fn handler(id: &'static str, kws: &'static [&'static str]) -> Box<TestHandler> {
        Box::new(TestHandler {
            id,
            triggers: Box::leak(vec![Trigger::keywords(kws)].into_boxed_slice()),
        })
    }

    /// A registry standing in for the real one — the vocabulary this module
    /// reasons over is whatever the registry declares, nothing more.
    fn test_registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register(handler("weather", &["weather"]));
        r.register(handler("time", &["time"]));
        r.register(handler("web", &["web"]));
        r.register(handler("define", &["define"]));
        r.register(handler("screenshot", &["screenshot"]));
        r
    }

    fn suggest(raw: &str) -> Option<CompletionItem> {
        super::suggest(raw, &test_registry())
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        // Transposition is ONE edit under Damerau-Levenshtein — the property
        // that separates a real typo from a merely-nearby word.
        assert_eq!(levenshtein("time", "tmie"), 1);
        assert_eq!(levenshtein("web", "wbe"), 1);
        // An unrelated word stays expensive.
        assert_eq!(levenshtein("the", "time"), 2);
    }

    // ---- Case 1: misspelled (the original behaviour, preserved) ----

    #[test]
    fn suggest_single_word_typo() {
        let s = suggest("weathr").unwrap();
        assert!(s.label.contains("weather"), "got {}", s.label);
    }

    #[test]
    fn suggest_single_word_with_args() {
        let s = suggest("weathr in tokyo").unwrap();
        assert_eq!(s.description.unwrap(), "weather in tokyo");
    }

    #[test]
    fn suggest_phrase_typo() {
        // Previously served by a hardcoded PHRASES table; now falls out of the
        // same vocabulary lookup with no table at all.
        let s = suggest("tme in tokyo").unwrap();
        assert_eq!(s.description.unwrap(), "time in tokyo");
    }

    // ---- Case 2: naturally phrased (the new behaviour) ----

    #[test]
    fn natural_phrasing_finds_the_command_word_anywhere() {
        let s = suggest("can you define gallop").unwrap();
        assert_eq!(s.description.unwrap(), "define gallop");
    }

    #[test]
    fn filler_is_dropped_without_any_stop_word_list() {
        for (query, want) in [
            ("please define ephemeral", "define ephemeral"),
            ("i want to define serendipity", "define serendipity"),
            ("could you take a screenshot", "screenshot"),
            ("whats the weather in tokyo", "weather in tokyo"),
        ] {
            let s = suggest(query).unwrap_or_else(|| panic!("no suggestion for {query}"));
            assert_eq!(s.description.unwrap(), want, "query: {query}");
        }
    }

    #[test]
    fn sentence_punctuation_never_reaches_the_argument() {
        // REGRESSION: "can you define gallop?" looked up `gallop?` and the
        // dictionary returned "No definition found". The "?" ends the SENTENCE,
        // not the word.
        for (query, want) in [
            ("can you define gallop?", "define gallop"),
            ("can you define gallop.", "define gallop"),
            ("please define ephemeral!", "define ephemeral"),
            ("whats the weather in tokyo?", "weather in tokyo"),
        ] {
            let s = suggest(query).unwrap_or_else(|| panic!("no suggestion for {query}"));
            assert_eq!(s.description.unwrap(), want, "query: {query}");
        }
    }

    #[test]
    fn punctuation_inside_a_word_is_preserved() {
        // Only LEADING/TRAILING marks are sentence punctuation. Hyphens,
        // underscores and dotted names are part of the argument itself.
        for (query, want) in [
            ("can you define well-being", "define well-being"),
            ("can you define e.g.", "define e.g"),
        ] {
            let s = suggest(query).unwrap_or_else(|| panic!("no suggestion for {query}"));
            assert_eq!(s.description.unwrap(), want, "query: {query}");
        }
    }

    #[test]
    fn an_exact_command_word_beats_a_fuzzy_earlier_one() {
        // "can" is within edit distance 2 of several keywords; the exact hit on
        // "define" must win rather than the query being mangled.
        let s = suggest("can you define gallop").unwrap();
        assert!(s.description.unwrap().starts_with("define"));
    }

    // ---- Abstention: what must NOT be suggested ----

    #[test]
    fn no_suggest_exact_match() {
        // Already routable — the pattern matcher handles it.
        assert!(suggest("weather").is_none());
        assert!(suggest("time in tokyo").is_none());
        assert!(suggest("define gallop").is_none());
    }

    #[test]
    fn a_query_with_no_command_word_falls_through_to_ai() {
        // The load-bearing abstention: genuine questions must reach the AI/web
        // fallback, not be forced into a command.
        for q in [
            "what is the meaning of life",
            "how do i center a div",
            "xyzabc",
            "tell me a joke",
            "why is the sky blue",
        ] {
            assert!(suggest(q).is_none(), "{q} should not suggest a command");
        }
    }

    #[test]
    fn mid_sentence_words_are_not_fuzzy_matched() {
        // REGRESSION: "the" is 2 edits from "time" and "life" is 2 from "time".
        // Fuzzy-scanning a whole sentence turned ordinary prose into a command
        // suggestion ("what is the meaning of life" → "time meaning of life").
        // Fuzzy matching is first-word-only; exact matching is what scans the
        // whole query. This test pins that split.
        assert!(suggest("what is the meaning of life").is_none());
        // But an EXACT command word mid-sentence still resolves.
        assert!(suggest("can you define gallop").is_some());
    }

    #[test]
    fn typo_tolerance_scales_with_word_length() {
        // A short word gets one edit, so it can't reach across the vocabulary.
        // "web" (3 chars) must not become "weather" via a generous budget.
        assert!(
            suggest("wbe")
                .unwrap()
                .description
                .unwrap()
                .starts_with("web")
        );
        // A longer word still tolerates two.
        assert!(
            suggest("weathr")
                .unwrap()
                .description
                .unwrap()
                .starts_with("weather")
        );
    }

    #[test]
    fn no_suggest_short_input() {
        assert!(suggest("we").is_none());
    }

    // ---- The Kind split: display-only vs safe-to-rewrite ----

    #[test]
    fn typo_only_refuses_to_rewrite_a_naturally_phrased_question() {
        let r = test_registry();
        // Display: offering the row is fine, the user can ignore it.
        assert!(
            super::suggest_kind("can you define gallop", &r, Kind::Any).is_some(),
            "the completions list should still offer it"
        );
        // Routing: rewriting the input would turn a question into a command.
        assert!(
            super::suggest_kind("can you define gallop", &r, Kind::TypoOnly).is_none(),
            "the classifier must not rewrite a question"
        );
    }

    #[test]
    fn typo_only_still_corrects_an_actual_misspelling() {
        let r = test_registry();
        // A real typo is safe to act on under BOTH tolerances — that's the
        // pre-existing behaviour this rewrite had to preserve.
        for kind in [Kind::Any, Kind::TypoOnly] {
            let s = super::suggest_kind("weathr tokyo", &r, kind)
                .unwrap_or_else(|| panic!("no suggestion for {kind:?}"));
            assert_eq!(s.description.unwrap(), "weather tokyo");
        }
    }

    #[test]
    fn short_filler_words_do_not_fuzzy_match_the_vocabulary() {
        // "is"/"it" are within a small edit distance of real keywords; the
        // MIN_WORD_LEN guard is what stops them hijacking a query.
        assert!(suggest("is it ok").is_none());
    }
}
