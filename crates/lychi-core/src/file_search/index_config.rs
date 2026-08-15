//! The one policy source for what the file-search walk indexes.
//!
//! A launcher must find any file the user can see — including config under a
//! dot-directory (`~/.config`, `~/.ssh`) — WITHOUT descending into the giant
//! machine-generated dot-trees (`~/.cache`, `~/.local/share/Steam`, browser
//! caches) that would make the walk take minutes and the path arena eat
//! gigabytes. So the walk keeps hidden-file filtering ON and RE-INCLUDES a small,
//! bounded ALLOWLIST of useful dot-dirs, rather than turning hidden off and
//! chasing an unbounded denylist of junk (which never stays complete).
//!
//! This module is the single home for that policy so the two consumers can't
//! drift: the walk's `filter_entry` (what to descend into) and the watcher's
//! `is_indexable` (whether an event should trigger a rebuild) both read it. It is
//! populated once at startup from `config.toml`'s `[file_search]` section via
//! [`init_config`]; before that (and in tests) [`current`] returns the built-in
//! [`IndexConfig::default`], so a corpus built without app startup still works.

use std::path::Path;
use std::sync::OnceLock;

/// Useful dot-directories re-included into the index even though hidden-filtering
/// is on. Small and stable by construction — the launcher indexes what a user
/// actually searches for, never `.cache`/`.local/share`. Matched as a leading
/// path component (so `~/.config/...` and `~/code/.config` both match `.config`).
const BUILTIN_DOT_DIRS: &[&str] = &[".config", ".ssh", ".gnupg", ".local"];

/// Under `.local`, ONLY these subdirs are useful; the rest (`share/Steam`,
/// `share/Trash`, `state`, …) is machine bulk. Checked as `.local/<name>`.
const BUILTIN_LOCAL_SUBDIRS: &[&str] = &["bin", "share/applications"];

/// Machine-generated directories pruned WHOLE-SUBTREE wherever they appear.
/// Seeded from KDE Baloo's default exclude filters + the XDG cache/data trees +
/// common dev caches. The launcher never indexes their contents.
const BUILTIN_EXCLUDES: &[&str] = &[
    // Caches / generated state
    ".cache",
    ".thumbnails",
    "Cache",
    "CacheStorage",
    "Code Cache",
    "GPUCache",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    // Package / build caches
    "node_modules",
    "target",
    ".gradle",
    ".m2",
    ".npm",
    ".yarn",
    ".cargo",
    ".rustup",
    ".nvm",
    ".pyenv",
    ".conda",
    "dist",
    "build",
    ".next",
    ".terraform",
    ".tox",
    // Big app data trees
    ".mozilla",
    ".steam",
    ".wine",
    ".docker",
    ".var", // Flatpak per-app data (~/.var/app/*)
    // VCS internals
    ".git",
    ".svn",
    ".hg",
    // venvs
    ".venv",
    "venv",
];

/// Default hard ceiling on indexed paths — the backstop that keeps a pathological
/// home dir from exhausting memory. A real home indexes ~160k; 400k is generous
/// headroom, and the walk stops (not truncates a growing arena) at this bound.
const DEFAULT_MAX_INDEXED_PATHS: usize = 400_000;

/// A directory with more immediate children than this is machine-generated bulk
/// and pruned whole-subtree — a NAME-AGNOSTIC net for junk trees the denylist
/// doesn't know about. Matches the existing watcher's `MAX_WATCHED_SUBTREE`.
const DEFAULT_MAX_DIR_CHILDREN: usize = 1000;

/// Resolved index policy: the built-in lists merged with the user's `config.toml`
/// additions, plus the caps. Cheap to clone (small `Vec<String>`s), read on the
/// walk's hot path.
#[derive(Debug, Clone)]
pub struct IndexConfig {
    dot_dirs: Vec<String>,
    excludes: Vec<String>,
    pub max_indexed_paths: usize,
    pub max_dir_children: usize,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            dot_dirs: BUILTIN_DOT_DIRS.iter().map(|s| s.to_string()).collect(),
            excludes: BUILTIN_EXCLUDES.iter().map(|s| s.to_string()).collect(),
            max_indexed_paths: DEFAULT_MAX_INDEXED_PATHS,
            max_dir_children: DEFAULT_MAX_DIR_CHILDREN,
        }
    }
}

impl IndexConfig {
    /// Build the resolved policy: built-ins + the user's additive `config.toml`
    /// entries. `0` caps fall back to the built-in defaults.
    pub fn from_parts(
        extra_dot_dirs: &[String],
        extra_excludes: &[String],
        max_indexed_paths: usize,
        max_dir_children: usize,
    ) -> Self {
        let mut base = Self::default();
        base.dot_dirs.extend(extra_dot_dirs.iter().cloned());
        base.excludes.extend(extra_excludes.iter().cloned());
        if max_indexed_paths > 0 {
            base.max_indexed_paths = max_indexed_paths;
        }
        if max_dir_children > 0 {
            base.max_dir_children = max_dir_children;
        }
        base
    }

