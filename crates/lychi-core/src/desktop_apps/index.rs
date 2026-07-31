use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwap;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use super::entry::{DesktopEntry, query_norm, tokenize};
use super::parse::discover_entries;

/// Auto-launch confidence threshold.
/// Scores ≥ this mean "we're certain enough to launch without asking".
pub const AUTO_LAUNCH_THRESHOLD: f32 = 0.90;

/// Magnitude of the objective app-nature ranking nudge. Deliberately tiny:
/// enough to break an otherwise-exact cold-start tie toward the real GUI app,
/// small enough that a single frecency launch (worth up to ~0.30 in the
/// downstream blend) overrides it — so a firewall admin who launches Firewall
/// once gets it pinned. NEVER references an app name.
const QUALITY_NUDGE: f32 = 0.02;

/// A name-agnostic app-nature signal, in `[-QUALITY_NUDGE, +QUALITY_NUDGE]`.
///
/// A launchable GUI application (e.g. a browser) nudges up; a Settings/System
/// config panel or a Terminal=true CLI tool nudges down. This objectively
/// separates "Firefox" (Network/WebBrowser) from "Firewall" (Settings/System)
/// at cold start, and generalizes to any app pair without hardcoding names.
fn quality_nudge(entry: &DesktopEntry) -> f32 {
    // Config-tool / CLI markers: lower affinity as a launch target.
    let is_config_tool = entry.is_terminal_app
        || entry
            .categories
            .iter()
            .any(|c| c == "settings" || c == "system" || c == "console-only");
    if is_config_tool {
        return -QUALITY_NUDGE;
    }
    // GUI application markers: higher affinity.
    let is_gui_app = entry.categories.iter().any(|c| {
        matches!(
            c.as_str(),
            "network"
                | "webbrowser"
                | "audiovideo"
                | "graphics"
                | "office"
                | "game"
                | "development"
        )
    });
    if is_gui_app { QUALITY_NUDGE } else { 0.0 }
}

/// Minimum score to include in completion candidates.
pub const CANDIDATE_THRESHOLD: f32 = 0.30;

/// Global AppIndex — ArcSwap allows lock-free reads and atomic hot-swap on rebuild.
static APP_INDEX: OnceLock<ArcSwap<AppIndex>> = OnceLock::new();

fn global_store() -> &'static ArcSwap<AppIndex> {
    APP_INDEX.get_or_init(|| ArcSwap::from_pointee(AppIndex::build(discover_entries())))
}

/// Get a snapshot of the current AppIndex. The returned Guard derefs to AppIndex.
pub fn app_index() -> arc_swap::Guard<Arc<AppIndex>> {
    global_store().load()
}

/// Rebuild the AppIndex from disk and atomically swap it in.
/// Called by the watcher thread after a debounced filesystem change.
pub fn rebuild_app_index() {
    let new_index = Arc::new(AppIndex::build(discover_entries()));
    global_store().store(new_index);
}

pub struct AppIndex {
    /// All entries — indexed by usize ID.
    pub entries: Vec<DesktopEntry>,
    /// Stable canonical ID lookup.
    by_desktop_path: HashMap<String, usize>,
    /// Exact name lookup (lowercased).
    by_name: HashMap<String, usize>,
    /// Name tokens → entry IDs.
    by_token: HashMap<String, Vec<usize>>,
    /// Keywords → entry IDs.
    by_keyword: HashMap<String, Vec<usize>>,
    /// Acronyms → entry IDs (e.g. "vsc" → VS Code).
    by_acronym: HashMap<String, Vec<usize>>,
    /// Exec basenames → entry IDs (e.g. "code" → VS Code).
    by_exec: HashMap<String, Vec<usize>>,
    /// WMClass → entry IDs.
    by_wmclass: HashMap<String, Vec<usize>>,
}

