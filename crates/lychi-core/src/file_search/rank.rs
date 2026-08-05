//! The one ranking definition, shared by every file-search surface.
//!
//! nucleo is a candidate *generator*: its path-scheme score narrows ~160k paths
//! to the matching set, but it ties a folder with its own children, so final
//! order is ours. That ordering is a product decision — which of two matches a
//! person meant — and it must not differ between surfaces. `/` search and the
//! `@` reference asking the same question have to get the same answer.
//!
//! Previously they each had their own copy of this: classify, frecency bonus,
//! description, and a five-key sort, duplicated between `emit_index_results` and
//! `fuzzy_path_completions`, with comments in both claiming to match the other.
//! Two copies of a ranking rule drift silently — nothing fails, the two surfaces
//! just quietly disagree — so they are one function here.

use std::path::Path;

use super::corpus::{PathData, SharedPath};
use crate::file_search_score::{MatchScore, classify};

/// A candidate that survived classification, with everything ranking needs.
pub struct Ranked {
    pub data: SharedPath,
    pub score: MatchScore,
    pub bonus: u16,
    /// Type hint for the UI: `Folder`, or the uppercased extension.
    pub description: Option<String>,
}

impl Ranked {
    pub fn is_dir(&self) -> bool {
        self.data.is_dir()
    }
}

/// Classify and rank `candidates` for `query`, best first.
///
/// Drops anything the tier classifier rejects — a query matching neither the
/// filename nor any ancestor directory — which is a real guard rather than a
/// formality: nucleo matches the whole relative path, so a query can score
/// against mid-path noise the user never meant.
///
/// Ordering (fzf's `--tiebreak=pathname,length` plus a Raycast usage layer):
///   1. tier — a filename match always beats a path-only one. Discrete, so no
///      amount of usage can promote a path-only hit above a name hit. This is
///      the structural fix for a folder tying with its own children.
///   2. shorter filename
///   3. shallower path
///   4. more used (frecency + recency)
///   5. label, so equal candidates never reorder between keystrokes
pub fn rank<F>(query: &str, candidates: Vec<SharedPath>, bonus_for: F) -> Vec<Ranked>
where
    F: Fn(&PathData) -> u16,
{
    let mut rows: Vec<Ranked> = candidates
        .into_iter()
        .filter_map(|data| {
            let score = classify(query, data.file_name(), data.rel_path())?;
            let bonus = bonus_for(&data);
            let description = describe(&data);
            Some(Ranked {
                data,
                score,
                bonus,
                description,
            })
        })
        .collect();
    rows.sort_by(compare);
    rows
}

/// The comparator, exposed so a caller ranking pre-split groups (folders and
/// files separately) uses the same ordering rather than restating it.
pub fn compare(a: &Ranked, b: &Ranked) -> std::cmp::Ordering {
    a.score
        .tier
        .cmp(&b.score.tier) // lower tier value = better match
        .then_with(|| a.score.name_len.cmp(&b.score.name_len))
        .then_with(|| a.score.depth.cmp(&b.score.depth))
        .then_with(|| b.bonus.cmp(&a.bonus)) // more used first
        .then_with(|| a.data.full_path().cmp(&b.data.full_path())) // stable
}

/// The UI's type hint: `Folder`, or the uppercased extension when it looks like
/// one. Length- and self-guarded so `.gitignore` or `archive.tar.gz.part` don't
/// produce nonsense labels.
fn describe(d: &PathData) -> Option<String> {
    if d.is_dir() {
        return Some("Folder".to_string());
    }
    d.file_name()
        .rsplit('.')
        .next()
        .filter(|ext| !ext.is_empty() && ext.len() < 6 && *ext != d.file_name())
        .map(|ext| ext.to_uppercase())
}

/// Split ranked rows into (folders, files), each capped at `per_group`.
///
/// Ranking the two groups independently lets each fill its own section without
/// one starving the other — an all-folders query still shows folders rather than
/// losing them to 25 better-scoring files.
pub fn split_groups(rows: Vec<Ranked>, per_group: usize) -> (Vec<Ranked>, Vec<Ranked>) {
    let (mut folders, mut files): (Vec<Ranked>, Vec<Ranked>) =
        rows.into_iter().partition(Ranked::is_dir);
    folders.truncate(per_group);
    files.truncate(per_group);
    (folders, files)
}

