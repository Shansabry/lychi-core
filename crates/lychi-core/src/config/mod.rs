pub mod db;
pub mod schema;

pub use schema::*;

use std::fs;
use std::path::Path;

use crate::error::LychiError;

impl Config {
    pub fn load(path: &Path) -> Result<Self, LychiError> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let content = fs::read_to_string(path)?;

        // Default-overlay load: defaults live in `Config::default()` (code, one
        // source of truth), and the file only overrides what it sets. A partial
        // or hand-edited config.toml that omits any field/section loads fine —
        // the missing values come from the defaults, not from serde-default
        // attributes on every field. So the structs stay clean.
        let user: toml::Value = toml::from_str(&content)?;
        let mut base = toml::Value::try_from(Config::default())
            .map_err(|e| LychiError::Config(format!("serializing default config: {e}")))?;
        merge_toml(&mut base, user);
        let mut config: Config = base
            .try_into()
            .map_err(|e| LychiError::Config(e.to_string()))?;

        // Drop any search-engine keyword that collides with a reserved command
        // (e.g. a hand-edited `open = "..."`) so it can't shadow a real command.
        // We degrade gracefully + warn rather than fail the whole config. The
        // load path has no action registry; reserved-command collisions are
        // re-checked on save. Here we only drop structurally malformed keys.
        let dropped = config.commands.sanitize_search_engines(&|_| false);
        if !dropped.is_empty() {
            tracing::warn!(
                "Ignoring reserved/invalid search-engine keywords: {}",
                dropped.join(", ")
            );
        }
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<(), LychiError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let toml_str =
            toml::to_string_pretty(self).map_err(|e| LychiError::Config(e.to_string()))?;
        // Crash-safe: a bare `fs::write` truncates in place, so a crash mid-write
        // leaves a corrupt `config.toml` — and quicklinks (user-authored, NOT in
        // the DB sync set) would be lost on the next start's reset-to-defaults.
        // The atomic writer swaps the file in whole, fsync'd. (config → fs_atomic
        // only; must NOT reach into `backup`, which depends on config.)
        crate::fs_atomic::write_atomic(path, toml_str.as_bytes())?;
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

/// Deep-merge `overlay` onto `base` in place: for tables, recurse key-by-key so
/// a partial `[ai]` section overrides only the fields it sets; for any other
/// value, `overlay` replaces `base`. This is what lets a partial config.toml
/// override only what it specifies while the rest falls back to `Config::default`.
fn merge_toml(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base_map), toml::Value::Table(overlay_map)) => {
            for (k, v) in overlay_map {
                match base_map.get_mut(&k) {
                    Some(base_v) => merge_toml(base_v, v),
                    None => {
                        base_map.insert(k, v);
                    }
                }
            }
        }
        (base_slot, overlay_val) => *base_slot = overlay_val,
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
    fn partial_config_overlays_onto_defaults() {
        // Default-overlay load: a config.toml that sets only ONE field in ONE
        // section must load fine — that field overrides, everything else falls
        // back to Config::default(). No serde-default attributes needed.
        let path = temp_path("partial");
        std::fs::write(&path, "[general]\ntheme = \"light\"\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        // The set field overrides.
        assert_eq!(cfg.general.theme, "light");
        // Sibling fields in the same section keep their defaults.
        assert!(cfg.general.hide_on_blur);
        // Untouched sections are fully defaulted (search engines present).
        assert!(cfg.commands.search_engines.contains_key("gh"));
        assert_eq!(cfg.ai.timeout_secs, AiConfig::default().timeout_secs);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_config_file_yields_defaults() {
        let path = temp_path("empty");
        std::fs::write(&path, "").unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.general.theme, GeneralConfig::default().theme);
        assert!(cfg.commands.search_engines.contains_key("npm"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn nested_partial_section_merges_field_by_field() {
        // Setting one ai field must not wipe the others in [ai].
        let path = temp_path("nested");
        std::fs::write(&path, "[ai]\nmodel = \"custom-model\"\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.ai.model, "custom-model");
        // timeout_secs untouched → default, not zero.
        assert_eq!(cfg.ai.timeout_secs, AiConfig::default().timeout_secs);
        assert_eq!(cfg.ai.max_tokens, AiConfig::default().max_tokens);
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
        assert_eq!(cfg.general.theme, GeneralConfig::default().theme);
        // … but the user's (broken) file was backed up, not lost.
        let backup = path.with_extension("toml.bak");
        assert!(backup.exists(), "broken config should be backed up");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&backup);
    }
}
