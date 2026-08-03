//! The suggestion list: what appears below the input, in what order.
//!
//! # Why this exists
//!
//! `Executor::completions` built its list by pushing, prepending, splicing and
//! truncating one shared `Vec` across nine stages. A suggestion's POSITION was
//! an emergent property of the order the code happened to run in, and every
//! ordering rule lived as a comment beside the line that implemented it —
//! "Context matches lead", "fallbacks sort last and are never auto-selected",
//! "Prepend so the repo choices sit at the top".
//!
//! Prose cannot fail a build. So the rules drifted, and the drift stayed
//! invisible until a user typed `services` and an application launched.
//!
//! The fix is the same shape as the output rework: **producers describe, one
//! owner decides.** A stage emits [`Suggestion`]s carrying their own provenance
//! and eligibility; [`rank`] alone decides order, caps and defaultability.
//!
//! # The two properties a suggestion carries
//!
//! [`Source`] is *where it came from*, and sets coarse ordering.
//!
//! [`Tier`] is *how well it matches what was typed*, and is the consent rule
//! made data: *Lychi executes only what the user typed, or what the user
//! selected.* Only [`Tier::Identity`] and [`Tier::Prefix`] may be Enter's
//! default. Anything weaker is offered, never taken.
//!
//! That rule previously existed twice — as a prefix check in the frontend's
//! `defaultMatchIndex`, and as a ≥0.90 confidence short-circuit in
//! `intent/mod.rs`. They disagreed, which is precisely how `dnf search firefox`
//! launched Firefox while `services` refused to auto-select a legitimate match.
//! Computing it once, here, is the point.

use std::collections::HashMap;

use crate::action_registry::{CompletionItem, CompletionKind};

/// What a row DOES, normalised — the identity used for both deduping and latch
/// lookup.
///
/// These two must agree. If a latch were keyed on the label while dedupe keyed
/// on the command, a latched row could be the one dropped as a duplicate, and
/// the learned preference would silently stop applying.
fn command_key(s: &Suggestion) -> String {
    s.item
        .run
        .as_deref()
        .or(s.item.fill.as_deref())
        .unwrap_or(&s.item.label)
        .to_lowercase()
}

/// Where a suggestion came from. Sets coarse ordering, ahead of score.
///
/// Ordered by declaration: `Ord` is derived, so `Guard < Context < …` and the
/// ranker can sort on the enum directly. Adding a variant in the middle
/// deliberately changes ordering — that is the one place ordering is decided,
/// which is the improvement over nine `insert()` calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// A safety warning about the state the command would run against (a dirty
    /// checkout before a shutdown). Leads unconditionally: a warning the user
    /// reads *after* deciding is not a warning.
    Guard,
    /// A preview of what an explicitly-configured quicklink expands to. The
    /// user configured the keyword, so once it matches, it leads.
    Quicklink,
    /// A choice the user must make before anything runs — which repo a command
    /// targets, which container a verb applies to. These are not ranked
    /// alternatives; the command is ambiguous without an answer.
    Disambiguation,
    /// Derived from the live environment, carrying learned per-context ranking
    /// that a generic completion cannot reproduce.
    Context,
    /// An action handler's own completions — the ordinary case.
    Handler,
    /// A "did you mean" offer for a near-miss.
    Correction,
    /// An escape hatch ("Ask AI", "Search web"). Always available, always last,
    /// never the default. Removing these made unmatched queries a dead end;
    /// making them defaultable let them hijack Enter. Present-but-never-default
    /// is the position that is neither bug.
    Fallback,
}

impl Source {
    /// Whether this source may ever supply Enter's default.
    ///
    /// A fallback must not: it is the answer to "nothing fit", so preferring it
    /// over the user's own text inverts the meaning of pressing Enter.
    /// A guard must not: it is a warning, not an action.
    fn can_be_default(self) -> bool {
        !matches!(self, Self::Fallback | Self::Guard)
    }
}

