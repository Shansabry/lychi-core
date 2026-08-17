use async_trait::async_trait;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::collections::HashMap;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, RwLock};
use std::time::Instant;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;

#[derive(Debug)]
struct ProjectEntry {
    name: String,
    path: PathBuf,
}

type ProjectCacheData = (Vec<String>, HashMap<String, ProjectEntry>);

/// Cache of discovered projects. Uses RwLock so it can be invalidated
/// when project directories change in settings.
static PROJECT_CACHE: RwLock<Option<ProjectCacheData>> = RwLock::new(None);

/// Cached nucleo matcher — reused across calls to avoid cold-start cost.
static MATCHER: Mutex<Option<Matcher>> = Mutex::new(None);

pub struct ProjectOpen {
    directories: Vec<String>,
}

impl ProjectOpen {
    pub fn with_directories(directories: Vec<String>) -> Self {
        // Invalidate cache when directories change
        if let Ok(mut guard) = PROJECT_CACHE.write() {
            *guard = None;
        }
        Self { directories }
    }

    /// Pre-scan project directories at startup so first project search is instant.
    pub fn warmup(directories: &[String]) {
        let t0 = Instant::now();
        let entries = Self::discover_projects(directories);
        let count = entries.len();
        if let Ok(mut guard) = PROJECT_CACHE.write() {
            *guard = Some((
                directories.to_vec(),
                entries
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            ProjectEntry {
                                name: v.name.clone(),
                                path: v.path.clone(),
                            },
                        )
                    })
                    .collect(),
            ));
        }
        // Pre-warm the nucleo matcher so first real query is instant
        {
            let mut guard = MATCHER.lock().unwrap();
            guard.get_or_insert_with(|| Matcher::new(Config::DEFAULT));
        }

        tracing::info!(
            "[project_open] warmup done: {:.0}ms ({count} projects)",
            t0.elapsed().as_secs_f64() * 1000.0
        );
    }

    fn expand_path(path: &str) -> PathBuf {
        if let Some(rest) = path.strip_prefix("~/") {
            dirs::home_dir()
                .map(|h| h.join(rest))
                .unwrap_or_else(|| PathBuf::from(path))
        } else {
            PathBuf::from(path)
        }
    }

    fn discover_projects(directories: &[String]) -> HashMap<String, ProjectEntry> {
        let mut entries = HashMap::new();

        for dir_str in directories {
            let dir = Self::expand_path(dir_str);
            Self::scan_dir(&dir, 0, 5, &mut entries);
        }

        entries
    }

    /// Markers that indicate a directory is a real project root.
    const PROJECT_MARKERS: &[&str] = &[
        // VCS
        ".git",
        ".hg",
        ".svn",
        // Rust
        "Cargo.toml",
        // JavaScript / TypeScript / Node / Bun / Deno
        "package.json",
        "bun.lockb",
        "bunfig.toml",
        "deno.json",
        "deno.jsonc",
        // Python
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "Pipfile",
        "requirements.txt",
        // Go
        "go.mod",
        // Java / Kotlin / Scala
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "build.sbt",
        // C / C++
        "CMakeLists.txt",
        "Makefile",
        "meson.build",
        "conanfile.txt",
        "conanfile.py",
        "vcpkg.json",
        // C# / .NET / F#
        "*.sln",
        "*.csproj",
        "*.fsproj",
        // Ruby
        "Gemfile",
        // PHP
        "composer.json",
        // Elixir
        "mix.exs",
        // Dart / Flutter
        "pubspec.yaml",
        // Tauri
        "tauri.conf.json",
        // Electron
        "electron.vite.config.ts",
        "electron-builder.yml",
        "electron-builder.json5",
        // Swift / Xcode
        "Package.swift",
        "*.xcodeproj",
        "*.xcworkspace",
        // Haskell
        "stack.yaml",
        "cabal.project",
        "*.cabal",
        // Zig
        "build.zig",
        // Nim
        "*.nimble",
        // OCaml
        "dune-project",
        // Clojure
        "project.clj",
        "deps.edn",
        // Lua
        "rockspec",
        // R
        "DESCRIPTION",
        // Julia
        "Project.toml",
        // Nix
        "flake.nix",
        // Terraform / Infra
        "main.tf",
        // Docker (standalone projects)
        "docker-compose.yml",
        "docker-compose.yaml",
        // Eclipse
        ".project",
    ];

    /// Directories to never recurse into — build artifacts, dependencies, caches.
    const SKIP_DIRS: &[&str] = &[
        // JS / TS / Bun / Node
        "node_modules",
        ".next",
        ".nuxt",
        // Rust
        "target",
        ".cargo",
        // Python
        ".venv",
        "venv",
        "env",
        "__pycache__",
        ".eggs",
        "*.egg-info",
        ".tox",
        ".mypy_cache",
        ".ruff_cache",
        // Go
        "vendor",
        // Java / Kotlin / Scala
        ".gradle",
        ".mvn",
        ".idea",
        // C# / .NET
        "bin",
        "obj",
        "packages",
        // Elixir
        "deps",
        "_build",
        // General build / output
        "dist",
        "build",
        "out",
        ".cache",
        // Zig
        "zig-cache",
        "zig-out",
        // OCaml
        "_opam",
        // Haskell
        ".stack-work",
        // Flutter / Dart
        ".dart_tool",
        // Terraform
        ".terraform",
    ];

    fn is_project_root(path: &PathBuf) -> bool {
        for marker in Self::PROJECT_MARKERS {
            if let Some(ext) = marker.strip_prefix('*') {
                // Glob pattern like "*.sln" — check if any file matches the extension
                if let Ok(rd) = fs::read_dir(path) {
                    for entry in rd.flatten() {
                        if let Some(name) = entry.file_name().to_str()
                            && name.ends_with(ext)
                        {
                            return true;
                        }
                    }
                }
            } else if path.join(marker).exists() {
                return true;
            }
        }
        false
    }

    fn scan_dir(
        dir: &PathBuf,
        depth: usize,
        max_depth: usize,
        entries: &mut HashMap<String, ProjectEntry>,
    ) {
        if depth >= max_depth {
            return;
        }
        let read_dir = match fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if name.starts_with('.') || Self::SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }

            if Self::is_project_root(&path) {
                // Found a project — index it but don't recurse deeper
                // (nested projects like monorepo packages are their own roots)
                let key = name.to_lowercase();
                entries.entry(key).or_insert_with(|| ProjectEntry {
                    name,
                    path: path.clone(),
                });
            } else {
                // Not a project — keep scanning deeper
                Self::scan_dir(&path, depth + 1, max_depth, entries);
            }
        }
    }

    fn get_projects(&self) -> HashMap<String, ProjectEntry> {
        // Check cache
        if let Ok(guard) = PROJECT_CACHE.read()
            && let Some((cached_dirs, entries)) = guard.as_ref()
            && cached_dirs == &self.directories
        {
            // Clone the entries since we can't return a reference to the guard
            return entries
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        ProjectEntry {
                            name: v.name.clone(),
                            path: v.path.clone(),
                        },
                    )
                })
                .collect();
        }

        // Discover and cache
        let entries = Self::discover_projects(&self.directories);
        if let Ok(mut guard) = PROJECT_CACHE.write() {
            *guard = Some((self.directories.clone(), {
                entries
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            ProjectEntry {
                                name: v.name.clone(),
                                path: v.path.clone(),
                            },
                        )
                    })
                    .collect()
            }));
        }
        entries
    }

    fn fuzzy_match<'a>(
        entries: &'a HashMap<String, ProjectEntry>,
        query: &str,
    ) -> Vec<(&'a ProjectEntry, u16)> {
        if query.is_empty() {
            return Vec::new();
        }

        let mut guard = MATCHER.lock().unwrap();
        let matcher = guard.get_or_insert_with(|| Matcher::new(Config::DEFAULT));
        let pattern = Atom::new(
            query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
            false,
        );

        let mut results: Vec<(&ProjectEntry, u16)> = entries
            .values()
            .filter_map(|entry| {
                let mut buf = Vec::new();
                let haystack = Utf32Str::new(&entry.name, &mut buf);
                let score = pattern.score(haystack, matcher)?;
                Some((entry, score))
            })
            .collect();

        results.sort_by_key(|b| std::cmp::Reverse(b.1));
        results
    }
}

