use std::path::{Path, PathBuf};

/// Resolve an icon name or path from a .desktop file to an actual file path.
///
/// If the icon value is already an absolute path, returns it if the file exists.
/// Otherwise, searches standard XDG icon directories for matching files.
pub fn resolve_icon(icon: &str) -> Option<String> {
    if icon.is_empty() {
        return None;
    }

    // Absolute path — use directly if it exists
    if icon.starts_with('/') {
        let path = Path::new(icon);
        if path.exists() {
            return Some(icon.to_string());
        }
        return None;
    }

    // TODO: cross-platform — Linux-specific XDG icon paths. macOS/Windows use different icon systems.
    let icon_roots = [
        PathBuf::from("/usr/share/icons"),
        PathBuf::from("/usr/share/pixmaps"),
        PathBuf::from("/var/lib/flatpak/exports/share/icons"),
        PathBuf::from("/var/lib/snapd/desktop/icons"),
        dirs::home_dir()
            .map(|h| h.join(".local/share/icons"))
            .unwrap_or_default(),
        dirs::home_dir()
            .map(|h| h.join(".local/share/flatpak/exports/share/icons"))
            .unwrap_or_default(),
    ];

    // Themes to search, in priority order
    let themes = [
        "hicolor",
        "breeze",
        "breeze-dark",
        "Adwaita",
        "gnome",
        "Cosmic",
        "elementary",
        "Papirus",
        "Papirus-Dark",
    ];

    // Sizes in preference order (largest first for quality)
    let sizes = [
        "scalable", "256x256", "128x128", "64x64", "48x48", "32x32", "24x24", "22x22",
    ];

    // Categories where app icons can live
    let categories = [
        "apps",
        "actions",
        "categories",
        "devices",
        "mimetypes",
        "status",
    ];

    let mut search_dirs: Vec<PathBuf> = Vec::new();
    for root in &icon_roots {
        if root.ends_with("pixmaps") {
            // pixmaps is flat, no subdirs
            search_dirs.push(root.clone());
        } else {
            // Theme-based: theme/size/category or theme/category/size
            for theme in &themes {
                for size in &sizes {
                    for cat in &categories {
                        search_dirs.push(root.join(theme).join(size).join(cat));
                        // Some themes use category/size layout
                        search_dirs.push(root.join(theme).join(cat).join(size));
                    }
                }
            }
        }
    }

    let extensions = ["png", "svg", "xpm"];

    for dir in &search_dirs {
        for ext in &extensions {
            let candidate = dir.join(format!("{icon}.{ext}"));
            if candidate.exists() {
                // Canonicalize to resolve symlinks (e.g. Flatpak exports)
                // so the real path matches Tauri's asset protocol scope.
                let resolved = candidate.canonicalize().unwrap_or(candidate);
                return resolved.to_str().map(|s| s.to_string());
            }
        }
    }

    None
}
