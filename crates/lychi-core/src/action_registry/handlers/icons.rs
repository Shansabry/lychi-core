use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::paths;

/// Directories covered by the Tauri asset protocol scope.
/// Paths outside these are copied to icon-cache so the webview can load them.
const ALLOWED_PREFIXES: &[&str] = &[
    "/usr/share/icons/",
    "/usr/share/pixmaps/",
    "/opt/",
    "/var/lib/flatpak/",
    "/var/lib/snapd/",
];

/// Returns the icon cache directory, creating it if needed.
fn icon_cache_dir() -> PathBuf {
    let dir = paths::data_dir().join("icon-cache");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Check if a path is within the Tauri asset protocol scope.
fn is_in_asset_scope(path: &str) -> bool {
    if ALLOWED_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return true;
    }
    // $HOME/.local/share/icons/ and $HOME/.local/share/flatpak/
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if path.starts_with(&format!("{home}/.local/share/icons/"))
            || path.starts_with(&format!("{home}/.local/share/flatpak/"))
            || path.starts_with(&format!("{home}/.local/share/lychi/icon-cache/"))
        {
            return true;
        }
    }
    false
}

/// If the path is outside the asset scope, copy it to icon-cache and return the cached path.
fn ensure_in_scope(path: String) -> Option<String> {
    if is_in_asset_scope(&path) {
        return Some(path);
    }
    // Copy to icon-cache with a stable name derived from the original path
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut h);
        h.finish()
    };
    let ext = Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png");
    let cached = icon_cache_dir().join(format!("{hash:016x}.{ext}"));
    if cached.exists() {
        return cached.to_str().map(|s| s.to_string());
    }
    match std::fs::copy(&path, &cached) {
        Ok(_) => cached.to_str().map(|s| s.to_string()),
        Err(e) => {
            tracing::warn!("[icons] failed to cache {path}: {e}");
            None
        }
    }
}

/// Cached active icon theme name, detected once at startup.
static THEME: OnceLock<String> = OnceLock::new();

/// Cached list of all installed themes for broad fallback search.
static ALL_THEMES: OnceLock<Vec<String>> = OnceLock::new();

fn active_theme() -> &'static str {
    THEME.get_or_init(|| {
        freedesktop_icons::default_theme_gtk().unwrap_or_else(|| "hicolor".to_string())
    })
}

fn all_themes() -> &'static [String] {
    ALL_THEMES.get_or_init(freedesktop_icons::list_themes)
}

/// Try to find an icon by name, searching the given theme.
fn try_lookup(name: &str, theme: &str) -> Option<PathBuf> {
    freedesktop_icons::lookup(name)
        .with_size(48)
        .with_theme(theme)
        .with_cache()
        .find()
}

/// Canonicalize and convert a path to a string.
fn resolve_path(path: PathBuf) -> Option<String> {
    let resolved = path.canonicalize().unwrap_or(path);
    resolved.to_str().map(|s| s.to_string())
}

/// Search the user's theme, common fallbacks, all installed themes, and pixmaps.
fn search_all_themes(name: &str) -> Option<String> {
    let theme = active_theme();

    // 1. User's active theme (crate handles Inherits chain + hicolor fallback)
    if let Some(path) = try_lookup(name, theme) {
        return resolve_path(path);
    }

    // 2. Common fallback themes
    for fallback in ["Adwaita", "gnome", "hicolor"] {
        if *fallback != *theme
            && let Some(path) = try_lookup(name, fallback)
        {
            return resolve_path(path);
        }
    }

    // 3. Broad sweep: try all installed themes
    for t in all_themes() {
        if t != theme
            && t != "Adwaita"
            && t != "gnome"
            && t != "hicolor"
            && let Some(path) = try_lookup(name, t)
        {
            return resolve_path(path);
        }
    }

    // 4. /usr/share/pixmaps (legacy apps store icons here without themes)
    for ext in ["svg", "png", "xpm"] {
        let pixmap = PathBuf::from(format!("/usr/share/pixmaps/{name}.{ext}"));
        if pixmap.exists() {
            return resolve_path(pixmap);
        }
    }

    None
}

/// Resolve an icon name or path from a .desktop file to an actual file path.
///
/// Search order (mirrors rofi/nkutils):
/// 1. User's GTK theme (+ Inherits chain, handled by the crate)
/// 2. Common fallback themes: Adwaita, gnome, hicolor
/// 3. All installed themes (broad sweep)
/// 4. /usr/share/pixmaps (legacy apps)
/// 5. Reverse-domain stripping for Flatpak/Snap apps
pub fn resolve_icon(icon: &str) -> Option<String> {
    if icon.is_empty() {
        return None;
    }

    // Absolute path — use directly if it exists (may need scope caching)
    if icon.starts_with('/') {
        return Path::new(icon)
            .exists()
            .then(|| icon.to_string())
            .and_then(ensure_in_scope);
    }

    if let Some(found) = search_all_themes(icon) {
        return ensure_in_scope(found);
    }

    // 5. Reverse-domain stripping: "com.spotify.Client" → "Client", "spotify", "com"
    if icon.contains('.') {
        for seg in icon.rsplit('.') {
            let lower = seg.to_lowercase();
            if let Some(found) = search_all_themes(&lower) {
                return ensure_in_scope(found);
            }
        }
    }

    None
}

/// Pre-warm: detect theme + cache theme list so first real icon lookup is fast.
pub fn warmup_icons() {
    let t0 = std::time::Instant::now();
    let theme = active_theme();
    let theme_count = all_themes().len();
    tracing::info!(
        "[icons] detected theme: {theme}, {theme_count} themes available, warmup {:.0}ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );
}