/// How well a suggestion matches what the user actually typed.
///
/// This is the consent rule as data. Ordered strongest-first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// The typed text IS this thing — `firefox` for the Firefox app. Executing
    /// it executes what the user wrote.
    Identity,
    /// This extends the typed text — `fir` → `firefox`. Safe to auto-select:
    /// the row is visible, and the completion is what typing more would reach.
    Prefix,
    /// The typed text merely CONTAINS this — `dnf search firefox` → Firefox.
    /// Rank it, never run it. This is the tier that launched a browser when a
    /// user asked to search a package index.
    Subset,
    /// A fuzzy or semantic association. Weakest; offered only.
    Fuzzy,
}

impl Tier {
    /// Whether Enter may take this without the user selecting the row.
    ///
    /// The line sits between `Prefix` and `Subset` deliberately: everything
    /// above is the user's own text, possibly completed; everything below is
    /// the launcher's inference.
    pub fn can_be_default(self) -> bool {
        matches!(self, Self::Identity | Self::Prefix)
    }

    /// Classify a candidate against the typed input.
    ///
    /// The single implementation of the rule that the frontend and the intent
    /// resolver each used to own a copy of.
    pub fn classify(typed: &str, candidate: &str) -> Self {
        let typed = typed.trim().to_lowercase();
        let candidate = candidate.trim().to_lowercase();
        if typed.is_empty() {
            // Nothing typed means nothing to be a prefix OF. On the empty
            // prompt every row is a browsable offer, so classifying zero-state
            // rows as `Prefix` would make the first one auto-run on Enter.
            return Tier::Fuzzy;
        }
        if candidate == typed {
            Tier::Identity
        } else if candidate.starts_with(&typed) {
            Tier::Prefix
        } else if candidate.contains(&typed) || typed.contains(&candidate) {
            // BOTH containment directions are `Subset`, and both are real:
            //
            // - candidate contains typed — mid-string completion ("fox" →
            //   "firefox"). Chromium refuses to default these because matching
            //   mid-string "will mislead the user into thinking the What You
            //   Typed match is what's selected".
            // - typed contains candidate — the `dnf search firefox` shape. The
            //   query MENTIONS an app; that is a reason to offer it and never a
            //   reason to launch it.
            //
            // The second is the one that shipped a bug, and it is the direction
            // easiest to omit: "does the suggestion match what I typed" reads
            // naturally as one-directional.
            Tier::Subset
        } else {
            Tier::Fuzzy
        }
    }
}

/// One candidate row, with the provenance the ranker needs.
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub item: CompletionItem,
    pub source: Source,
    pub tier: Tier,
}

impl Suggestion {
    pub fn new(item: CompletionItem, source: Source, tier: Tier) -> Self {
        Self { item, source, tier }
    }

    /// A suggestion whose tier is derived from the typed input.
    ///
    /// Uses `run` when present, else `fill`, else `label` — the same precedence
    /// the frontend uses to decide what a row actually does, so the tier
    /// describes the thing that would execute rather than the thing displayed.
    /// (A row labelled "Search YouTube: cats" runs `yt cats`; classifying the
    /// label would be classifying prose.)
    pub fn matched(item: CompletionItem, source: Source, typed: &str) -> Self {
        let text = item
            .run
            .as_deref()
            .or(item.fill.as_deref())
            .unwrap_or(&item.label)
            .to_string();
        let tier = Tier::classify(typed, &text);
        Self::new(item, source, tier)
    }

    /// Whether Enter may take this row without the user selecting it.
    ///
    /// Both the source and the tier must permit it. A fallback that happens to
    /// prefix-match is still a fallback; a handler row that only fuzzy-matches
    /// is still a guess.
    pub fn can_be_default(&self) -> bool {
        self.source.can_be_default()
            && self.tier.can_be_default()
            && !self.item.kind.is_some_and(CompletionKind::is_fallback)
    }
}

/// How many rows the list shows before fallbacks are appended.
///
/// Researched against 11 launchers (see `zero-state-ux`): none shows a long
/// list. Beyond roughly this many the list stops being scannable and the user
/// reaches for the mouse — which is the failure mode a keyboard launcher exists
/// to avoid.
const MAX_ROWS: usize = 8;