impl AppIndex {
    pub fn build(entries: Vec<DesktopEntry>) -> Self {
        let mut by_desktop_path = HashMap::new();
        let mut by_name = HashMap::new();
        let mut by_token: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_keyword: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_acronym: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_exec: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_wmclass: HashMap<String, Vec<usize>> = HashMap::new();

        for (id, entry) in entries.iter().enumerate() {
            by_desktop_path.insert(entry.desktop_path.clone(), id);
            by_name.insert(entry.name.to_lowercase(), id);

            for token in &entry.name_tokens {
                by_token.entry(token.clone()).or_default().push(id);
            }
            for kw in &entry.keywords {
                by_keyword.entry(kw.clone()).or_default().push(id);
            }
            if !entry.acronym.is_empty() {
                by_acronym
                    .entry(entry.acronym.clone())
                    .or_default()
                    .push(id);
            }
            if !entry.exec_basename.is_empty() {
                by_exec
                    .entry(entry.exec_basename.clone())
                    .or_default()
                    .push(id);
            }
            if let Some(ref wmc) = entry.wm_class {
                by_wmclass.entry(wmc.to_lowercase()).or_default().push(id);
            }
            // Also index generic_name tokens into by_token
            if let Some(ref gn) = entry.generic_name {
                for token in tokenize(gn) {
                    by_token.entry(token).or_default().push(id);
                }
            }
        }

        // Dedup all inverted map vecs (prevents inflated candidate sets)
        for v in by_token.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        for v in by_keyword.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        for v in by_acronym.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        for v in by_exec.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        for v in by_wmclass.values_mut() {
            v.sort_unstable();
            v.dedup();
        }

        Self {
            entries,
            by_desktop_path,
            by_name,
            by_token,
            by_keyword,
            by_acronym,
            by_exec,
            by_wmclass,
        }
    }

    /// Look up an entry by its desktop_path (stable canonical ID).
    pub fn by_path(&self, path: &str) -> Option<&DesktopEntry> {
        self.by_desktop_path.get(path).map(|&id| &self.entries[id])
    }

    /// Whether `args` is a known .desktop path that exists in this index.
    /// Used by the open handler fast-path to distinguish "concrete target" from "human query".
    pub fn is_desktop_path(&self, args: &str) -> bool {
        let path = std::path::Path::new(args);
        path.is_absolute() && args.ends_with(".desktop") && self.by_desktop_path.contains_key(args)
    }

    /// Get entry by usize ID.
    pub fn entry(&self, id: usize) -> &DesktopEntry {
        &self.entries[id]
    }

    /// Lowercased `Categories=` of the .desktop entry whose `StartupWMClass`
    /// matches `wm_class` (case-insensitive). Empty if no entry is indexed for
    /// that class. Used as a standards-based fallback for classifying a focused
    /// window (e.g. `TerminalEmulator` / `IDE`) when curated lists miss it.
    pub fn categories_for_wmclass(&self, wm_class: &str) -> Vec<String> {
        let key = wm_class.to_lowercase();
        self.by_wmclass
            .get(&key)
            .and_then(|ids| ids.first())
            .map(|&id| self.entries[id].categories.clone())
            .unwrap_or_default()
    }