/// Display label for a result, `~`-relative when it is under home.
pub fn display_label(d: &PathData, home: Option<&Path>) -> String {
    super::search_display_label(Path::new(&d.full_path()), d.is_dir(), home)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one path under the `/h` scope. `full` and `name` are no longer
    /// stored separately — the arena derives both from `rel` — so they are
    /// asserted here instead, which keeps the old call sites readable and
    /// checks the derivation on every test path.
    fn path(full: &str, name: &str, rel: &str, is_dir: bool) -> SharedPath {
        let p = super::super::corpus::arena_from("/h", &[(rel, is_dir)])
            .pop()
            .expect("one path in, one out");
        assert_eq!(p.full_path(), full, "full_path derivation");
        assert_eq!(p.file_name(), name, "file_name derivation");
        p
    }

    fn names(rows: &[Ranked]) -> Vec<&str> {
        rows.iter().map(|r| r.data.file_name()).collect()
    }

    /// A filename match must outrank a path-only match regardless of usage —
    /// the tier is discrete for exactly this reason.
    #[test]
    fn filename_match_beats_path_only_match() {
        let rows = rank(
            "readme",
            vec![
                path(
                    "/h/readme/other.txt",
                    "other.txt",
                    "readme/other.txt",
                    false,
                ),
                path("/h/docs/readme.md", "readme.md", "docs/readme.md", false),
            ],
            // Give the path-only hit maximum usage; it must still lose.
            |d| {
                if d.file_name() == "other.txt" {
                    u16::MAX
                } else {
                    0
                }
            },
        );
        assert_eq!(
            names(&rows).first(),
            Some(&"readme.md"),
            "{:?}",
            names(&rows)
        );
    }

    /// Non-matches are dropped, not ranked last.
    #[test]
    fn non_matches_are_dropped() {
        let rows = rank(
            "readme",
            vec![path("/h/a/nope.txt", "nope.txt", "a/nope.txt", false)],
            |_| 0,
        );
        assert!(rows.is_empty(), "{:?}", names(&rows));
    }

    /// Equal candidates keep a stable order, so rows don't jitter between
    /// keystrokes.
    #[test]
    fn ordering_is_stable_for_equal_candidates() {
        let build = || {
            vec![
                path("/h/b/readme.md", "readme.md", "b/readme.md", false),
                path("/h/a/readme.md", "readme.md", "a/readme.md", false),
            ]
        };
        let first = rank("readme", build(), |_| 0);
        let second = rank("readme", build(), |_| 0);
        let paths = |r: &[Ranked]| {
            r.iter()
                .map(|x| x.data.full_path().clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(paths(&first), paths(&second));
    }

    /// Usage breaks ties only within the same tier.
    #[test]
    fn usage_breaks_ties_within_a_tier() {
        let rows = rank(
            "readme",
            vec![
                path("/h/a/readme.md", "readme.md", "a/readme.md", false),
                path("/h/b/readme.md", "readme.md", "b/readme.md", false),
            ],
            |d| {
                if d.full_path().starts_with("/h/b") {
                    500
                } else {
                    0
                }
            },
        );
        assert_eq!(
            rows.first().map(|r| r.data.full_path()).as_deref(),
            Some("/h/b/readme.md"),
            "more-used file should win an otherwise exact tie"
        );
    }

    #[test]
    fn descriptions_are_type_hints() {
        assert_eq!(
            describe(&path("/h/d", "d", "d", true)).as_deref(),
            Some("Folder")
        );
        assert_eq!(
            describe(&path("/h/a.md", "a.md", "a.md", false)).as_deref(),
            Some("MD")
        );
        // A file with no dot at all has nothing to report.
        assert_eq!(
            describe(&path("/h/Makefile", "Makefile", "Makefile", false)),
            None
        );
        // Dotfiles read their suffix as the type — carried over from the
        // original behaviour deliberately, since `.env` showing "ENV" is
        // reasonable and changing it here would be an unrelated UI change.
        assert_eq!(
            describe(&path("/h/.env", ".env", ".env", false)).as_deref(),
            Some("ENV")
        );
    }

    /// Groups are ranked independently so one can't starve the other.
    #[test]
    fn split_caps_each_group_separately() {
        let mut items = vec![];
        for i in 0..30 {
            items.push(path(
                &format!("/h/readme{i}.md"),
                &format!("readme{i}.md"),
                &format!("readme{i}.md"),
                false,
            ));
        }
        items.push(path("/h/readme", "readme", "readme", true));
        let (folders, files) = split_groups(rank("readme", items, |_| 0), 25);
        assert_eq!(folders.len(), 1, "the folder must survive 30 files");
        assert_eq!(files.len(), 25);
    }
}
