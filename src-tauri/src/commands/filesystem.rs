use lychi_core::action_registry::CompletionItem;
use lychi_core::error::LychiError;
use std::path::PathBuf;

/// Resolve a partial path to an absolute path.
/// - `/...` → absolute path as-is
/// - `~/...` → expand ~ to home
/// - anything else → treat as relative to home directory (e.g. `Do` → `~/Do`)
fn resolve_path(raw: &str) -> PathBuf {
    if raw.starts_with('/') {
        PathBuf::from(raw)
    } else if let Some(rest) = raw.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(raw))
    } else if raw == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw))
    } else {
        // Bare name like "Do" → treat as ~/Do
        dirs::home_dir()
            .map(|h| h.join(raw))
            .unwrap_or_else(|| PathBuf::from(raw))
    }
}

/// Build a display label using `~/` prefix for home-relative paths.
fn build_label(original_partial: &str, entry_name: &str, is_dir: bool) -> String {
    let trailing = if is_dir { "/" } else { "" };

    if original_partial.starts_with('/') {
        // Absolute path — preserve the directory prefix
        let prefix = if let Some(idx) = original_partial.rfind('/') {
            &original_partial[..=idx]
        } else {
            "/"
        };
        format!("{prefix}{entry_name}{trailing}")
    } else {
        // Home-relative — always show ~/
        let prefix = if let Some(idx) = original_partial.rfind('/') {
            // e.g. "Documents/sub" → "~/Documents/"
            format!("~/{}", &original_partial[..=idx])
        } else {
            "~/".to_string()
        };
        format!("{prefix}{entry_name}{trailing}")
    }
}

/// Given the text after `@`, return filesystem completions.
///
/// - Empty or `~` → list home directory
/// - Bare name (e.g. `Do`) → filter home directory contents
/// - `/...` → absolute path
/// - Directories sort before files, max 10 results
#[tauri::command]
pub async fn list_path_completions(partial: String) -> Result<Vec<CompletionItem>, LychiError> {
    let raw = partial.trim();

    let (dir_to_list, stem_filter): (PathBuf, String) =
        if raw.is_empty() || raw == "~" || raw == "~/" {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
            (home, String::new())
        } else {
            let resolved = resolve_path(raw);
            if raw.ends_with('/') {
                (resolved, String::new())
            } else {
                let parent = resolved
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("/"));
                let stem = resolved
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                (parent, stem)
            }
        };

    if !dir_to_list.exists() || !dir_to_list.is_dir() {
        return Ok(Vec::new());
    }

    let read_dir = std::fs::read_dir(&dir_to_list)?;

    let mut entries: Vec<CompletionItem> = read_dir
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            // Skip hidden files unless the user is explicitly typing a dot
            if name.starts_with('.') && !stem_filter.starts_with('.') {
                return None;
            }
            // Case-insensitive prefix filter
            if !stem_filter.is_empty()
                && !name.to_lowercase().starts_with(&stem_filter.to_lowercase())
            {
                return None;
            }
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            let label = build_label(raw, &name, is_dir);
            Some(CompletionItem {
                label,
                icon_path: if is_dir {
                    Some("__folder__".into())
                } else {
                    None
                },
                score: if is_dir { 100 } else { 50 },
            })
        })
        .collect();

    // Dirs first, then files, alphabetical within each group
    entries.sort_by(|a, b| {
        let a_dir = a.icon_path.as_deref() == Some("__folder__");
        let b_dir = b.icon_path.as_deref() == Some("__folder__");
        b_dir.cmp(&a_dir).then(a.label.cmp(&b.label))
    });

    entries.truncate(10);
    Ok(entries)
}

/// List subdirectories of the given path (directories only, absolute paths).
/// Used by the in-app folder picker to avoid the native GTK dialog which
/// crashes on Wayland layer-shell surfaces.
#[tauri::command]
pub async fn list_directories(path: String) -> Result<Vec<DirEntry>, LychiError> {
    let dir = if path.is_empty() {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    } else {
        resolve_path(&path)
    };

    if !dir.exists() || !dir.is_dir() {
        return Ok(Vec::new());
    }

    let read_dir = std::fs::read_dir(&dir)?;

    let mut entries: Vec<DirEntry> = read_dir
        .flatten()
        .filter_map(|entry| {
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if !is_dir {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            if name.starts_with('.') {
                return None;
            }
            let path = entry.path().to_string_lossy().to_string();
            Some(DirEntry { name, path })
        })
        .collect();

    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(entries)
}

#[derive(serde::Serialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
}
