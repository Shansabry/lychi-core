use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

    // Absolute path — use directly if it exists
    if icon.starts_with('/') {
        return Path::new(icon).exists().then(|| icon.to_string());
    }

    if let Some(found) = search_all_themes(icon) {
        return Some(found);
    }

    // 5. Reverse-domain stripping: "com.spotify.Client" → "Client", "spotify", "com"
    if icon.contains('.') {
        for seg in icon.rsplit('.') {
            let lower = seg.to_lowercase();
            if let Some(found) = search_all_themes(&lower) {
                return Some(found);
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
