use serde::{Deserialize, Serialize};

/// Current config schema version. Bump when a *breaking* change to the config
/// shape needs a migration (see `Config::migrate`). A config written by an older
/// Lychi loads with its stored version; migrations bring it up to `CONFIG_VERSION`.
pub const CONFIG_VERSION: u32 = 1;

fn default_config_version() -> u32 {
    // A config file without a `version` field predates versioning → treat as v1.
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(default)]
pub struct Config {
    /// Schema version — enables safe migration across breaking config changes.
    #[serde(default = "default_config_version")]
    pub version: u32,
    pub general: GeneralConfig,
    pub commands: CommandsConfig,
    pub history: HistoryConfig,
    pub ai: AiConfig,
    pub projects: ProjectsConfig,
    pub weather: WeatherConfig,
    pub privacy: PrivacyConfig,
    pub keybindings: KeybindingsConfig,
    pub suggestions: SuggestionsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // A fresh config is written at the current version.
            version: CONFIG_VERSION,
            general: GeneralConfig::default(),
            commands: CommandsConfig::default(),
            history: HistoryConfig::default(),
            ai: AiConfig::default(),
            projects: ProjectsConfig::default(),
            weather: WeatherConfig::default(),
            privacy: PrivacyConfig::default(),
            keybindings: KeybindingsConfig::default(),
            suggestions: SuggestionsConfig::default(),
        }
    }
}

/// Controls the context-aware suggestion panel.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(default)]
pub struct SuggestionsConfig {
    /// Show frecency recents (past commands) on the empty prompt.
    pub zero_state_recents: bool,
    /// Surface context-derived actions (git/docker/project) once the user types
    /// a matching keyword.
    pub context_actions_typed: bool,
}