    /// Whether a directory name is a pruned junk tree (checked per component, so
    /// the whole subtree is skipped at the walk's `filter_entry`).
    pub fn is_excluded_dir(&self, name: &str) -> bool {
        self.excludes.iter().any(|e| e == name)
    }

    /// Whether `path` (relative to the scope root) is under an allowlisted useful
    /// dot-dir — so a re-included dotfile is still watched/indexed. Non-hidden
    /// paths are handled by the caller; this only answers the hidden case.
    ///
    /// A path is allowed when EITHER:
    /// - it's a home-root dotFILE (a leading `.name` with no further dir), or
    /// - its first component is an allowlisted dot-dir (`.config`, `.ssh`, …),
    ///   with `.local` narrowed to its useful subdirs only.
    pub fn dotpath_allowed(&self, rel: &Path) -> bool {
        use std::path::Component;
        let comps: Vec<&str> = rel
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect();
        let Some(&first) = comps.first() else {
            return false;
        };

        if !self.dot_dirs.iter().any(|d| d == first) {
            return false; // first component isn't an allowlisted dot-dir
        }

        // `.local` is huge; only its useful subtrees are indexed. `.local` on its
        // own (the bare dir) is fine to surface; a deeper path must be under one
        // of the useful subdirs (`bin`, `share/applications`).
        if first == ".local" && comps.len() > 1 {
            let rest = comps[1..].join("/");
            return BUILTIN_LOCAL_SUBDIRS
                .iter()
                .any(|sub| rest == *sub || rest.starts_with(&format!("{sub}/")));
        }
        true
    }
}

// ── The process-global, set once at startup (mirrors db::frecency's store) ──

static CONFIG: OnceLock<IndexConfig> = OnceLock::new();

/// Register the resolved index policy. Called once at app startup from the loaded
/// `config.toml`. Idempotent-ish: a second call is ignored (set-once).
pub fn init_config(cfg: IndexConfig) {
    let _ = CONFIG.set(cfg);
}

/// The current index policy. Returns the built-in [`IndexConfig::default`] before
/// startup registration and in tests — so a corpus built without app startup
/// (many tests, the CLI probe) uses the sensible built-ins rather than nothing.
#[cfg(not(test))]
pub fn current() -> IndexConfig {
    CONFIG.get().cloned().unwrap_or_default()
}

#[cfg(test)]
thread_local! {
    static TEST_CONFIG: std::cell::RefCell<Option<IndexConfig>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn current() -> IndexConfig {
    TEST_CONFIG
        .with(|c| c.borrow().clone())
        .or_else(|| CONFIG.get().cloned())
        .unwrap_or_default()
}

/// Override this thread's index policy (test-only), for testing prune/allowlist
/// behaviour without app startup.
#[cfg(test)]
pub fn set_config_for_test(cfg: IndexConfig) {
    TEST_CONFIG.with(|c| *c.borrow_mut() = Some(cfg));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn useful_dotdirs_allowed_junk_excluded() {
        let c = IndexConfig::default();
        assert!(c.dotpath_allowed(Path::new(".config/nvim/init.lua")));
        assert!(c.dotpath_allowed(Path::new(".ssh/config")));
        assert!(c.dotpath_allowed(Path::new(".local/bin/my-script")));
        assert!(c.dotpath_allowed(Path::new(".local/share/applications/foo.desktop")));
        // .local junk is NOT allowed even though .local is on the list.
        assert!(!c.dotpath_allowed(Path::new(".local/share/Steam/game")));
        assert!(!c.dotpath_allowed(Path::new(".local/state/whatever")));
        // Non-allowlisted dot-dirs are excluded.
        assert!(!c.dotpath_allowed(Path::new(".mozilla/firefox/prefs.js")));
        assert!(!c.dotpath_allowed(Path::new(".cache/thumbnails/x.png")));
    }

    #[test]
    fn excluded_dirs_are_pruned() {
        let c = IndexConfig::default();
        for name in [
            ".cache",
            "node_modules",
            "target",
            ".git",
            ".venv",
            ".mozilla",
        ] {
            assert!(c.is_excluded_dir(name), "{name} must be excluded");
        }
        assert!(!c.is_excluded_dir("Documents"));
        assert!(!c.is_excluded_dir("src"));
    }

    #[test]
    fn config_extras_are_additive() {
        let c = IndexConfig::from_parts(
            &[".dotfiles".to_string()],
            &["Backups".to_string()],
            50_000,
            2000,
        );
        assert!(c.dotpath_allowed(Path::new(".dotfiles/vimrc")));
        assert!(c.dotpath_allowed(Path::new(".config/x"))); // built-in still there
        assert!(c.is_excluded_dir("Backups"));
        assert!(c.is_excluded_dir(".cache")); // built-in still there
        assert_eq!(c.max_indexed_paths, 50_000);
        assert_eq!(c.max_dir_children, 2000);
    }

    #[test]
    fn zero_caps_fall_back_to_builtin() {
        let c = IndexConfig::from_parts(&[], &[], 0, 0);
        assert_eq!(c.max_indexed_paths, DEFAULT_MAX_INDEXED_PATHS);
        assert_eq!(c.max_dir_children, DEFAULT_MAX_DIR_CHILDREN);
    }
}
