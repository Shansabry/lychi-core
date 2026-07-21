//! Script Commands — a file dropped in `~/.config/lychi/scripts/` becomes a
//! named launcher command. The keyword is the filename stem (`deploy.sh` →
//! `deploy`), invoked as `deploy [args]`. This is Lychi's cheap-extensibility
//! bridge: no plugin system, no compilation — just a script.
//!
//! Discovery does NOT require the executable bit; scripts run through the login
//! shell so the shebang is honored. Optional Raycast-style header metadata:
//! ```text
//! # @lychi.title       Deploy to prod
//! # @lychi.description  Runs the deploy pipeline
//! # @lychi.mode         inline | terminal
//! # @lychi.risk         low | medium
//! ```

pub mod watcher;

use std::fs;
use std::path::{Path, PathBuf};

/// How a script's output is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptMode {
    /// Capture stdout and show it inline in Lychi (the default).
    Inline,
    /// Launch the script in a terminal window (for interactive/long scripts).
    Terminal,
}

/// A discovered Script Command.
#[derive(Debug, Clone)]
pub struct ScriptCommand {
    /// Keyword the user types (filename stem, lowercased).
    pub keyword: String,
    /// Absolute path to the script file.
    pub path: PathBuf,
    /// Display title (metadata `title`, else the keyword).
    pub title: String,
    /// One-line description (metadata `description`, else empty).
    pub description: String,
    pub mode: ScriptMode,
    /// True if the script opted into a confirmation via `@lychi.risk medium`.
    pub confirm: bool,
}

/// Files that are never scripts (editor/OS noise), matched by extension.
fn is_ignored(path: &Path) -> bool {
    // Hidden files, backups, and common non-script noise.
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.starts_with('.') {
        return true;
    }
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("bak" | "swp" | "tmp" | "md" | "txt")
    )
}

/// Scan the scripts directory and parse each file into a `ScriptCommand`.
/// Missing directory → empty list (not an error). Later files with a duplicate
/// keyword are dropped so routing stays unambiguous.
pub fn discover(dir: &Path) -> Vec<ScriptCommand> {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<ScriptCommand> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_file() || is_ignored(&path) {
            continue;
        }
        let Some(cmd) = parse_script(&path) else {
            continue;
        };
        if seen.insert(cmd.keyword.clone()) {
            out.push(cmd);
        } else {
            tracing::warn!(
                "[scripts] duplicate keyword '{}' — ignoring {}",
                cmd.keyword,
                path.display()
            );
        }
    }
    out.sort_by(|a, b| a.keyword.cmp(&b.keyword));
    out
}

/// Parse one script file: keyword from the filename stem, plus optional header
/// metadata comments. Returns `None` only if the file has no usable stem.
pub fn parse_script(path: &Path) -> Option<ScriptCommand> {
    let stem = path.file_stem().and_then(|s| s.to_str())?.trim();
    if stem.is_empty() {
        return None;
    }
    let keyword = stem.to_lowercase();

    // Read only the header (first ~40 lines) for metadata — don't slurp huge files.
    let mut title = stem.to_string();
    let mut description = String::new();
    let mut mode = ScriptMode::Inline;
    let mut confirm = false;

    if let Ok(content) = fs::read_to_string(path) {
        for line in content.lines().take(40) {
            let line = line.trim();
            // Metadata lines look like `# @lychi.<key> <value>` (any comment
            // prefix). Stop scanning once we hit a non-comment, non-blank line.
            let comment = line
                .strip_prefix('#')
                .or_else(|| line.strip_prefix("//"))
                .map(str::trim);
            let Some(comment) = comment else {
                if !line.is_empty() && !line.starts_with("#!") {
                    break; // first real code line — header is over
                }
                continue;
            };
            if let Some(rest) = comment.strip_prefix("@lychi.") {
                let (key, value) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                let value = value.trim();
                match key {
                    "title" if !value.is_empty() => title = value.to_string(),
                    "description" => description = value.to_string(),
                    "mode" => {
                        mode = match value.to_lowercase().as_str() {
                            "terminal" => ScriptMode::Terminal,
                            _ => ScriptMode::Inline,
                        }
                    }
                    "risk" => confirm = value.eq_ignore_ascii_case("medium"),
                    _ => {}
                }
            }
        }
    }

    Some(ScriptCommand {
        keyword,
        path: path.to_path_buf(),
        title,
        description,
        mode,
        confirm,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);
    fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("lychi_scripts_test_{n}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn keyword_is_filename_stem_lowercased() {
        let dir = temp_dir();
        let p = write(&dir, "Deploy.sh", "#!/bin/sh\necho hi\n");
        let cmd = parse_script(&p).unwrap();
        assert_eq!(cmd.keyword, "deploy");
        assert_eq!(cmd.title, "Deploy"); // stem preserves case as title
        assert_eq!(cmd.mode, ScriptMode::Inline);
        assert!(!cmd.confirm);
    }

    #[test]
    fn parses_header_metadata() {
        let dir = temp_dir();
        let p = write(
            &dir,
            "backup.sh",
            "#!/bin/bash\n# @lychi.title Backup home\n# @lychi.description saves ~/ to NAS\n# @lychi.mode terminal\n# @lychi.risk medium\necho go\n",
        );
        let cmd = parse_script(&p).unwrap();
        assert_eq!(cmd.title, "Backup home");
        assert_eq!(cmd.description, "saves ~/ to NAS");
        assert_eq!(cmd.mode, ScriptMode::Terminal);
        assert!(cmd.confirm);
    }

    #[test]
    fn stops_scanning_at_first_code_line() {
        let dir = temp_dir();
        // A @lychi line AFTER real code must be ignored.
        let p = write(
            &dir,
            "x.sh",
            "#!/bin/sh\necho start\n# @lychi.title Should Not Apply\n",
        );
        let cmd = parse_script(&p).unwrap();
        assert_eq!(cmd.title, "x");
    }

    #[test]
    fn discover_skips_hidden_and_noise_and_dedups() {
        let dir = temp_dir();
        write(&dir, "alpha.sh", "#!/bin/sh\n");
        write(&dir, ".hidden.sh", "#!/bin/sh\n");
        write(&dir, "notes.md", "# not a script\n");
        write(&dir, "alpha.py", "print(1)"); // duplicate keyword "alpha"
        let cmds = discover(&dir);
        let keywords: Vec<_> = cmds.iter().map(|c| c.keyword.as_str()).collect();
        assert_eq!(keywords, vec!["alpha"], "hidden/noise skipped, dedup by keyword");
    }

    #[test]
    fn missing_dir_is_empty_not_error() {
        assert!(discover(Path::new("/nonexistent/lychi/scripts")).is_empty());
    }
}
