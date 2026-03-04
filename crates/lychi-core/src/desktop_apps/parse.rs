use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use super::entry::{DesktopEntry, exec_basename, make_acronym, strip_field_codes, tokenize};

/// Return the XDG application directories that should be watched for .desktop file changes.
pub fn watch_dirs() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        PathBuf::from("/var/lib/snapd/desktop/applications"),
        dirs::home_dir()
            .map(|h| h.join(".local/share/applications"))
            .unwrap_or_default(),
        dirs::home_dir()
            .map(|h| h.join(".local/share/flatpak/exports/share/applications"))
            .unwrap_or_default(),
    ]
}

/// Discover all desktop entries from XDG application directories.
pub fn discover_entries() -> Vec<DesktopEntry> {
    let dirs = watch_dirs();

    let mut entries = Vec::new();

    for dir in &dirs {
        if let Ok(read_dir) = fs::read_dir(dir) {
            for file in read_dir.flatten() {
                let path = file.path();
                if path.extension().is_some_and(|ext| ext == "desktop")
                    && let Some(entry) = parse_desktop_file(&path)
                {
                    entries.push(entry);
                }
            }
        }
    }

    entries
}

/// Parse a single .desktop file into a `DesktopEntry`.
/// Returns `None` if the entry is hidden, has no Exec=, or no Name=.
pub fn parse_desktop_file(path: &PathBuf) -> Option<DesktopEntry> {
    let content = fs::read_to_string(path).ok()?;
    let mut name = None;
    let mut exec = None;
    let mut icon = None;
    let mut generic_name = None;
    let mut keywords_raw: Option<String> = None;
    let mut wm_class = None;
    let mut no_display = false;
    let mut hidden = false;
    let mut in_desktop_entry = false;

    for line in content.lines() {
        let line = line.trim();
        if line == "[Desktop Entry]" {
            in_desktop_entry = true;
            continue;
        }
        if line.starts_with('[') {
            in_desktop_entry = false;
            continue;
        }
        if !in_desktop_entry {
            continue;
        }

        if let Some(val) = line.strip_prefix("Name=") {
            if name.is_none() {
                name = Some(val.to_string());
            }
        } else if let Some(val) = line.strip_prefix("GenericName=") {
            if generic_name.is_none() {
                generic_name = Some(val.to_string());
            }
        } else if let Some(val) = line.strip_prefix("Keywords=") {
            if keywords_raw.is_none() {
                keywords_raw = Some(val.to_string());
            }
        } else if let Some(val) = line.strip_prefix("Exec=") {
            exec = Some(strip_field_codes(val));
        } else if let Some(val) = line.strip_prefix("Icon=") {
            icon = Some(val.to_string());
        } else if let Some(val) = line.strip_prefix("StartupWMClass=") {
            wm_class = Some(val.to_string());
        } else if line == "NoDisplay=true" {
            no_display = true;
        } else if line == "Hidden=true" {
            hidden = true;
        }
    }

    if no_display || hidden {
        return None;
    }

    let name = name?;
    let exec = exec?;
    let exec_base = exec_basename(&exec);
    let keywords = parse_keywords(keywords_raw.as_deref());
    let name_tokens = tokenize(&name);
    let acronym = make_acronym(&name);

    Some(DesktopEntry {
        name,
        exec,
        exec_basename: exec_base,
        wm_class,
        generic_name,
        keywords,
        name_tokens,
        acronym,
        icon,
        desktop_path: path.to_string_lossy().into_owned(),
        icon_path: OnceLock::new(),
    })
}

/// Parse and normalize the Keywords= field.
/// Input: "internet;web;Browser;" → ["internet", "web", "browser"]
fn parse_keywords(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else { return Vec::new() };
    tokenize(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keywords_basic() {
        let kws = parse_keywords(Some("internet;web;Browser;"));
        assert!(kws.contains(&"internet".to_string()));
        assert!(kws.contains(&"web".to_string()));
        assert!(kws.contains(&"browser".to_string()));
    }

    #[test]
    fn parse_keywords_deduped() {
        let kws = parse_keywords(Some("editor;Editor;editor;"));
        assert_eq!(kws.iter().filter(|k| k.as_str() == "editor").count(), 1);
    }

    #[test]
    fn parse_keywords_stopwords_removed() {
        let kws = parse_keywords(Some("app;application;text;editor;"));
        assert!(!kws.contains(&"app".to_string()));
        assert!(!kws.contains(&"application".to_string()));
        assert!(kws.contains(&"text".to_string()));
        assert!(kws.contains(&"editor".to_string()));
    }

    #[test]
    fn exec_basename_extraction() {
        assert_eq!(exec_basename("/usr/bin/code %U"), "code");
        assert_eq!(exec_basename("firefox"), "firefox");
        assert_eq!(
            exec_basename("/usr/bin/org.gnome.Nautilus"),
            "org.gnome.nautilus"
        );
        // env + KEY=VALUE prefix
        assert_eq!(
            exec_basename("env BAMF_HINT=/foo /usr/bin/code --flag"),
            "code"
        );
        // Quoted binary
        assert_eq!(exec_basename("\"code\" --new-window %U"), "code");
    }

    #[test]
    fn acronym_generation() {
        assert_eq!(make_acronym("Visual Studio Code"), "vsc");
        assert_eq!(make_acronym("GIMP"), "g");
        assert_eq!(make_acronym("Firefox"), "f");
    }
}
