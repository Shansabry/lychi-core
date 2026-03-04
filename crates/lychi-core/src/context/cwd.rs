//! Terminal CWD detection.
//!
//! Parses CWD from the terminal window title (e.g. `user@host:/path`).
//! This reflects the **active tab** in tabbed terminals (GNOME Terminal,
//! Konsole, etc.).
//!
//! No `/proc` fallback — multi-tab terminals share one PID, so walking
//! the process tree picks a random tab's shell. Per C16: "No context is
//! better than wrong context."

/// Detect the working directory of a terminal process.
///
/// Parses the terminal window title for a path. Returns `None` if the
/// title doesn't contain a recognizable path (e.g. when a command is
/// running and the title shows `pnpm dev` instead of `user@host:/path`).
pub fn detect(pid: u32, wm_class: &str, title: &str) -> Option<String> {
    // Try terminal-native probe first (accurate even when commands are running)
    if let Some(cwd) = super::terminal_probe::probe_terminal_cwd(wm_class, pid, title) {
        tracing::debug!("cwd::detect: probe({wm_class}, pid={pid}) → {cwd}");
        return Some(cwd);
    }

    let home = std::env::var("HOME").unwrap_or_default();

    if let Some(cwd) = cwd_from_title(title, &home) {
        tracing::debug!("cwd::detect: from title '{}' → {}", title, cwd);
        return Some(cwd);
    }

    tracing::debug!("cwd::detect: no path (wm_class={wm_class}, pid={pid}, title='{title}')");
    None
}

/// Extract CWD from terminal window title.
///
/// Common formats:
/// - `user@host:/path/to/dir`
/// - `user@host:~/subdir` (expand ~ to $HOME)
/// - `/path/to/dir` (some terminals set just the path)
/// - `command — user@host:/path` (some terminals append command info)
/// - `user@host:/path/with spaces/dir` (paths containing spaces)
fn cwd_from_title(title: &str, home: &str) -> Option<String> {
    // Try "user@host:/path" pattern — look for `:` after `@`
    if let Some(at_pos) = title.find('@') {
        // Find the first `:` after the `@`
        let after_at = &title[at_pos..];
        if let Some(colon_offset) = after_at.find(':') {
            let path_start = at_pos + colon_offset + 1;
            let path = title[path_start..].trim();
            // Trim trailing shell prompt chars ($ %) and whitespace
            let path = path.trim_end_matches(['$', '%', ' ']);
            if let Some(resolved) = resolve_path_progressive(path, home) {
                return Some(resolved);
            }
        }
    }

    // Try bare path at the start of the title
    // Take everything up to common delimiters that aren't part of paths
    let bare = title.trim();
    if bare.starts_with('/') || bare.starts_with('~') {
        let path = bare.split(['\t', '$', '%']).next().unwrap_or(bare).trim();
        if let Some(resolved) = resolve_path_progressive(path, home) {
            return Some(resolved);
        }
    }

    None
}

/// Try to resolve a path, progressively trimming trailing segments
/// to handle cases where extra text follows the path (e.g. prompts, commands).
fn resolve_path_progressive(path: &str, home: &str) -> Option<String> {
    // First try the full path
    if let Some(resolved) = resolve_path(path, home) {
        return Some(resolved);
    }

    // Progressively trim from the end at whitespace boundaries
    // This handles "~/My Projects/app some extra text"
    let mut candidate = path;
    while let Some(last_space) = candidate.rfind(' ') {
        candidate = &candidate[..last_space];
        if let Some(resolved) = resolve_path(candidate, home) {
            return Some(resolved);
        }
    }

    None
}

/// Resolve a path string, expanding `~` and validating it exists.
/// Returns None for $HOME or non-existent paths.
fn resolve_path(path: &str, home: &str) -> Option<String> {
    let expanded = if path.starts_with("~/") {
        format!("{}{}", home, &path[1..])
    } else if path == "~" {
        return None; // $HOME is not useful
    } else {
        path.to_string()
    };

    if expanded == home {
        return None;
    }

    let p = std::path::Path::new(&expanded);
    if p.is_absolute() && p.is_dir() {
        Some(expanded)
    } else {
        None
    }
}
