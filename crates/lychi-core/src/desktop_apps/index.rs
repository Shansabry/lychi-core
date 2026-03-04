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
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
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
        let nucleo_ids = if candidate_ids.len() < 5 {
            self.nucleo_candidates(norm, 20)
        } else {
            Vec::new()
        };
        for id in &nucleo_ids {
            candidate_ids.insert(*id);
        }

        // --- Score all candidates ---
        candidate_ids
            .into_iter()
            .filter_map(|id| {
                let entry = &self.entries[id];
                let score = self.score(norm, &query_tokens, entry, &nucleo_ids);
                if score >= CANDIDATE_THRESHOLD {
                    Some((id, score))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Deterministic scoring function.
    ///
    /// Deterministic signals always dominate nucleo.
    /// Nucleo is only used as a tie-breaker when no deterministic signal fires.
    fn score(
        &self,
        norm: &str,
        query_tokens: &[String],
        entry: &DesktopEntry,
        nucleo_ids: &[usize],
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

        // If a deterministic signal fired, return it
        if det_score > 0.0 {
            return det_score;
        }

        // Nucleo fallback — only when no deterministic signal
        // Find this entry's position in nucleo results to get a normalized score
        let id = self.by_desktop_path.get(&entry.desktop_path).copied();
        if let Some(id) = id
            && let Some(pos) = nucleo_ids.iter().position(|&nid| nid == id)
        {
            // Rank-based normalization: top result ≈ 0.80, drops off
            let rank_score = 0.80 * (1.0 - pos as f32 / nucleo_ids.len() as f32);
            return rank_score.clamp(0.0, 0.80);
        }

        0.0
    }

    /// Run nucleo fuzzy match over all entry names. Returns IDs of top matches.
    fn nucleo_candidates(&self, query: &str, limit: usize) -> Vec<usize> {
        if query.is_empty() || self.entries.is_empty() {
            return Vec::new();
        }

        let mut matcher = Matcher::new(Config::DEFAULT);
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

        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored.truncate(limit);
        scored.into_iter().map(|(id, _)| id).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop_apps::entry::{DesktopEntry, exec_basename, make_acronym};

    fn make_entry(
        name: &str,
        exec: &str,
        keywords: &[&str],
        generic_name: Option<&str>,
        wm_class: Option<&str>,
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
