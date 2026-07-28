//! Reading the old `search_engines` table as quicklinks.
//!
//! Quicklinks supersede the `[commands.search_engines]` map, which was
//! `keyword = "url-template"` and nothing else. That map is still what sits in
//! every existing user's `config.toml`, so it has to keep working — silently
//! dropping someone's shortcuts on upgrade is not an acceptable migration.
//!
//! The old form carries strictly less information than the new one, and the
//! missing fields have exactly one correct answer each:
//!
//! | Field  | Value for a migrated entry | Why                                    |
//! |--------|----------------------------|----------------------------------------|
//! | `kind` | [`QuicklinkKind::Url`]     | The old map could only produce URLs.   |
//! | `name` | empty                      | It had no name; the keyword is shown.  |
//!
//! So the conversion is total and unambiguous — there is no guessing here, only
//! filling in fields whose old value was fixed by construction.
//!
//! ## Why config is read, not rewritten
//!
//! Migration happens on **load**, in memory. The user's `config.toml` is left
//! alone until they next save from Settings. Rewriting a hand-maintained config
//! file behind someone's back — reordering it, dropping their comments — is a
//! worse failure than carrying a legacy shape for a while.

use std::collections::HashMap;

use super::{Quicklink, QuicklinkKind};

/// Convert a legacy `search_engines` map into quicklinks.
///
/// Order is stabilised by keyword so a `HashMap`'s arbitrary iteration order
/// doesn't make the Settings list jump around between launches.
pub fn from_search_engines(engines: &HashMap<String, String>) -> Vec<Quicklink> {
    let mut out: Vec<Quicklink> = engines
        .iter()
        .map(|(keyword, template)| Quicklink {
            keyword: Quicklink::normalize_keyword(keyword),
            name: String::new(),
            // The old map could only ever produce a URL — this is the old
            // behaviour restated, not an inference about the template's text.
            kind: QuicklinkKind::Url,
            template: template.clone(),
        })
        .collect();
    out.sort_by(|a, b| a.keyword.cmp(&b.keyword));
    out
}

/// Merge legacy entries into an explicit quicklink list.
///
/// An explicit `[[commands.quicklinks]]` entry wins over a legacy
/// `search_engines` entry with the same keyword: the user wrote the new form
/// deliberately, and it can express things the old one cannot. Legacy entries
/// with no new-form counterpart are kept, so a partially-migrated config loses
/// nothing.
pub fn merge(explicit: Vec<Quicklink>, legacy: &HashMap<String, String>) -> Vec<Quicklink> {
    let mut out = explicit;
    let taken: std::collections::HashSet<String> =
        out.iter().map(|q| q.keyword.to_lowercase()).collect();

    let mut carried: Vec<Quicklink> = from_search_engines(legacy)
        .into_iter()
        .filter(|q| !taken.contains(&q.keyword.to_lowercase()))
        .collect();

    out.append(&mut carried);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy() -> HashMap<String, String> {
        [
            ("gh", "https://github.com/search?q="),
            ("npm", "https://www.npmjs.com/search?q="),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn legacy_entries_become_url_quicklinks() {
        let out = from_search_engines(&legacy());
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|q| q.kind == QuicklinkKind::Url));
    }

    #[test]
    fn legacy_templates_are_preserved_byte_for_byte() {
        // The template is the user's data. Migration must not "tidy" it — a
        // rewritten template could change what the shortcut resolves to.
        let out = from_search_engines(&legacy());
        let gh = out.iter().find(|q| q.keyword == "gh").unwrap();
        assert_eq!(gh.template, "https://github.com/search?q=");
    }

    #[test]
    fn migrated_order_is_stable_across_runs() {
        // HashMap iteration order is arbitrary; without sorting, the Settings
        // list would reshuffle on every launch.
        let a = from_search_engines(&legacy());
        let b = from_search_engines(&legacy());
        assert_eq!(a, b);
        assert_eq!(a[0].keyword, "gh");
    }

    #[test]
    fn keywords_are_normalized_on_the_way_in() {
        let mut m = HashMap::new();
        m.insert("  GH  ".to_string(), "https://x.com/?q=".to_string());
        assert_eq!(from_search_engines(&m)[0].keyword, "gh");
    }

    #[test]
    fn an_explicit_entry_wins_over_its_legacy_twin() {
        let explicit = vec![Quicklink {
            keyword: "gh".to_string(),
            name: "GitHub".to_string(),
            kind: QuicklinkKind::Shell,
            template: "gh repo view {repo}".to_string(),
        }];
        let merged = merge(explicit, &legacy());
        let gh: Vec<_> = merged.iter().filter(|q| q.keyword == "gh").collect();
        assert_eq!(gh.len(), 1, "keyword must not appear twice");
        assert_eq!(gh[0].kind, QuicklinkKind::Shell);
        assert_eq!(gh[0].name, "GitHub");
    }

    #[test]
    fn legacy_entries_without_a_new_counterpart_survive() {
        // The partial-migration case: someone converted one shortcut by hand and
        // left the rest. Nothing may be dropped.
        let explicit = vec![Quicklink {
            keyword: "gh".to_string(),
            name: String::new(),
            kind: QuicklinkKind::Url,
            template: "https://github.com/search?q={query}".to_string(),
        }];
        let merged = merge(explicit, &legacy());
        assert!(
            merged.iter().any(|q| q.keyword == "npm"),
            "legacy-only entry was dropped: {merged:?}"
        );
    }

    #[test]
    fn an_empty_legacy_map_adds_nothing() {
        let explicit = vec![Quicklink {
            keyword: "x".to_string(),
            name: String::new(),
            kind: QuicklinkKind::Url,
            template: "https://x.com/{q}".to_string(),
        }];
        assert_eq!(merge(explicit.clone(), &HashMap::new()), explicit);
    }
}
