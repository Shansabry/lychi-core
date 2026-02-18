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
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CommandsConfig {
    pub default_search_engine: String,
    pub youtube_url: String,
    pub shell: String,
}

impl Default for CommandsConfig {
    fn default() -> Self {
        Self {
            default_search_engine: "https://www.google.com/search?q=".to_string(),
            youtube_url: "https://www.youtube.com/results?search_query=".to_string(),
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
        }
    }
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
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            mode: "disabled".to_string(),
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
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
