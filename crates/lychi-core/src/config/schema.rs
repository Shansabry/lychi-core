use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub general: GeneralConfig,
    pub commands: CommandsConfig,
    pub history: HistoryConfig,
    pub ai: AiConfig,
    pub projects: ProjectsConfig,
    pub weather: WeatherConfig,
    pub privacy: PrivacyConfig,
    pub keybindings: KeybindingsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Window strategy: "auto", "layer-shell", "toplevel", or "x11"
    pub window_strategy: String,
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
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Terminal routing: "auto" | "manual" | "off"
    /// - auto: always try sending to existing terminal first
    /// - manual: only route when terminal and IDE are in the same project
    /// - off: always open new terminal (current behavior)
    pub terminal_routing: String,
}

impl Default for CommandsConfig {
    fn default() -> Self {
        Self {
            default_search_engine: "https://www.google.com/search?q=".to_string(),
            youtube_url: "https://www.youtube.com/results?search_query=".to_string(),
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
            terminal: detect_terminal(),
            extra_terminals: Vec::new(),
            terminal_routing: "manual".to_string(),
        }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// AI mode: "disabled", "byo", "ollama", "cloud"
    pub mode: String,
    /// BYO provider: "openai", "anthropic", "groq"
    pub provider: String,
    /// Model identifier (e.g. "claude-sonnet-4-5-20250929", "gpt-4o-mini")
    pub model: String,
    /// Ollama server URL (for future Phase 2.1)
    pub ollama_url: String,
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
            timeout_secs: default_ai_timeout(),
            max_tokens: default_max_tokens(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyConfig {
    /// Allow IP geolocation (weather auto-detect via freeipapi.com)
    pub allow_ip_geolocation: bool,
    /// Allow public IP lookup (sysinfo net via ifconfig.me)
    pub allow_public_ip: bool,
}

/// Configurable keyboard shortcuts for in-app actions.
/// Uses "Modifier+Key" string format (e.g. "Ctrl+1", "Shift+Tab").
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        }
    }
}
