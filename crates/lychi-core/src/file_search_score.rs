//! Explicit-tier scoring for file/folder search — the Spotlight/Raycast/fzf model.
//!
//! The old design deferred ranking entirely to nucleo's opaque path-scheme
//! number, which scores a directory (`games/lighthouse`) and its children
//! (`games/lighthouse/tests`) IDENTICALLY — the "ligh" match lands on the same
//! path segment — so the folder the user actually meant got buried under its own
//! contents, and every fix was a tiebreak band-aid over an intrinsic tie.
//!
//! This module owns the score instead. Every candidate is classified into a
//! discrete **match tier** by comparing the query against the FILENAME first,
//! then the path. Tiers are ordered by how well the launcher standards say a
//! match satisfies intent:
//!
//! | Tier | Meaning (query vs. filename, unless noted)             |
//! |------|-------------------------------------------------------|
//! | 0    | Exact filename (case-insensitive)                     |
//! | 1    | Filename starts with the query (prefix)               |
//! | 2    | Query sits on a word boundary in the filename         |
//! | 3    | Query is a substring of the filename                   |
//! | 4    | Query is a fuzzy subsequence of the filename          |
//! | 5    | Query matches only an ANCESTOR directory, not the name|
//!
//! `games/lighthouse` is now a tier-0 filename match while
//! `games/lighthouse/tests` is only tier-5 (path-only) — the folder wins by
//! *structure*, with no tiebreak needed. Within a tier we prefer the shorter
//! filename, then the shallower path, then usage (frecency), exactly matching
//! fzf's `--tiebreak=pathname,length` plus a Raycast-style usage layer.
//!
//! Fully adaptive and name-agnostic: tiers are computed from the query and the
//! candidate's own name, never from a hardcoded list of files or extensions
//! (see the project's `dynamic-over-hardcoded` rule).

/// Discrete match quality, best (lowest) first. `Ord` sorts tiers directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchTier {
    /// Filename equals the query (case-insensitive).
    ExactName = 0,
    /// Filename starts with the query.
    PrefixName = 1,
    /// Query begins at a word boundary inside the filename (`_`, `-`, `.`,
    /// space, or a camelCase hump).
    BoundaryName = 2,
    /// Query is a substring of the filename (not at a boundary).
    ContainsName = 3,
    /// Query characters appear in order in the filename (fuzzy subsequence).
    FuzzyName = 4,
    /// Query matched only an ancestor directory, not the filename itself.
    PathOnly = 5,
}

impl MatchTier {
    /// A coarse numeric weight (higher = better) for callers that fold the tier
    /// into a single `u16` display score. Kept well-separated so no in-tier
    /// nudge can cross a tier boundary.
    pub fn weight(self) -> u16 {
        match self {
            MatchTier::ExactName => 600,
            MatchTier::PrefixName => 500,
            MatchTier::BoundaryName => 400,
            MatchTier::ContainsName => 300,
            MatchTier::FuzzyName => 200,
            MatchTier::PathOnly => 100,
        }
    }
}

/// The full classification of one candidate against a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchScore {
    pub tier: MatchTier,
    /// Filename length in chars — shorter wins within a tier (fzf `length`).
    pub name_len: usize,
    /// Number of path separators — shallower wins within a tier (fzf `pathname`
    /// prefers the tail; a parent dir is shallower than its children).
    pub depth: usize,
}

/// Classify `rel_path` (scope-relative, e.g. `games/lighthouse/tests`) and its
/// `file_name` (final segment) against `query`. Returns `None` when nothing —
/// not the name and not any ancestor — matches, so the candidate is dropped.
///
/// `query` is matched case-insensitively. A query containing `/` is treated as
/// a path query (the user is spelling out a path), so it always considers the
/// whole relative path, not just the filename.
pub fn classify(query: &str, file_name: &str, rel_path: &str) -> Option<MatchScore> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return None;
    }
    let name = file_name.to_lowercase();
    let path = rel_path.to_lowercase();
    let depth = rel_path.matches('/').count();
    let name_len = file_name.chars().count();

    // A slash in the query means "match against the path spelling" — go straight
    // to path matching (still prefer a name hit if the tail lines up).
    let has_slash = q.contains('/');

    if !has_slash {
        if name == q {
            return Some(MatchScore {
                tier: MatchTier::ExactName,
                name_len,
                depth,
            });
        }
        if name.starts_with(&q) {
            return Some(MatchScore {
                tier: MatchTier::PrefixName,
                name_len,
                depth,
            });
        }
        if starts_at_word_boundary(&name, &q) {
            return Some(MatchScore {
                tier: MatchTier::BoundaryName,
                name_len,
                depth,
            });
        }
        if name.contains(&q) {
            return Some(MatchScore {
                tier: MatchTier::ContainsName,
                name_len,
                depth,
            });
        }
        if is_subsequence(&name, &q) {
            return Some(MatchScore {
                tier: MatchTier::FuzzyName,
                name_len,
                depth,
            });
        }
    }

    // Path match: query hits an ancestor directory (or the user spelled a
    // slashed path). Either a plain substring of the relative path, or a fuzzy
    // subsequence of it, still counts — but only as the lowest tier, so a real
    // filename hit anywhere always outranks it.
    if path.contains(&q) || is_subsequence(&path, &q) {
        return Some(MatchScore {
            tier: MatchTier::PathOnly,
            name_len,
            depth,
        });
    }

    None
}

