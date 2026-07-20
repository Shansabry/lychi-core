pub mod db;
pub mod schema;

pub use schema::*;

use std::fs;
use std::path::Path;

use crate::error::LychiError;

impl Config {
    pub fn load(path: &Path) -> Result<Self, LychiError> {
        if path.exists() {
            let content = fs::read_to_string(path)?;
            let mut config: Config = toml::from_str(&content)?;
            config.migrate();
            // Drop any search-engine keyword that collides with a reserved
            // command (e.g. a hand-edited `open = "..."`) so it can't shadow a
            // real command. Rejecting here would fail the whole config, so we
            // degrade gracefully and warn instead.
            // The load path has no action registry available. Reserved-command
            // collisions are re-checked on the save path (which does have one);
            // here we only drop structurally malformed keys (empty / multi-word).
            let dropped = config.commands.sanitize_search_engines(&|_| false);
            if !dropped.is_empty() {
                tracing::warn!(
                    "Ignoring reserved/invalid search-engine keywords: {}",
                    dropped.join(", ")
                );
            }
            Ok(config)
        } else {
            Ok(Config::default())
        }
    }

    /// Migrate an older config in-place up to `CONFIG_VERSION`. Each step handles
    /// one version bump; add a match arm per breaking change. Runs after load so
    /// a config written by an older Lychi is brought current before use.
    ///
    /// (No migrations are needed for v1 — this is the framework so the *next*
    /// breaking change has a home instead of silently corrupting configs.)
    fn migrate(&mut self) {
        while self.version < CONFIG_VERSION {
            match self.version {
                // Example for the future:
                // 1 => { /* rename ai.mode values, etc. */ self.version = 2; }
                _ => {
                    // Unknown/newer-than-us version, or no migration defined:
                    // stamp current and stop rather than loop forever.
                    self.version = CONFIG_VERSION;
                }
            }
        }
        // A config newer than this binary (downgrade) keeps its version; serde
        // defaults fill any fields this older binary doesn't know about.
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
                // A parse error used to SILENTLY discard the user's entire config
                // (one bad hand-edited field → everything reset). Instead, back
                // the broken file up so their settings aren't lost, and log
                // loudly. The user (or a future repair path) can recover from the
                // `.bak` file.
                if path.exists() {
                    let backup = path.with_extension("toml.bak");
                    match fs::copy(path, &backup) {
                        Ok(_) => tracing::error!(
                            "Config at {} failed to parse: {e}. Backed it up to {} \
                             and starting with defaults — your file was NOT overwritten \
                             on disk until the next save.",
                            path.display(),
                            backup.display()
                        ),
                        Err(be) => tracing::error!(
                            "Config at {} failed to parse: {e}. (Backup to {} also \
                             failed: {be}.) Starting with defaults.",
                            path.display(),
                            backup.display()
                        ),
                    }
                } else {
                    tracing::warn!(
                        "Failed to load config from {}: {e} — using defaults",
                        path.display()
                    );
                }
                Config::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    // Unique temp paths per test to avoid clashes under the parallel runner.
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    fn temp_path(tag: &str) -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("lychi_cfg_test_{tag}_{n}.toml"))
    }

    #[test]
    fn fresh_default_is_current_version() {
        assert_eq!(Config::default().version, CONFIG_VERSION);
    }

    #[test]
    fn config_without_version_field_loads_as_v1() {
        // A pre-versioning config (no `version =` line) must load as v1, not 0.
        let path = temp_path("noversion");
        std::fs::write(&path, "[general]\ntheme = \"dark\"\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.version, 1);
        assert_eq!(cfg.general.theme, "dark");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn broken_config_is_backed_up_not_discarded_silently() {
        // The dangerous old behavior: a parse error silently wiped everything.
        // Now the broken file must be preserved as a .bak.
        let path = temp_path("broken");
        std::fs::write(&path, "this is not valid toml =[[[").unwrap();
        let cfg = Config::load_or_default(&path);
        // Falls back to defaults …
        assert_eq!(cfg.version, CONFIG_VERSION);
        // … but the user's (broken) file was backed up, not lost.
        let backup = path.with_extension("toml.bak");
        assert!(backup.exists(), "broken config should be backed up");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&backup);
    }

    #[test]
    fn migrate_stamps_current_version() {
        // A config claiming an old version gets stamped current after migrate.
        let mut cfg = Config {
            version: 0,
            ..Default::default()
        };
        cfg.migrate();
        assert_eq!(cfg.version, CONFIG_VERSION);
    }
}
