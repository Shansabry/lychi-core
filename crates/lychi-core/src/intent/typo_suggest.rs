//! Fuzzy typo correction for near-miss inputs.
//!
//! When the pattern router falls through to "open"/"web" and no completions match,
//! this module checks for close matches against known keywords using Levenshtein distance.

use crate::action_registry::CompletionItem;

/// Minimum Levenshtein distance to accept as a suggestion.
/// Distance 1 = single typo (e.g., "tmie" → "time").
/// Distance 2 = two typos (e.g., "weathr" → "weather").
const MAX_DISTANCE: usize = 2;

/// Minimum word length to consider for typo matching (skip very short inputs).
const MIN_WORD_LEN: usize = 3;

/// Known multi-word patterns (checked as full phrases after lowering).
const PHRASES: &[(&str, &str)] = &[
    ("time in", "time in <city>"),
    ("weather in", "weather in <city>"),
    ("remind me", "remind me to <text> in <time>"),
    ("timer start", "timer start <duration>"),
    ("set a timer", "set a timer for <duration>"),
    ("volume up", "volume up"),
    ("volume down", "volume down"),
    ("brightness up", "brightness up"),
    ("brightness down", "brightness down"),
    ("lock screen", "lock screen"),
    ("wifi on", "wifi on"),
    ("wifi off", "wifi off"),
    ("bluetooth on", "bluetooth on"),
    ("bluetooth off", "bluetooth off"),
    ("pause all", "pause all"),
    ("clip search", "clip <query>"),
];

/// Levenshtein edit distance (standard DP, no allocations beyond stack for small strings).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();

    // Early exits
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    // Single-row DP
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1) // deletion
                .min(curr[j - 1] + 1) // insertion
                .min(prev[j - 1] + cost); // substitution
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

/// Try to find a "Did you mean: X?" suggestion for a near-miss input.
///
/// Returns a `CompletionItem` if a close match is found, or `None`.
pub fn suggest(raw: &str) -> Option<CompletionItem> {
    let lower = raw.trim().to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();

    if words.is_empty() || lower.len() < MIN_WORD_LEN {
        return None;
    }

    // 1. Check multi-word phrases first (e.g., "tmie in tokyo" → "time in tokyo")
    if let Some(item) = words.len().ge(&2).then(|| suggest_phrase(&words)).flatten() {
        return Some(item);
    }

    // 2. Check first word against single keywords (e.g., "weathr" → "weather")
    //    Skip if the first word is already a known command/keyword (no typo to fix).
    let first = words[0];
    let is_known = super::patterns::is_known_prefix(first);
    if first.len() >= MIN_WORD_LEN && !is_known {
        let mut best: Option<(&str, usize)> = None;
        for &kw in super::patterns::KNOWN_PREFIXES {
            let dist = levenshtein(first, kw);
            if dist > 0 && dist <= MAX_DISTANCE && (best.is_none() || dist < best.unwrap().1) {
                best = Some((kw, dist));
            }
        }
        if let Some((kw, _)) = best {
            // Reconstruct the suggestion: replace the typo'd first word with the correct one
            let rest = if words.len() > 1 {
                format!(" {}", words[1..].join(" "))
            } else {
                String::new()
            };
            let suggestion = format!("{kw}{rest}");
            return Some(CompletionItem {
                label: format!("Did you mean: {suggestion}?"),
                icon_path: Some("__none__".to_string()),
                score: 90,
                description: Some(suggestion),
                reason: None,
            });
        }
    }

    None
}

/// Check if the input is a near-miss of a known multi-word phrase.
fn suggest_phrase(words: &[&str]) -> Option<CompletionItem> {
    for &(pattern, _hint) in PHRASES {
        let pattern_words: Vec<&str> = pattern.split_whitespace().collect();
        if words.len() < pattern_words.len() {
            continue;
        }

        // Compare each word in the pattern against the input words
        let mut total_dist = 0usize;
        let mut any_typo = false;
        for (input_word, pattern_word) in words.iter().zip(pattern_words.iter()) {
            let dist = levenshtein(input_word, pattern_word);
            total_dist += dist;
            if dist > 0 {
                any_typo = true;
            }
            if total_dist > MAX_DISTANCE {
                break;
            }
        }

        if any_typo && total_dist <= MAX_DISTANCE {
            // Reconstruct: use pattern words + remaining input words
            let mut suggestion = pattern.to_string();
            if words.len() > pattern_words.len() {
                suggestion.push(' ');
                suggestion.push_str(&words[pattern_words.len()..].join(" "));
            }
            return Some(CompletionItem {
                label: format!("Did you mean: {suggestion}?"),
                icon_path: Some("__none__".to_string()),
                score: 90,
                description: Some(suggestion),
                reason: None,
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("time", "tmie"), 2); // transposition = 2 edits in standard Levenshtein
    }

    #[test]
    fn suggest_single_word_typo() {
        let s = suggest("weathr").unwrap();
        assert!(s.label.contains("weather"));
    }

    #[test]
    fn suggest_single_word_with_args() {
        let s = suggest("weathr in tokyo").unwrap();
        assert!(s.label.contains("weather in tokyo"));
    }

    #[test]
    fn suggest_phrase_typo() {
        let s = suggest("tme in tokyo").unwrap();
        assert!(s.label.contains("time in tokyo"));
    }

    #[test]
    fn no_suggest_exact_match() {
        assert!(suggest("weather").is_none());
        assert!(suggest("time in tokyo").is_none());
    }

    #[test]
    fn no_suggest_too_different() {
        assert!(suggest("xyzabc").is_none());
    }

    #[test]
    fn no_suggest_short_input() {
        assert!(suggest("we").is_none());
    }
}
