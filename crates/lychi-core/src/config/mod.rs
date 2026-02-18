mod schema;

pub use schema::*;

use std::fs;
use std::path::Path;

use crate::error::LychiError;

impl Config {
    pub fn load(path: &Path) -> Result<Self, LychiError> {
        if path.exists() {
            let content = fs::read_to_string(path)?;
            let config: Config = toml::from_str(&content)?;
            Ok(config)
        } else {
            Ok(Config::default())
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), LychiError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let toml_str =
            toml::to_string_pretty(self).map_err(|e| LychiError::Config(e.to_string()))?;
        fs::write(path, toml_str)?;
        Ok(())
    }

    pub fn load_or_default(path: &Path) -> Self {
        match Self::load(path) {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!(
                    "Failed to load config from {}: {e} — using defaults",
                    path.display()
                );
                Config::default()
            }
        }
    }
}