/// Invalidate the project cache so the next lookup re-scans directories.
pub fn invalidate_project_cache() {
    if let Ok(mut guard) = PROJECT_CACHE.write() {
        *guard = None;
    }
}

/// `project`'s argument surface: a single free-form action whose flat form IS
/// the project name. The JSON Schema and the structured→flat adapter both
/// derive from this.
const PROJECT_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Open a code project in the user's code editor. The name is \
               fuzzy-matched against project roots (git repos, Cargo/npm/… \
               projects) discovered under the user's configured project \
               directories, and the best match opens. Use when the user wants \
               to start working on a project they name — not for arbitrary \
               folders (use the file/browse actions for those).",
        mutates: false,
        operands: &[Operand {
            name: "name",
            desc: "The project's folder name (e.g. \"lychi\"), fuzzy-matched \
                   against discovered projects — a partial or approximate name \
                   is fine. The best match opens in the code editor.",
            required: true,
            kind: ArgKind::Text,
            prefix: None,
        }],
    }],
};

/// Normalize the tool's `args` to the flat name string the fuzzy matcher reads.
/// A constrained model sends the structured JSON (`{"name":"lychi"}`); a human
/// or legacy/flat caller sends the name directly and passes through unchanged.
/// Malformed JSON falls back to the raw string; the fuzzy matcher rejects it
/// with the usual "No project matching" error.
fn project_args_to_flat(args: &str) -> String {
    PROJECT_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
}

