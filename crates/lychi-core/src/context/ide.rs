//! IDE workspace detection — 3-tier layered approach.
//!
//! Tier 1: Read IDE config file for exact workspace path (VS Code, Cursor, etc.)
//! Tier 2: Parse window title for folder name + resolve on disk
//! Tier 3: (handled elsewhere) Window stack terminal scan
//!
//! Per C16: "No context is better than wrong context." Every tier validates
//! the resolved path exists on disk before returning it.

use std::path::Path;

/// Detect workspace path from an IDE window.
///
/// Returns the absolute path to the project root, or `None` if detection fails.
pub fn detect_workspace(title: &str, wm_class: &str) -> Option<String> {
    // Tier 1: IDE config file (exact path, most reliable)
    if let Some(ws) = workspace_from_config(wm_class) {
        tracing::debug!("ide::detect: from config → {}", ws);
        return Some(ws);
    }

    // Tier 2: Title parsing + filesystem resolution
    if let Some(ws) = workspace_from_title(title) {
        tracing::debug!("ide::detect: from title '{}' → {}", title, ws);
        return Some(ws);
    }

    tracing::debug!(
        "ide::detect: no workspace found for '{}' (title='{}')",
        wm_class,
        title
    );
    None
}

// ── Tier 1: Config File ──────────────────────────────────────────────────

/// Map wm_class to the IDE's config directory name under `~/.config/`.
fn config_dir_for_ide(wm_class: &str) -> Option<&'static str> {
    let lower = wm_class.to_lowercase();
    // Order matters: "cursor" before "code" (Cursor's class might contain "code")
    if lower.contains("cursor") {
        return Some("Cursor");
    }
    if lower.contains("windsurf") {
        return Some("Windsurf");
    }
    if lower.contains("vscodium") {
        return Some("VSCodium");
    }
    if lower.contains("code") {
        return Some("Code");
    }
    None // JetBrains, Zed, etc. — no config file support yet
}

/// Read the active workspace path from the IDE's storage.json.
///
/// VS Code/Cursor/Windsurf/VSCodium store it at:
/// `~/.config/<IDE>/User/globalStorage/storage.json`
/// → `windowsState.lastActiveWindow.folder` = `"file:///path/to/project"`
fn workspace_from_config(wm_class: &str) -> Option<String> {
    let ide_name = config_dir_for_ide(wm_class)?;
    let home = std::env::var("HOME").ok()?;
    let path = format!(
        "{}/.config/{}/User/globalStorage/storage.json",
        home, ide_name
    );

    let content = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let folder = json["windowsState"]["lastActiveWindow"]["folder"].as_str()?;

    let raw_path = folder.strip_prefix("file://")?;
    let decoded = urlencoding::decode(raw_path).ok()?;
    let p = Path::new(decoded.as_ref());

    if p.is_absolute() && p.is_dir() {
        let s = decoded.into_owned();
        let home_check = std::env::var("HOME").unwrap_or_default();
        if s == home_check {
            return None; // $HOME is not useful context
        }
        Some(s)
    } else {
        None
    }
}

// ── Tier 2: Title Parsing ────────────────────────────────────────────────

/// Extract the folder/project name from an IDE window title and resolve it on disk.
fn workspace_from_title(title: &str) -> Option<String> {
    let folder_name = extract_folder_from_title(title)?;
    let home = std::env::var("HOME").unwrap_or_default();

    // Default parent directories to search
    let mut search_dirs = vec![
        home.clone(),
        format!("{}/Projects", home),
        format!("{}/projects", home),
        format!("{}/Developer", home),
        format!("{}/dev", home),
        format!("{}/workspace", home),
        format!("{}/code", home),
        format!("{}/Code", home),
        format!("{}/repos", home),
        format!("{}/src", home),
    ];

    // Add user-configured project_dirs
    if let Some(extra) = read_project_dirs() {
        search_dirs.extend(extra);
    }

    for parent in &search_dirs {
        let candidate = format!("{}/{}", parent, folder_name);
        let p = Path::new(&candidate);
        if p.is_dir() && has_project_marker(p) {
            if candidate == home {
                continue; // Skip $HOME
            }
            return Some(candidate);
        }
    }

    None
}

/// Extract the project/folder name from an IDE window title.
///
/// All major IDEs use ` — ` (em-dash U+2014) as separator:
/// - `file.rs — Lychi — Visual Studio Code` → `Lychi`
/// - `Lychi — Visual Studio Code` → `Lychi`
/// - `Lychi — IntelliJ IDEA` → `Lychi`
fn extract_folder_from_title(title: &str) -> Option<&str> {
    let parts: Vec<&str> = title.split(" \u{2014} ").collect();
    match parts.len() {
        n if n >= 3 => Some(parts[n - 2].trim()),
        2 => Some(parts[0].trim()),
        _ => None,
    }
}

/// Check if a directory contains a project marker file.
fn has_project_marker(path: &Path) -> bool {
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "setup.py",
        "Makefile",
        "CMakeLists.txt",
        "build.gradle",
        "pom.xml",
    ];
    MARKERS.iter().any(|m| path.join(m).exists())
}

/// Read user-configured project directories.
///
/// Reads from `~/.config/lychi/project_dirs.json`: `["/mnt/DevSSD", "/mnt/Data/work"]`
fn read_project_dirs() -> Option<Vec<String>> {
    let config_dir = crate::paths::config_dir();
    let path = config_dir.join("project_dirs.json");
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str::<Vec<String>>(&content).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_folder_3_segments() {
        assert_eq!(
            extract_folder_from_title("mod.rs \u{2014} Lychi \u{2014} Visual Studio Code"),
            Some("Lychi")
        );
    }

    #[test]
    fn test_extract_folder_2_segments() {
        assert_eq!(
            extract_folder_from_title("Lychi \u{2014} Visual Studio Code"),
            Some("Lychi")
        );
    }

    #[test]
    fn test_extract_folder_jetbrains() {
        assert_eq!(
            extract_folder_from_title("Main.java \u{2014} myapp \u{2014} IntelliJ IDEA"),
            Some("myapp")
        );
    }

    #[test]
    fn test_extract_folder_zed() {
        assert_eq!(
            extract_folder_from_title("file.rs \u{2014} Lychi \u{2014} Zed"),
            Some("Lychi")
        );
    }

    #[test]
    fn test_extract_folder_single_segment_returns_none() {
        assert_eq!(extract_folder_from_title("Visual Studio Code"), None);
    }

    #[test]
    fn test_extract_folder_untitled_returns_name() {
        // "Untitled-1" is returned but will fail filesystem resolution
        assert_eq!(
            extract_folder_from_title("Untitled-1 \u{2014} Visual Studio Code"),
            Some("Untitled-1")
        );
    }

    #[test]
    fn test_config_dir_mapping() {
        assert_eq!(config_dir_for_ide("code"), Some("Code"));
        assert_eq!(config_dir_for_ide("code - oss"), Some("Code"));
        assert_eq!(config_dir_for_ide("cursor"), Some("Cursor"));
        assert_eq!(config_dir_for_ide("vscodium"), Some("VSCodium"));
        assert_eq!(config_dir_for_ide("windsurf"), Some("Windsurf"));
        assert_eq!(config_dir_for_ide("jetbrains-idea"), None);
        assert_eq!(config_dir_for_ide("zed"), None);
    }

    #[test]
    fn test_has_project_marker() {
        // The Lychi project root should have .git and Cargo.toml
        let lychi_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        assert!(has_project_marker(lychi_root));
    }
}