/// Decide the final order.
///
/// The ONE place ordering is decided. Everything that used to be an `insert(0,
/// …)`, a `splice`, an `extend` or a comment is expressed here as a sort key.
///
/// Ordering is `(source, latched, tier, score descending)`:
/// - **source** first, because a category of row outranks a good match in a
///   weaker category — a safety guard beats an excellent fuzzy app hit.
/// - **latched** second: what this user picked for this exact query beats any
///   general-purpose judgement about the query. See [`rank_with_latches`].
/// - **tier** next, because the consent rule decides among unlatched rows: one
///   that extends what was typed beats one that merely contains it.
/// - **score** last, as the tiebreak that producers control.
///
/// The sort is stable, so equal keys keep producer order — a source that
/// already ranked its own output (frecency-ordered repos) keeps that order.
pub fn rank(all: Vec<Suggestion>) -> Vec<Suggestion> {
    rank_with_latches(all, &HashMap::new())
}

/// [`rank`], with the user's learned query→command bindings applied.
///
/// # Why latching sits between source and tier, and not anywhere else
///
/// A latch is evidence about **ranking**, never about **consent**. Those are
/// different questions, and conflating them would undo the fix that motivated
/// this module:
///
/// - It must **not** raise a row's [`Tier`], because tier gates what Enter may
///   auto-run. If latching could promote a `Subset` match to defaultable, then
///   picking Firefox once for `dnf search firefox` would restore the exact
///   behaviour — a query silently launching an app it merely mentions — that
///   the consent rule exists to prevent. Latching makes the right row easy to
///   *reach*; the user still chooses it.
/// - It must **not** cross a [`Source`] boundary, because sources encode
///   category truths that usage cannot overrule. No amount of picking makes a
///   fallback outrank a safety guard.
///
/// Within those two limits it dominates, and it should: "what this user chose
/// last time they typed this" is strictly better evidence than any general
/// heuristic about the query.
pub fn rank_with_latches(
    mut all: Vec<Suggestion>,
    latches: &HashMap<String, f64>,
) -> Vec<Suggestion> {
    // Resolve each row's latch strength once. Doing it inside the comparator
    // would recompute it O(n log n) times and, worse, make the sort's result
    // depend on how many times the comparator happened to be called.
    let strength = |s: &Suggestion| -> f64 {
        if latches.is_empty() {
            return 0.0;
        }
        latches.get(&command_key(s)).copied().unwrap_or(0.0)
    };
    let mut keyed: Vec<(f64, Suggestion)> = all.drain(..).map(|s| (strength(&s), s)).collect();

    keyed.sort_by(|(la, a), (lb, b)| {
        a.source
            .cmp(&b.source)
            // Descending: a stronger latch ranks earlier. `total_cmp` because
            // these are f64 and `partial_cmp` would need an unwrap that a NaN
            // could trip.
            .then(lb.total_cmp(la))
            .then(a.tier.cmp(&b.tier))
            .then(b.item.score.cmp(&a.item.score))
    });

    let mut all: Vec<Suggestion> = keyed.into_iter().map(|(_, s)| s).collect();

    // Dedupe by what a row DOES, not by how it reads. Two sources proposing the
    // same command is normal (a context match and a handler completion both
    // offering `open /home/u/lychi`); showing it twice is not. Keyed on the
    // effective command so differently-worded rows for one action collapse.
    //
    // AFTER the sort, deliberately: `retain` keeps the first occurrence, so
    // sorting first means the survivor is the best-ranked duplicate rather than
    // whichever source happened to run earliest.
    let mut seen: Vec<String> = Vec::new();
    all.retain(|s| {
        let key = command_key(s);
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });

    // Cap the body, then re-append fallbacks. Truncating the whole list would
    // let a long run of handler rows push the escape hatches off the end —
    // which is how an unmatched query became a dead end before.
    let (mut body, fallbacks): (Vec<_>, Vec<_>) =
        all.into_iter().partition(|s| s.source != Source::Fallback);
    body.truncate(MAX_ROWS);
    body.extend(fallbacks);
    body
}