    /// Best single match for a query. Returns `(id, score)`.
    /// Used by Phase 3: score ≥ AUTO_LAUNCH_THRESHOLD → route to "open".
    pub fn best_match(&self, query: &str) -> Option<(usize, f32)> {
        let norm = query_norm(query);
        if norm.is_empty() {
            return None;
        }

        let candidates = self.gather_candidates(&norm);
        if candidates.is_empty() {
            return None;
        }

        let (id, score) = candidates
            .into_iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())?;
        if score >= CANDIDATE_THRESHOLD {
            Some((id, score))
        } else {
            None
        }
    }

    /// Return up to `limit` scored candidates for completions. Returns `(id, score)` pairs.
    /// Sorted descending by score.
    pub fn candidates(&self, query: &str, limit: usize) -> Vec<(usize, f32)> {
        let norm = query_norm(query);
        if norm.is_empty() {
            return Vec::new();
        }

        let mut scored = self.gather_candidates(&norm);
        // Primary sort by score; deterministic last-resort tiebreak (fzf-style:
        // shorter name, then stable id) only when scores are effectively equal.
        // This is a determinism guarantee, NOT a ranking opinion — real ties
        // are meant to be resolved by frecency downstream and by the objective
        // app-nature nudge in score().
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| self.entry(a.0).name.len().cmp(&self.entry(b.0).name.len()))
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(limit);
        scored
    }

    /// Gather candidate (id, score) pairs from all indices + nucleo fallback.
    fn gather_candidates(&self, norm: &str) -> Vec<(usize, f32)> {
        use std::collections::HashSet;

        let mut candidate_ids: HashSet<usize> = HashSet::new();

        // --- High-precision indexed recall (always run) ---
        // Exact name
        if let Some(&id) = self.by_name.get(norm) {
            candidate_ids.insert(id);
        }
        // Acronym
        if let Some(ids) = self.by_acronym.get(norm) {
            candidate_ids.extend(ids);
        }
        // Exec basename
        if let Some(ids) = self.by_exec.get(norm) {
            candidate_ids.extend(ids);
        }
        // WMClass
        if let Some(ids) = self.by_wmclass.get(norm) {
            candidate_ids.extend(ids);
        }
        // Keyword exact (whole query as a keyword)
        if let Some(ids) = self.by_keyword.get(norm) {
            candidate_ids.extend(ids);
        }

        // Token lookup — can produce large sets for common words; cap total at 200
        let query_tokens: Vec<String> = tokenize(norm);
        'token_loop: for token in &query_tokens {
            for map in [&self.by_token, &self.by_keyword] {
                if let Some(ids) = map.get(token) {
                    for &id in ids {
                        candidate_ids.insert(id);
                        if candidate_ids.len() >= 200 {
                            break 'token_loop;
                        }
                    }
                }
            }
        }

        // --- Nucleo fuzzy fallback ---
        // Only when indices didn't produce enough candidates.
        let nucleo_scored = if candidate_ids.len() < 5 {
            self.nucleo_candidates(norm, 20)
        } else {
            Vec::new()
        };
        for (id, _) in &nucleo_scored {
            candidate_ids.insert(*id);
        }

        // --- Score all candidates ---
        candidate_ids
            .into_iter()
            .filter_map(|id| {
                let entry = &self.entries[id];
                let score = self.score(norm, &query_tokens, entry, &nucleo_scored);
                if score >= CANDIDATE_THRESHOLD {
                    Some((id, score))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Deterministic scoring function. Deterministic signals always dominate
    /// nucleo; nucleo is only a tie-breaker when no deterministic signal fires.
    /// The objective app-nature nudge (see `quality_nudge`) is applied last.
    fn score(
        &self,
        norm: &str,
        query_tokens: &[String],
        entry: &DesktopEntry,
        nucleo_scored: &[(usize, u16)],
    ) -> f32 {
        let name_lower = entry.name.to_lowercase();

        // Exact name match
        if norm == name_lower {
            return 1.0;
        }

        let mut det_score: f32 = 0.0;

        // Acronym match
        if norm == entry.acronym {
            det_score = det_score.max(0.85 + 0.08);
        }

        // Exec basename match
        if norm == entry.exec_basename {
            det_score = det_score.max(0.85 + 0.03);
        }

        // WMClass match
        if entry
            .wm_class
            .as_deref()
            .map(|s| s.to_lowercase())
            .as_deref()
            == Some(norm)
        {
            det_score = det_score.max(0.85 + 0.03);
        }

        // All query tokens found in name tokens
        if !query_tokens.is_empty() && query_tokens.iter().all(|t| entry.name_tokens.contains(t)) {
            let mut s = 0.85_f32;
            // Prefix bonus: any query token is a prefix of a name token
            if query_tokens.iter().any(|t| {
                entry
                    .name_tokens
                    .iter()
                    .any(|nt| nt.starts_with(t.as_str()))
            }) {
                s += 0.05;
            }
            // Keyword bonus
            if query_tokens.iter().any(|t| entry.keywords.contains(t)) {
                s += 0.05;
            }
            det_score = det_score.max(s);
        }

        // Token-set (subset) match — the `token_set_ratio` technique. The
        // COMPLETE app name appears among the query tokens, plus extra words:
        // "can you open spotify" → name `[spotify]` ⊆ query `[can,you,open,spotify]`.
        // This makes intent adaptive to phrasing/grammar WITHOUT a verb or
        // stop-word blocklist — any framing that contains the whole app name
        // resolves to it. Guards against a generic word hijacking a phrase:
        //   - ALL name tokens must be present (not a fragment), and
        //   - the name must be DISTINCTIVE — multi-token (e.g. "visual studio
        //     code"), or a single token ≥ 5 chars — so a bare generic app like
        //     "Code"/"Docs"/"Files" doesn't fire on a sentence that mentions it.
        // Distinctiveness is a length property, not a hardcoded word list.
        let name_is_distinctive =
            entry.name_tokens.len() > 1 || entry.name_tokens.iter().any(|nt| nt.len() >= 5);
        if name_is_distinctive
            && query_tokens.len() > entry.name_tokens.len()
            && entry.name_tokens.iter().all(|nt| query_tokens.contains(nt))
        {
            det_score = det_score.max(0.90);
        }

        // Keyword-only match (query token in keywords or generic_name tokens)
        if det_score == 0.0 {
            let in_keywords = query_tokens.iter().any(|t| entry.keywords.contains(t));
            let in_generic = entry
                .generic_name
                .as_deref()
                .map(|gn| {
                    let gn_tokens = tokenize(gn);
                    query_tokens.iter().any(|t| gn_tokens.contains(t))
                })
                .unwrap_or(false);
            if in_keywords || in_generic {
                det_score = 0.75;
            }
        }

        // If a deterministic signal fired, return it (with the app-nature nudge)
        if det_score > 0.0 {
            return (det_score + quality_nudge(entry)).clamp(0.0, 0.99);
        }

        // Nucleo fallback — only when no deterministic signal.
        // Normalize by the BEST raw score in this result set (not by rank
        // position). Equally-good matches → near-equal scores, so the tiny
        // objective nudge and downstream frecency decide the order — instead
        // of an artificial rank gap picking an arbitrary winner.
        let id = self.by_desktop_path.get(&entry.desktop_path).copied();
        if let Some(id) = id
            && let Some(&(_, raw)) = nucleo_scored.iter().find(|(nid, _)| *nid == id)
        {
            let best = nucleo_scored.first().map(|(_, s)| *s).unwrap_or(raw).max(1);
            // Top of the band ≈ 0.80; proportional to raw match quality.
            let base = 0.80 * (raw as f32 / best as f32);
            return (base + quality_nudge(entry)).clamp(0.0, 0.82);
        }

        0.0
    }

    /// Run nucleo fuzzy match over all entry names. Returns `(id, raw_score)`
    /// pairs of the top matches, descending. Raw nucleo scores are kept (not
    /// discarded for rank position) so `score()` can normalize them RELATIVELY
    /// — two equally-good matches (e.g. "fir" → Firefox/Firewall, both clean
    /// prefixes) land near-equal, letting the objective nudge and frecency
    /// decide rather than an artificial rank gap.
    fn nucleo_candidates(&self, query: &str, limit: usize) -> Vec<(usize, u16)> {
        if query.is_empty() || self.entries.is_empty() {
            return Vec::new();
        }

        // prefer_prefix: nucleo's own recommendation for autocompletion —
        // a match anchored at the start scores higher than a mid-string match,
        // so "fir" ranks "Firefox"/"Firewall" above "Thunar File Manager".
        let mut config = Config::DEFAULT;
        config.prefer_prefix = true;
        let mut matcher = Matcher::new(config);
        let pattern = Atom::new(
            query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
            false,
        );

        let mut scored: Vec<(usize, u16)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(id, entry)| {
                let mut buf = Vec::new();
                let haystack = Utf32Str::new(&entry.name, &mut buf);
                let score = pattern.score(haystack, &mut matcher)?;
                Some((id, score))
            })
            .collect();

        scored.sort_by_key(|b| std::cmp::Reverse(b.1));
        scored.truncate(limit);
        scored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop_apps::entry::{DesktopEntry, exec_basename, make_acronym};

    pub(crate) fn make_entry(
        name: &str,
        exec: &str,
        keywords: &[&str],
        generic_name: Option<&str>,
        wm_class: Option<&str>,
    ) -> DesktopEntry {
        make_entry_full(name, exec, keywords, generic_name, wm_class, &[], false)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn make_entry_full(
        name: &str,
        exec: &str,
        keywords: &[&str],
        generic_name: Option<&str>,
        wm_class: Option<&str>,
        categories: &[&str],
        is_terminal_app: bool,
    ) -> DesktopEntry {
        let exec_base = exec_basename(exec);
        DesktopEntry {
            name: name.to_string(),
            exec: exec.to_string(),
            exec_basename: exec_base,
            wm_class: wm_class.map(|s| s.to_string()),
            generic_name: generic_name.map(|s| s.to_string()),
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
            name_tokens: tokenize(name),
            acronym: make_acronym(name),
            icon: None,
            categories: categories.iter().map(|s| s.to_lowercase()).collect(),
            is_terminal_app,
            desktop_path: format!(
                "/usr/share/applications/{}.desktop",
                name.to_lowercase().replace(' ', "-")
            ),
            icon_path: std::sync::OnceLock::new(),
        }
    }

    fn test_index() -> AppIndex {
        AppIndex::build(vec![
            make_entry(
                "Visual Studio Code",
                "/usr/bin/code",
                &["editor", "ide", "vscode"],
                Some("Text Editor"),
                Some("Code"),
            ),
            make_entry(
                "Firefox",
                "/usr/bin/firefox",
                &["internet", "web", "browser"],
                Some("Web Browser"),
                Some("firefox"),
            ),
            make_entry(
                "Firewall",
                "/usr/bin/firewall-config",
                &["security", "network"],
                Some("Firewall Configuration"),
                None,
            ),
            make_entry(
                "Application Finder",
                "/usr/bin/xfce4-appfinder",
                &["launcher", "run"],
                None,
                None,
            ),
            make_entry(
                "GIMP",
                "/usr/bin/gimp",
                &["image", "photo", "editor", "graphics"],
                Some("Image Editor"),
                None,
            ),
            make_entry(
                "Nautilus",
                "/usr/bin/nautilus",
                &["files", "folder", "manager"],
                Some("File Manager"),
                Some("org.gnome.Nautilus"),
            ),
        ])
    }

    #[test]
    fn exact_name_match() {
        let idx = test_index();
        let (id, score) = idx.best_match("visual studio code").unwrap();
        assert_eq!(idx.entry(id).name, "Visual Studio Code");
        assert_eq!(score, 1.0);
    }

    #[test]
    fn acronym_match() {
        let idx = test_index();
        let (id, score) = idx.best_match("vsc").unwrap();
        assert_eq!(idx.entry(id).name, "Visual Studio Code");
        assert!(score >= AUTO_LAUNCH_THRESHOLD, "score {score} < threshold");
    }

    #[test]
    fn exec_basename_match() {
        let idx = test_index();
        let (id, score) = idx.best_match("code").unwrap();
        assert_eq!(idx.entry(id).name, "Visual Studio Code");
        assert!(score >= AUTO_LAUNCH_THRESHOLD);
    }

    #[test]
    fn token_set_natural_phrasing_matches_app() {
        // The headline fix: a natural-language phrase containing the full app
        // name resolves to that app, above the auto-launch threshold — no verb
        // or stop-word blocklist, adaptive to any framing/grammar.
        let idx = test_index();
        for phrase in [
            "can you open firefox",
            "open the firefox please",
            "firefox launch pls",
            "i want to open visual studio code now",
        ] {
            let (id, score) = idx
                .best_match(phrase)
                .unwrap_or_else(|| panic!("no match for {phrase:?}"));
            let name = &idx.entry(id).name;
            assert!(
                name == "Firefox" || name == "Visual Studio Code",
                "phrase {phrase:?} matched {name:?}"
            );
            assert!(
                score >= AUTO_LAUNCH_THRESHOLD,
                "phrase {phrase:?} scored {score} < auto-launch threshold"
            );
        }
    }

    #[test]
    fn token_set_requires_full_distinctive_name() {
        // A generic single short word inside a sentence must NOT hijack: "GIMP"
        // is distinctive (would match), but a bare mention shouldn't fire on an
        // unrelated phrase that merely shares a common word.
        let idx = test_index();
        // "editor" is only a keyword, not the full name → no auto-launch hijack.
        let m = idx.best_match("what is the best editor for me today");
        if let Some((id, score)) = m {
            // If anything matches it must not be a confident auto-launch off a
            // single generic keyword buried in a sentence.
            assert!(
                score < AUTO_LAUNCH_THRESHOLD,
                "generic keyword in a sentence should not auto-launch (got {} @ {score})",
                idx.entry(id).name
            );
        }
    }

    #[test]
    fn cold_start_gui_app_beats_config_tool() {
        // "fir" prefixes both. With no usage data, the objective app-nature
        // nudge (Firefox=Network GUI vs Firewall=Settings) breaks the tie
        // toward the launchable GUI app — WITHOUT any name hardcoding.
        let idx = AppIndex::build(vec![
            make_entry_full(
                "Firewall",
                "/usr/bin/firewall-config",
                &[],
                None,
                None,
                &["Settings", "System"],
                false,
            ),
            make_entry_full(
                "Firefox",
                "/usr/bin/firefox",
                &["web", "browser"],
                Some("Web Browser"),
                Some("firefox"),
                &["Network", "WebBrowser"],
                false,
            ),
        ]);
        let ranked = idx.candidates("fir", 5);
        assert!(!ranked.is_empty());
        assert_eq!(
            idx.entry(ranked[0].0).name,
            "Firefox",
            "GUI app should win cold-start tie over config tool"
        );
    }

    #[test]
    fn quality_nudge_is_name_agnostic() {
        // The nudge reads categories, never the name — swap the names and the
        // Settings tool still loses. Guards against regressing to name rules.
        let idx = AppIndex::build(vec![
            make_entry_full(
                "Firefox", // named "Firefox" but categorized as a Settings tool
                "/usr/bin/firefox-settings",
                &[],
                None,
                None,
                &["Settings"],
                false,
            ),
            make_entry_full(
                "Firewall", // named "Firewall" but a real Network GUI app
                "/usr/bin/firewall",
                &[],
                None,
                None,
                &["Network"],
                false,
            ),
        ]);
        let ranked = idx.candidates("fir", 5);
        assert_eq!(
            idx.entry(ranked[0].0).name,
            "Firewall",
            "nudge must follow category, not name"
        );
    }

    #[test]
    fn keyword_match_browser() {
        let idx = test_index();
        let (id, score) = idx.best_match("browser").unwrap();
        assert_eq!(idx.entry(id).name, "Firefox");
        assert!(score >= 0.70);
    }

    #[test]
    fn generic_name_match() {
        let idx = test_index();
        let (id, score) = idx.best_match("file manager").unwrap();
        assert_eq!(idx.entry(id).name, "Nautilus");
        assert!(score >= 0.70);
    }

    #[test]
    fn desktop_path_lookup() {
        let idx = test_index();
        let entry = idx
            .by_path("/usr/share/applications/firefox.desktop")
            .unwrap();
        assert_eq!(entry.name, "Firefox");
    }

    #[test]
    fn is_desktop_path_check() {
        let idx = test_index();
        // Known path in index → true
        assert!(idx.is_desktop_path("/usr/share/applications/firefox.desktop"));
        // Absolute .desktop path but NOT in index → false (key correctness check)
        assert!(!idx.is_desktop_path("/usr/share/applications/nonexistent.desktop"));
        // Not absolute
        assert!(!idx.is_desktop_path("firefox"));
        assert!(!idx.is_desktop_path("visual studio code"));
        assert!(!idx.is_desktop_path("relative/path.desktop"));
    }

    #[test]
    fn candidates_sorted_by_score() {
        let idx = test_index();
        let results = idx.candidates("editor", 10);
        // Should return GIMP and VS Code (both have "editor" keyword)
        assert!(!results.is_empty());
        // Scores descending
        for i in 1..results.len() {
            assert!(results[i - 1].1 >= results[i].1);
        }
    }
}