impl Default for SuggestionsConfig {
    fn default() -> Self {
        Self {
            zero_state_recents: true,
            context_actions_typed: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(default)]
pub struct GeneralConfig {
    pub hide_on_blur: bool,
    pub show_duration_ms: bool,
    pub theme: String,
    pub hotkey: String,
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
    /// Which monitor to open the launcher on: "cursor" or "primary"
    pub monitor_mode: String,
    /// Window strategy: "auto", "layer-shell", "toplevel", "toplevel-window", or "x11"
    pub window_strategy: String,
    /// Set true once the user has seen (and dismissed) the first-run
    /// onboarding hints, e.g. the Wayland hotkey banner.
    pub first_run_completed: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            hide_on_blur: true,
            show_duration_ms: true,
            theme: "dark".to_string(),
            hotkey: "Super+Space".to_string(),
            window_x: None,
            window_y: None,
            monitor_mode: "cursor".to_string(),
            window_strategy: "auto".to_string(),
            first_run_completed: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(default)]
pub struct CommandsConfig {
    pub default_search_engine: String,
    pub youtube_url: String,
    pub shell: String,
    /// Default terminal emulator for `run` commands (auto-detected if empty).
    pub terminal: String,
    /// Additional WM classes to recognise as terminal emulators.
    /// Matched exactly (case-insensitive) — no substring matching.
    /// Example: extra_terminals = ["com.my.custom.term"]
    #[serde(default)]
    pub extra_terminals: Vec<String>,
    /// Additional WM classes to recognise as IDEs / code editors.
    /// Matched exactly (case-insensitive). Example: extra_ides = ["my-editor"]
    #[serde(default)]
    pub extra_ides: Vec<String>,
    /// Terminal routing: "auto" | "manual" | "off"
    /// - auto: always try sending to existing terminal first
    /// - manual: only route when terminal and IDE are in the same project
    /// - off: always open new terminal (current behavior)
    pub terminal_routing: String,
    /// User-defined search-engine shortcuts ("bangs"): keyword → URL template.
    /// The query is URL-encoded and appended (or substituted for `{}` if the
    /// template contains it). E.g. `gh = "https://github.com/search?q="` makes
    /// `gh tokio` open a GitHub search. Fully user-extensible — the core of a
    /// no-code "ecosystem". Ships with a few sensible defaults; user entries in
    /// config.toml override/extend them.
    #[serde(default = "default_search_engines")]
    pub search_engines: std::collections::HashMap<String, String>,
}

/// Sensible built-in search shortcuts. Users add their own in config.toml.
fn default_search_engines() -> std::collections::HashMap<String, String> {
    [
        ("gh", "https://github.com/search?q="),
        ("npm", "https://www.npmjs.com/search?q="),
        ("mdn", "https://developer.mozilla.org/en-US/search?q="),
        ("crates", "https://crates.io/search?q="),
        ("so", "https://stackoverflow.com/search?q="),
        ("wiki", "https://en.wikipedia.org/w/index.php?search="),
        ("maps", "https://www.google.com/maps/search/"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

impl Default for CommandsConfig {
    fn default() -> Self {
        Self {
            default_search_engine: "https://www.google.com/search?q=".to_string(),
            youtube_url: "https://www.youtube.com/results?search_query=".to_string(),
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
            terminal: detect_terminal(),
            extra_terminals: Vec::new(),
            extra_ides: Vec::new(),
            terminal_routing: "manual".to_string(),
            search_engines: default_search_engines(),
        }
    }
}

impl CommandsConfig {
    /// Normalize a would-be search-engine keyword: trimmed, lowercased.
    /// Bang keywords are matched case-insensitively at the router, so we store
    /// them canonically to keep collision checks and lookups consistent.
    pub fn normalize_engine_keyword(keyword: &str) -> String {
        keyword.trim().to_ascii_lowercase()
    }

    /// Reason a search-engine keyword is invalid, or `None` if it's fine.
    /// The keyword is expected to already be normalized. A keyword is rejected
    /// when it's empty, contains whitespace (keywords are a single token), or
    /// collides with a built-in command prefix (`open`, `kill`, `qr`, …) —
    /// otherwise it would shadow a real command instead of doing a web search.
    ///
    /// `is_reserved` reports whether a keyword collides with a built-in command
    /// prefix. It's injected by the caller because `config` sits upstream of the
    /// action registry and must not depend on it.
    pub fn engine_keyword_error(
        keyword: &str,
        is_reserved: &dyn Fn(&str) -> bool,
    ) -> Option<String> {
        if keyword.is_empty() {
            return Some("Keyword can't be empty".to_string());
        }
        if keyword.split_whitespace().count() != 1 {
            return Some("Keyword must be a single word (no spaces)".to_string());
        }
        if is_reserved(keyword) {
            return Some(format!(
                "\"{keyword}\" is a reserved command and can't be used as a shortcut"
            ));
        }
        None
    }

    /// Validate every user-provided search-engine keyword, returning the first
    /// error found. Used on the save path so a bad entry is rejected up front
    /// with a message the UI can surface (rather than silently dropped).
    pub fn validate_search_engines(
        &self,
        is_reserved: &dyn Fn(&str) -> bool,
    ) -> Result<(), String> {
        for keyword in self.search_engines.keys() {
            let normalized = Self::normalize_engine_keyword(keyword);
            if let Some(err) = Self::engine_keyword_error(&normalized, is_reserved) {
                return Err(err);
            }
        }
        Ok(())
    }

    /// Drop any search-engine keyword that collides with a reserved command or
    /// is otherwise malformed. Used on the load path so a hand-edited config
    /// with a bad key degrades gracefully instead of shadowing a command or
    /// failing to load. Returns the keys that were removed (for logging).
    pub fn sanitize_search_engines(&mut self, is_reserved: &dyn Fn(&str) -> bool) -> Vec<String> {
        let bad: Vec<String> = self
            .search_engines
            .keys()
            .filter(|k| {
                Self::engine_keyword_error(&Self::normalize_engine_keyword(k), is_reserved)
                    .is_some()
            })
            .cloned()
            .collect();
        for key in &bad {
            self.search_engines.remove(key);
        }
        bad
    }
}

const TERMINAL_CANDIDATES: &[&str] = &[
    "ghostty",
    "kitty",
    "alacritty",
    "wezterm",
    "foot",
    "gnome-terminal",
    "konsole",
    "xfce4-terminal",
    "mate-terminal",
    "tilix",
    "terminator",
    "ptyxis",
    "blackbox",
    "rio",
    "contour",
    "sakura",
    "xterm",
];

/// Auto-detect the user's terminal emulator from PATH (first match).
fn detect_terminal() -> String {
    for term in TERMINAL_CANDIDATES {
        if which::which(term).is_ok() {
            return term.to_string();
        }
    }
    "xterm".to_string()
}

/// Return all installed terminal emulators found in PATH.
pub fn detect_installed_terminals() -> Vec<String> {
    TERMINAL_CANDIDATES
        .iter()
        .filter(|t| which::which(t).is_ok())
        .map(|t| t.to_string())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(default)]
pub struct HistoryConfig {
    pub max_entries: usize,
    pub deduplicate: bool,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            max_entries: 500,
            deduplicate: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(default)]
pub struct ProjectsConfig {
    pub directories: Vec<String>,
    /// Extra filenames treated as strong project markers (tier 1).
    /// Example: `["flake.nix", "WORKSPACE.bazel"]`
    #[serde(default)]
    pub extra_strong_markers: Vec<String>,
    /// Extra filenames treated as soft project markers (tier 2).
    /// Soft markers only accepted when a strong marker exists in a child dir.
    /// Example: `[".devcontainer"]`
    #[serde(default)]
    pub extra_soft_markers: Vec<String>,
    /// Pinned workspace path — overrides auto-detection when set.
    #[serde(default)]
    pub pinned_workspace: Option<String>,
}

impl Default for ProjectsConfig {
    fn default() -> Self {
        Self {
            directories: vec![
                "~/Projects".to_string(),
                "~/Dev".to_string(),
                "~/Code".to_string(),
                "~/repos".to_string(),
            ],
            extra_strong_markers: Vec::new(),
            extra_soft_markers: Vec::new(),
            pinned_workspace: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(default)]
pub struct AiConfig {
    /// AI mode: "disabled", "byo", "ollama", "cloud"
    pub mode: String,
    /// BYO provider: "openai", "anthropic", "groq"
    pub provider: String,
    /// Model identifier (e.g. "claude-sonnet-4-5-20250929", "gpt-4o-mini")
    pub model: String,
    /// Ollama server URL
    pub ollama_url: String,
    /// Ollama model name (e.g. "mistral:latest", "llama3:8b")
    #[serde(default)]
    pub ollama_model: String,
    /// AI request timeout in seconds (default 8).
    #[serde(default = "default_ai_timeout")]
    pub timeout_secs: u64,
    /// Max tokens for routing/intent calls (default 300).
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_ai_timeout() -> u64 {
    8
}
fn default_max_tokens() -> u32 {
    300
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            mode: "disabled".to_string(),
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
            ollama_model: String::new(),
            timeout_secs: default_ai_timeout(),
            max_tokens: default_max_tokens(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(default)]
pub struct WeatherConfig {
    /// Temperature unit: "celsius" or "fahrenheit"
    pub unit: String,
    /// Default location when no args provided
    pub default_location: String,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            unit: "celsius".to_string(),
            default_location: String::new(),
        }
    }
}

/// Privacy consent flags — all default to false (C6: Privacy First).
/// Each flag records whether the user has consented to a specific network call.
#[derive(Debug, Clone, Default, Serialize, Deserialize, specta::Type)]
#[serde(default)]
pub struct PrivacyConfig {
    /// Allow IP geolocation (weather auto-detect via freeipapi.com)
    pub allow_ip_geolocation: bool,
    /// Allow public IP lookup (sysinfo net via ifconfig.me)
    pub allow_public_ip: bool,
}

/// Configurable keyboard shortcuts for in-app actions.
/// Uses "Modifier+Key" string format (e.g. "Ctrl+1", "Shift+Tab").
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(default)]
pub struct KeybindingsConfig {
    pub toggle_history: String,
    pub toggle_notes: String,
    pub toggle_media: String,
    pub toggle_settings: String,
    pub open_inline_url: String,
    pub submit: String,
    pub dismiss: String,
    pub tab_complete: String,
    pub tab_back: String,
    pub switch_scope: String,
    pub web_search: String,
    /// Capture a `run` shell command's output inline in Lychi instead of
    /// opening a terminal (terminal is the default). Shift+Enter.
    pub run_inline: String,
    /// Copy the selected search result's full path to the clipboard. Ctrl+Shift+C.
    pub copy_path: String,
    /// Quick screenshot: hide Lychi and capture a region. Ctrl+Shift+S.
    #[serde(default = "default_screenshot_key")]
    pub screenshot: String,
}

fn default_screenshot_key() -> String {
    "Ctrl+Shift+S".to_string()
}

impl Default for KeybindingsConfig {
    fn default() -> Self {
        Self {
            toggle_history: "Ctrl+1".to_string(),
            toggle_notes: "Ctrl+2".to_string(),
            toggle_media: "Ctrl+3".to_string(),
            toggle_settings: "Ctrl+4".to_string(),
            open_inline_url: "Ctrl+O".to_string(),
            submit: "Enter".to_string(),
            dismiss: "Escape".to_string(),
            tab_complete: "Tab".to_string(),
            tab_back: "Shift+Tab".to_string(),
            switch_scope: "Ctrl+Tab".to_string(),
            web_search: "Ctrl+Enter".to_string(),
            run_inline: "Shift+Enter".to_string(),
            copy_path: "Ctrl+Shift+C".to_string(),
            screenshot: "Ctrl+Shift+S".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand-in for the action registry's reserved-command check.
    fn reserved(w: &str) -> bool {
        ["open", "kill", "qr", "base64", "json"].contains(&w)
    }

    #[test]
    fn engine_keyword_rejects_reserved_command() {
        // "open" is a real command prefix — using it as a bang would shadow it.
        assert!(CommandsConfig::engine_keyword_error("open", &reserved).is_some());
        assert!(CommandsConfig::engine_keyword_error("kill", &reserved).is_some());
        assert!(CommandsConfig::engine_keyword_error("qr", &reserved).is_some());
    }

    #[test]
    fn engine_keyword_accepts_free_keyword() {
        assert!(CommandsConfig::engine_keyword_error("gh", &reserved).is_none());
        assert!(CommandsConfig::engine_keyword_error("jira", &reserved).is_none());
    }

    #[test]
    fn engine_keyword_rejects_empty_and_multiword() {
        assert!(CommandsConfig::engine_keyword_error("", &reserved).is_some());
        assert!(CommandsConfig::engine_keyword_error("two words", &reserved).is_some());
    }

    #[test]
    fn normalize_lowercases_and_trims() {
        assert_eq!(CommandsConfig::normalize_engine_keyword("  GH "), "gh");
        // Normalized "OPEN" still collides with the reserved prefix.
        let normalized = CommandsConfig::normalize_engine_keyword(" Open ");
        assert!(CommandsConfig::engine_keyword_error(&normalized, &reserved).is_some());
    }

    #[test]
    fn validate_rejects_config_with_reserved_keyword() {
        let mut cfg = CommandsConfig::default();
        cfg.search_engines
            .insert("open".to_string(), "https://x/?q=".to_string());
        assert!(cfg.validate_search_engines(&reserved).is_err());
    }

    #[test]
    fn sanitize_drops_reserved_keeps_valid() {
        let mut cfg = CommandsConfig::default();
        cfg.search_engines.clear();
        cfg.search_engines
            .insert("open".to_string(), "https://x/?q=".to_string());
        cfg.search_engines
            .insert("gh".to_string(), "https://github.com/search?q=".to_string());
        let dropped = cfg.sanitize_search_engines(&reserved);
        assert_eq!(dropped, vec!["open".to_string()]);
        assert!(cfg.search_engines.contains_key("gh"));
        assert!(!cfg.search_engines.contains_key("open"));
    }
}