/// The index Enter defaults to, or `None` when Enter should run the typed text.
///
/// Returning `None` is a real answer, not a failure: if nothing sufficiently
/// matches, the correct behaviour is to run what the user wrote rather than the
/// launcher's best guess.
pub fn default_index(ranked: &[Suggestion]) -> Option<usize> {
    ranked.iter().position(Suggestion::can_be_default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, score: u16) -> CompletionItem {
        CompletionItem::new(label, None, score)
    }

    fn sugg(label: &str, source: Source, tier: Tier, score: u16) -> Suggestion {
        Suggestion::new(item(label, score), source, tier)
    }

    // ── The consent rule ────────────────────────────────────────────────

    #[test]
    fn typed_text_itself_is_identity() {
        assert_eq!(Tier::classify("firefox", "firefox"), Tier::Identity);
        // Case and surrounding space are noise, not meaning.
        assert_eq!(Tier::classify("  FireFox ", "firefox"), Tier::Identity);
    }

    #[test]
    fn an_extension_of_the_typed_text_is_prefix() {
        assert_eq!(Tier::classify("fir", "firefox"), Tier::Prefix);
    }

    /// The `dnf search firefox` case: the QUERY contains an app name, which is
    /// a reason to offer it and never a reason to launch it.
    ///
    /// Note the argument order — `classify(typed, candidate)`. Getting this
    /// backwards is how the direction that actually shipped a bug went
    /// unasserted the first time this test was written.
    #[test]
    fn a_query_that_merely_mentions_a_match_is_only_subset() {
        assert_eq!(
            Tier::classify("dnf search firefox", "firefox"),
            Tier::Subset
        );
        assert!(!Tier::Subset.can_be_default());
    }

    /// The other containment direction: mid-string completion. Chromium
    /// refuses to default these for the same reason.
    #[test]
    fn a_mid_string_match_is_only_subset() {
        assert_eq!(Tier::classify("fox", "firefox"), Tier::Subset);
    }

    #[test]
    fn only_identity_and_prefix_may_be_default() {
        assert!(Tier::Identity.can_be_default());
        assert!(Tier::Prefix.can_be_default());
        assert!(!Tier::Subset.can_be_default());
        assert!(!Tier::Fuzzy.can_be_default());
    }

    /// On the empty prompt there is nothing to be a prefix of, so no row may
    /// auto-run — otherwise summoning the launcher and hitting Enter would fire
    /// whatever happened to rank first.
    #[test]
    fn empty_input_makes_everything_non_default() {
        assert_eq!(Tier::classify("", "firefox"), Tier::Fuzzy);
        assert!(!Tier::classify("", "firefox").can_be_default());
    }

    /// Tier is classified against what the row RUNS, not what it displays.
    #[test]
    fn tier_is_classified_against_the_command_not_the_label() {
        let it = CompletionItem::new("Search YouTube: cats", None, 50).with_run("yt cats");
        // The label doesn't start with "yt", the command does.
        assert_eq!(
            Suggestion::matched(it, Source::Handler, "yt").tier,
            Tier::Prefix
        );
    }

    // ── Ordering ────────────────────────────────────────────────────────

    /// The invariants that used to be comments, asserted as one ordering.
    #[test]
    fn sources_order_guard_context_handler_fallback() {
        let ranked = rank(vec![
            sugg("web", Source::Fallback, Tier::Fuzzy, 1),
            sugg("handler", Source::Handler, Tier::Prefix, 90),
            sugg("guard", Source::Guard, Tier::Fuzzy, 5),
            sugg("context", Source::Context, Tier::Subset, 10),
        ]);
        let labels: Vec<&str> = ranked.iter().map(|s| s.item.label.as_str()).collect();
        assert_eq!(labels, ["guard", "context", "handler", "web"]);
    }

    /// A high-scoring handler row must not outrank a context match. Score is
    /// the LAST key, not the first — the bug being prevented is a generic
    /// completion burying a learned, context-specific one.
    #[test]
    fn score_never_overrides_source() {
        let ranked = rank(vec![
            sugg("handler", Source::Handler, Tier::Identity, 999),
            sugg("context", Source::Context, Tier::Fuzzy, 1),
        ]);
        assert_eq!(ranked[0].item.label, "context");
    }

    #[test]
    fn within_a_source_a_stronger_tier_wins() {
        let ranked = rank(vec![
            sugg("subset", Source::Handler, Tier::Subset, 100),
            sugg("prefix", Source::Handler, Tier::Prefix, 1),
        ]);
        assert_eq!(ranked[0].item.label, "prefix");
    }

    #[test]
    fn within_a_tier_a_higher_score_wins() {
        let ranked = rank(vec![
            sugg("low", Source::Handler, Tier::Prefix, 10),
            sugg("high", Source::Handler, Tier::Prefix, 90),
        ]);
        assert_eq!(ranked[0].item.label, "high");
    }

    // ── Fallbacks ───────────────────────────────────────────────────────

    /// Fallbacks survive the cap. Truncating the whole list would drop the
    /// escape hatches exactly when a long list of poor matches makes them most
    /// useful — the dead-end bug, re-created by a `truncate`.
    #[test]
    fn fallbacks_survive_truncation() {
        let mut all: Vec<Suggestion> = (0..30)
            .map(|i| sugg(&format!("row{i}"), Source::Handler, Tier::Prefix, 50))
            .collect();
        all.push(sugg("web", Source::Fallback, Tier::Fuzzy, 1));

        let ranked = rank(all);
        assert_eq!(ranked.len(), MAX_ROWS + 1, "body capped, fallback kept");
        assert_eq!(ranked.last().unwrap().item.label, "web");
    }

    #[test]
    fn a_fallback_is_never_the_default_even_if_it_prefix_matches() {
        let ranked = rank(vec![sugg("web thing", Source::Fallback, Tier::Prefix, 1)]);
        assert_eq!(
            default_index(&ranked),
            None,
            "a fallback must never take Enter"
        );
    }

    /// A guard is a warning, not an action — it leads the list but Enter must
    /// skip past it to the real suggestion.
    #[test]
    fn a_guard_leads_but_is_not_the_default() {
        let ranked = rank(vec![
            sugg("real", Source::Handler, Tier::Prefix, 50),
            sugg("⚠ dirty", Source::Guard, Tier::Prefix, 200),
        ]);
        assert_eq!(ranked[0].item.label, "⚠ dirty", "the warning leads");
        assert_eq!(
            ranked[default_index(&ranked).unwrap()].item.label,
            "real",
            "but Enter takes the actionable row"
        );
    }

    #[test]
    fn nothing_defaultable_means_run_what_was_typed() {
        let ranked = rank(vec![
            sugg("a", Source::Handler, Tier::Subset, 90),
            sugg("b", Source::Fallback, Tier::Fuzzy, 1),
        ]);
        assert_eq!(default_index(&ranked), None);
    }

    // ── Latching ────────────────────────────────────────────────────────

    fn latches(pairs: &[(&str, f64)]) -> HashMap<String, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    /// The Alfred property: what this user picked for this query beats the
    /// launcher's own judgement about the query.
    #[test]
    fn a_latched_row_leads_its_source_group() {
        let ranked = rank_with_latches(
            vec![
                sugg("popular", Source::Handler, Tier::Prefix, 99),
                sugg("chosen", Source::Handler, Tier::Fuzzy, 1),
            ],
            &latches(&[("chosen", 0.5)]),
        );
        assert_eq!(
            ranked[0].item.label, "chosen",
            "a weak-tier, low-score row the user actually picks must win"
        );
    }

    /// THE critical property. Latching must not make a subset match
    /// defaultable: picking Firefox once for `dnf search firefox` would
    /// otherwise restore the exact auto-launch bug the consent rule prevents.
    #[test]
    fn a_latch_never_makes_a_subset_match_defaultable() {
        let ranked = rank_with_latches(
            vec![sugg("firefox", Source::Handler, Tier::Subset, 92)],
            &latches(&[("firefox", 1.0)]),
        );
        assert_eq!(ranked[0].item.label, "firefox", "it still leads");
        assert_eq!(
            default_index(&ranked),
            None,
            "but a latch is evidence about RANK, never about consent"
        );
    }

    /// Nor may a latch lift a row out of its source category — usage cannot
    /// overrule a safety guard or promote a fallback.
    #[test]
    fn a_latch_never_crosses_a_source_boundary() {
        // Between two NON-partitioned sources. A first version used Guard vs
        // Fallback and passed even with latch sorted ahead of source — because
        // `rank` partitions fallbacks to the end regardless of sort order, so
        // the fixture was measuring the partition, not the comparator.
        let ranked = rank_with_latches(
            vec![
                sugg("context", Source::Context, Tier::Fuzzy, 1),
                sugg("handler", Source::Handler, Tier::Prefix, 99),
            ],
            &latches(&[("handler", 1.0)]),
        );
        assert_eq!(
            ranked[0].item.label, "context",
            "source is the outer key: a latch reorders WITHIN a source, never across"
        );
    }

    /// The partitioned case is worth its own test, since it is a different
    /// mechanism: a latched fallback stays last and stays non-defaultable.
    #[test]
    fn a_latched_fallback_is_still_last_and_never_default() {
        let ranked = rank_with_latches(
            vec![
                sugg("real", Source::Handler, Tier::Prefix, 1),
                sugg("web", Source::Fallback, Tier::Prefix, 1),
            ],
            &latches(&[("web", 1.0)]),
        );
        assert_eq!(ranked.last().unwrap().item.label, "web");
        assert_eq!(
            ranked[default_index(&ranked).unwrap()].item.label,
            "real",
            "Enter must still take the real row"
        );
    }

    #[test]
    fn a_stronger_latch_outranks_a_weaker_one() {
        let ranked = rank_with_latches(
            vec![
                sugg("weak", Source::Handler, Tier::Prefix, 50),
                sugg("strong", Source::Handler, Tier::Prefix, 50),
            ],
            &latches(&[("weak", 0.2), ("strong", 0.9)]),
        );
        assert_eq!(ranked[0].item.label, "strong");
    }

    /// With no latches the ordering must be byte-identical to plain `rank`,
    /// so the feature is inert for a new user.
    #[test]
    fn no_latches_leaves_ordering_unchanged() {
        let mk = || {
            vec![
                sugg("a", Source::Handler, Tier::Subset, 10),
                sugg("b", Source::Context, Tier::Fuzzy, 90),
                sugg("c", Source::Handler, Tier::Prefix, 5),
            ]
        };
        let plain: Vec<String> = rank(mk()).iter().map(|s| s.item.label.clone()).collect();
        let latched: Vec<String> = rank_with_latches(mk(), &HashMap::new())
            .iter()
            .map(|s| s.item.label.clone())
            .collect();
        assert_eq!(plain, latched);
    }

    /// A latch for a command that isn't on offer must not perturb anything.
    #[test]
    fn an_irrelevant_latch_is_ignored() {
        let ranked = rank_with_latches(
            vec![
                sugg("a", Source::Handler, Tier::Prefix, 90),
                sugg("b", Source::Handler, Tier::Prefix, 10),
            ],
            &latches(&[("something else", 1.0)]),
        );
        assert_eq!(ranked[0].item.label, "a");
    }

    /// Latch lookup and dedupe must key on the same thing, or a latched row
    /// could be the copy that gets dropped.
    #[test]
    fn a_latch_matches_the_command_not_the_label() {
        let it = CompletionItem::new("Search YouTube: cats", None, 10).with_run("yt cats");
        let ranked = rank_with_latches(
            vec![
                Suggestion::new(it, Source::Handler, Tier::Fuzzy),
                sugg("other", Source::Handler, Tier::Prefix, 90),
            ],
            &latches(&[("yt cats", 0.8)]),
        );
        assert_eq!(ranked[0].item.label, "Search YouTube: cats");
    }

    // ── Dedupe ──────────────────────────────────────────────────────────

    #[test]
    fn the_same_command_from_two_sources_appears_once() {
        let ctx = CompletionItem::new("Open project root", None, 91).with_run("open /home/u/l");
        let handler = CompletionItem::new("open /home/u/l", None, 50);
        let ranked = rank(vec![
            Suggestion::new(handler, Source::Handler, Tier::Prefix),
            Suggestion::new(ctx, Source::Context, Tier::Prefix),
        ]);
        assert_eq!(ranked.len(), 1);
        // The stronger source is the one kept.
        assert_eq!(ranked[0].source, Source::Context);
    }

    #[test]
    fn different_commands_are_both_kept() {
        let ranked = rank(vec![
            sugg("open a", Source::Handler, Tier::Prefix, 50),
            sugg("open b", Source::Handler, Tier::Prefix, 50),
        ]);
        assert_eq!(ranked.len(), 2);
    }
}
