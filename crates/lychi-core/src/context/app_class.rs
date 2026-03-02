//! Application classification by WM class.
//!
//! Categorises the focused window so the suggestion engine can adapt
//! recommendations to the application type (browser, media player, etc.).

use super::active_window;

/// High-level application category derived from `wm_class`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppClass {
    Terminal,
    Ide,
    Browser,
    MediaPlayer,
    FileManager,
    Communication,
    Unknown,
}

const BROWSERS: &[&str] = &[
    "firefox",
    "chromium",
    "google-chrome",
    "brave-browser",
    "vivaldi",
    "opera",
    "zen-browser",
    "zen",
    "librewolf",
    "microsoft-edge",
    "epiphany",
    "falkon",
    "waterfox",
    "floorp",
    "thorium",
];

const MEDIA_PLAYERS: &[&str] = &[
    "spotify",
    "vlc",
    "mpv",
    "celluloid",
    "totem",
    "rhythmbox",
    "elisa",
    "strawberry",
    "audacious",
    "lollypop",
    "amberol",
    "clementine",
];

const FILE_MANAGERS: &[&str] = &[
    "nautilus",
    "org.gnome.nautilus",
    "dolphin",
    "thunar",
    "nemo",
    "pcmanfm",
    "caja",
    "krusader",
    "spacefm",
];

const COMMUNICATION: &[&str] = &[
    "slack",
    "discord",
    "telegram",
    "telegramdesktop",
    "signal",
    "thunderbird",
    "element",
    "fractal",
    "teams",
    "microsoft teams",
];

/// Classify a window by its WM class string.
///
/// Order matters: check specific app categories first, then fall through to
/// terminal/IDE. The terminal classifier uses substring matching (e.g. `"st"`)
/// which can false-positive on classes like "vivaldi-stable" or "jetbrains-rustrover".
pub fn classify(wm_class: &str) -> AppClass {
    let lower = wm_class.to_lowercase();

    // Check specific app categories first (exact enough to avoid false positives).
    if BROWSERS.iter().any(|b| lower.contains(b)) {
        return AppClass::Browser;
    }
    if MEDIA_PLAYERS.iter().any(|m| lower.contains(m)) {
        return AppClass::MediaPlayer;
    }
    if FILE_MANAGERS.iter().any(|f| lower.contains(f)) {
        return AppClass::FileManager;
    }
    if COMMUNICATION.iter().any(|c| lower.contains(c)) {
        return AppClass::Communication;
    }

    // IDE before terminal — IDE names are longer/more specific, while the terminal
    // list includes "st" which false-positives on "jetbrains-rustrover" etc.
    if active_window::is_ide_class(wm_class) {
        return AppClass::Ide;
    }
    if active_window::is_terminal_class(wm_class) {
        return AppClass::Terminal;
    }

    AppClass::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_browsers() {
        assert_eq!(classify("firefox"), AppClass::Browser);
        assert_eq!(classify("Google-chrome"), AppClass::Browser);
        assert_eq!(classify("brave-browser"), AppClass::Browser);
        assert_eq!(classify("zen-browser"), AppClass::Browser);
        assert_eq!(classify("Vivaldi-stable"), AppClass::Browser);
    }

    #[test]
    fn test_classify_terminals() {
        assert_eq!(classify("Alacritty"), AppClass::Terminal);
        assert_eq!(classify("kitty"), AppClass::Terminal);
        assert_eq!(classify("org.gnome.Terminal"), AppClass::Terminal);
    }

    #[test]
    fn test_classify_ides() {
        assert_eq!(classify("code"), AppClass::Ide);
        assert_eq!(classify("cursor"), AppClass::Ide);
        assert_eq!(classify("jetbrains-rustrover"), AppClass::Ide);
    }

    #[test]
    fn test_classify_media() {
        assert_eq!(classify("spotify"), AppClass::MediaPlayer);
        assert_eq!(classify("vlc"), AppClass::MediaPlayer);
        assert_eq!(classify("mpv"), AppClass::MediaPlayer);
    }

    #[test]
    fn test_classify_file_managers() {
        assert_eq!(classify("org.gnome.Nautilus"), AppClass::FileManager);
        assert_eq!(classify("dolphin"), AppClass::FileManager);
        assert_eq!(classify("thunar"), AppClass::FileManager);
    }

    #[test]
    fn test_classify_communication() {
        assert_eq!(classify("Slack"), AppClass::Communication);
        assert_eq!(classify("discord"), AppClass::Communication);
        assert_eq!(classify("TelegramDesktop"), AppClass::Communication);
    }

    #[test]
    fn test_classify_unknown() {
        assert_eq!(classify("gimp"), AppClass::Unknown);
        assert_eq!(classify("libreoffice"), AppClass::Unknown);
        assert_eq!(classify("random-app"), AppClass::Unknown);
    }
}