#[async_trait]
impl ActionHandler for ProjectOpen {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["project"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "project"
    }

    fn description(&self) -> &str {
        "Open a project folder in the code editor"
    }
    fn usage(&self) -> &str {
        "the project name (e.g. 'lychi'). Opens it in the code editor"
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(PROJECT_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Files
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Files
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // A constrained model sends `{"name":..}`; flatten it (a plain-string
        // caller passes through) to the bare name the fuzzy matcher reads.
        let flat = project_args_to_flat(args);
        let query = flat.trim();
        if query.is_empty() {
            return Ok(ActionResult::err("Usage: project <name>".to_string()));
        }

        let start = Instant::now();

        if self.directories.is_empty() {
            return Ok(ActionResult::err("No project directories configured. Add your project folders in Settings → Projects."
                        .to_string(),));
        }

        let entries = self.get_projects();

        let matches = Self::fuzzy_match(&entries, query);
        let (entry, _) = matches.into_iter().next().ok_or_else(|| {
            LychiError::ExecutionFailed(format!(
                "No project matching '{query}' found. Make sure the project's parent directory is added in Settings → Projects."
            ))
        })?;

        let path = &entry.path;

        Command::new("code")
            .arg(path)
            .process_group(0)
            .spawn()
            .map_err(|e| {
                LychiError::ExecutionFailed(format!("Failed to open {} in editor: {e}", entry.name))
            })?;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(ActionResult::ok(
            format!("Opened {} in editor", entry.name),
            OutputType::Status,
        )
        .with_duration(duration_ms))
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let query = partial.trim();
        if query.is_empty() {
            return Vec::new();
        }

        let entries = self.get_projects();
        let matches = Self::fuzzy_match(&entries, query);

        matches
            .into_iter()
            .take(8)
            .map(|(entry, score)| CompletionItem {
                label: entry.name.clone(),
                icon_path: None,
                score,
                description: None,
                reason: None,
                thumb_b64: None,
                run: Some(format!("project {}", entry.name)),
                ..Default::default()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; it flattens to the bare
        // name the fuzzy matcher reads.
        assert_eq!(project_args_to_flat(r#"{"name":"lychi"}"#), "lychi");
        assert_eq!(project_args_to_flat(r#"{"name":" lychi "}"#), "lychi");
        // Absent/empty name → "" (execute's usage guard fires).
        assert_eq!(project_args_to_flat("{}"), "");
        // A plain-string caller passes straight through.
        assert_eq!(project_args_to_flat("lychi"), "lychi");
        assert_eq!(project_args_to_flat(""), "");
        // Malformed JSON falls back to the raw string.
        assert_eq!(project_args_to_flat("{not json"), "{not json");
    }

    #[test]
    fn project_schema_requires_the_name() {
        let schema = PROJECT_GRAMMAR.handler_schema();
        assert_eq!(schema["required"], serde_json::json!(["name"]));
        assert_eq!(schema["properties"]["name"]["type"], "string");
    }
}