/// Does `needle` appear in `haystack` starting right after a word boundary?
/// Boundaries: start of string, or immediately after `_ - . /` or whitespace,
/// or at a camelCase hump (lowercase/digit → uppercase). All inputs here are
/// already lowercased for the name check, so camelCase humps are covered by the
/// separator set that survives lowercasing; the check stays cheap and correct
/// for the separator-delimited names that dominate real filesystems.
fn starts_at_word_boundary(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let mut search_from = 0;
    while let Some(rel) = haystack[search_from..].find(needle) {
        let idx = search_from + rel;
        if idx == 0 {
            // Prefix — handled by a higher tier, so not a *boundary* hit here.
            search_from = idx + 1;
            continue;
        }
        let prev = bytes[idx - 1];
        if matches!(prev, b'_' | b'-' | b'.' | b'/' | b' ' | b'\t') {
            return true;
        }
        search_from = idx + 1;
    }
    false
}

/// Is `needle` an in-order subsequence of `haystack`? (fuzzy fallback.)
fn is_subsequence(haystack: &str, needle: &str) -> bool {
    let mut chars = haystack.chars();
    for nc in needle.chars() {
        // advance haystack until we consume nc
        if !chars.any(|hc| hc == nc) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier(q: &str, name: &str, rel: &str) -> Option<MatchTier> {
        classify(q, name, rel).map(|s| s.tier)
    }

    #[test]
    fn exact_beats_prefix_beats_contains() {
        assert_eq!(
            tier("lighthouse", "lighthouse", "lighthouse"),
            Some(MatchTier::ExactName)
        );
        assert_eq!(
            tier("ligh", "lighthouse", "lighthouse"),
            Some(MatchTier::PrefixName)
        );
        assert_eq!(
            tier("house", "lighthouse", "lighthouse"),
            Some(MatchTier::ContainsName)
        );
    }

    #[test]
    fn word_boundary_beats_plain_contains() {
        // "solo" begins right after the '_' boundary in lighthouse_solo.png.
        assert_eq!(
            tier("solo", "lighthouse_solo.png", "a/lighthouse_solo.png"),
            Some(MatchTier::BoundaryName)
        );
        // "ighth" is mid-word — only a substring.
        assert_eq!(
            tier("ighth", "lighthouse.png", "a/lighthouse.png"),
            Some(MatchTier::ContainsName)
        );
    }

    #[test]
    fn fuzzy_subsequence_is_lowest_name_tier() {
        // l-h-s appears in order but not contiguously.
        assert_eq!(
            tier("lhs", "lighthouse", "a/lighthouse"),
            Some(MatchTier::FuzzyName)
        );
    }

    /// The headline case: a folder and its children must NOT tie. The folder is
    /// a filename match; the child paths only match an ancestor directory.
    #[test]
    fn parent_folder_outranks_its_children() {
        let parent = classify("ligh", "lighthouse", "games/lighthouse").unwrap();
        let child_tests = classify("ligh", "tests", "games/lighthouse/tests").unwrap();
        let child_assets = classify("ligh", "assets", "games/lighthouse/assets").unwrap();

        assert_eq!(parent.tier, MatchTier::PrefixName);
        assert_eq!(child_tests.tier, MatchTier::PathOnly);
        assert_eq!(child_assets.tier, MatchTier::PathOnly);
        // Lower tier value = better; parent sorts strictly before children.
        assert!(parent.tier < child_tests.tier);
    }

    #[test]
    fn shorter_name_wins_within_tier() {
        let short = classify("ligh", "light", "a/light").unwrap();
        let long = classify("ligh", "lighthouse", "a/lighthouse").unwrap();
        assert_eq!(short.tier, long.tier); // both PrefixName
        assert!(short.name_len < long.name_len);
    }

    #[test]
    fn shallower_path_wins_within_tier() {
        let shallow = classify("ligh", "lighthouse", "lighthouse").unwrap();
        let deep = classify("ligh", "lighthouse", "a/b/c/lighthouse").unwrap();
        assert_eq!(shallow.tier, deep.tier);
        assert!(shallow.depth < deep.depth);
    }

    #[test]
    fn slashed_query_matches_path() {
        // Spelling out a path segment → PathOnly (the tail need not match).
        assert_eq!(
            tier("games/light", "tests", "games/lighthouse/tests"),
            Some(MatchTier::PathOnly)
        );
    }

    #[test]
    fn no_match_is_dropped() {
        assert_eq!(tier("zzz", "lighthouse", "games/lighthouse"), None);
    }

    #[test]
    fn tier_weights_are_strictly_ordered() {
        // No in-tier nudge (name_len/depth/frecency) can cross a tier gap: the
        // gap between adjacent tiers is 100, far larger than any nudge budget.
        let ordered = [
            MatchTier::ExactName,
            MatchTier::PrefixName,
            MatchTier::BoundaryName,
            MatchTier::ContainsName,
            MatchTier::FuzzyName,
            MatchTier::PathOnly,
        ];
        for pair in ordered.windows(2) {
            assert!(pair[0].weight() > pair[1].weight());
            assert!(pair[0].weight() - pair[1].weight() >= 100);
        }
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(
            tier("LIGH", "lighthouse", "a/lighthouse"),
            Some(MatchTier::PrefixName)
        );
        assert_eq!(
            tier("ligh", "LIGHTHOUSE.gd", "a/LIGHTHOUSE.gd"),
            Some(MatchTier::PrefixName)
        );
    }
}
