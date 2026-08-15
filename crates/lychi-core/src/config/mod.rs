pub mod db;
mod migrate;
pub mod schema;

pub use schema::*;

use std::fs;
use std::path::Path;

use crate::error::LychiError;

/// Comment banner prepended to every written `config.toml`. TOML has no way to
/// attach a comment to a serialized field, so this is how the `schema_version`
/// note reaches the file. Ends in a blank line so it sits cleanly above the
/// first section (`[meta]`).
const CONFIG_HEADER: &str = "\
# Lychi configuration. Edit the settings below freely.
#
# The `schema_version` under [meta] is managed by Lychi — it records the config
# format version so the app can migrate your settings across updates. Do not set
# it by hand; leave it as written (deleting it is harmless — Lychi restamps it).

";

impl Config {
    pub fn load(path: &Path) -> Result<Self, LychiError> {
        if !path.exists() {
            // Fresh install: defaults, stamped current so the first save writes
            // the version rather than a legacy 0 that would trigger migration.
            let mut config = Config::default();
            config.meta.schema_version = schema::CURRENT_SCHEMA_VERSION;
            return Ok(config);
        }
        let content = fs::read_to_string(path)?;

        // Default-overlay load: defaults live in `Config::default()` (code, one
        // source of truth), and the file only overrides what it sets. A partial
        // or hand-edited config.toml that omits any field/section loads fine —
        // the missing values come from the defaults, not from serde-default
        // attributes on every field. So the structs stay clean.
        let mut user: toml::Value = toml::from_str(&content)?;

        // Migrate the RAW value first, while renamed/removed keys still exist —
        // after `try_into::<Config>()` they'd already be dropped. Adding a field
        // needs no migration (the overlay below fills it); this only handles the
        // structural changes the overlay can't recover. See `config::migrate`.
        let migrated = migrate::migrate_value(&mut user);

        let mut base = toml::Value::try_from(Config::default())
            .map_err(|e| LychiError::Config(format!("serializing default config: {e}")))?;
        merge_toml(&mut base, user);
        let mut config: Config = base
            .try_into()
            .map_err(|e| LychiError::Config(e.to_string()))?;

        // Stamp the current version so a save persists it and migrations don't
        // re-run. Done unconditionally: a legacy file (no [meta] → version 0)
        // and a just-migrated file both need bringing up to CURRENT, and a file
        // already current is simply re-stamped to the same value.
        config.meta.schema_version = schema::CURRENT_SCHEMA_VERSION;
        if migrated > 0 {
            tracing::info!(
                "config: applied {migrated} migration(s), now at schema v{}",
                schema::CURRENT_SCHEMA_VERSION
            );
        }

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
        let body = toml::to_string_pretty(self).map_err(|e| LychiError::Config(e.to_string()))?;
        // TOML serializers emit no comments, so the guidance that would otherwise
        // annotate `[meta] schema_version` is prepended here as a file header.
        // `schema_version` is the one field a user might see and wrongly "fix":
        // it is managed by Lychi (stamped on load, migrations key off it), and a
        // hand-edit only risks a spurious migration — never edit it. `[meta]`
        // sorts first in the serialized output, so this banner sits right above
        // it. Regenerated on every save, so it can't drift or be lost.
        let toml_str = format!("{CONFIG_HEADER}{body}");
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
                let mut config = Config::default();
                config.meta.schema_version = schema::CURRENT_SCHEMA_VERSION;
                config
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
    fn legacy_file_is_stamped_current_on_load() {
        // A pre-versioning config.toml (no [meta]) must load AND come back
        // stamped at the current schema version, so the next save persists it
        // and migrations stop re-running. The user never writes this field.
        let path = temp_path("legacy_stamp");
        std::fs::write(&path, "[general]\ntheme = \"light\"\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.meta.schema_version, CURRENT_SCHEMA_VERSION);
        // User data still intact through the migrate → merge → stamp pipeline.
        assert_eq!(cfg.general.theme, "light");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fresh_install_carries_current_version() {
        // No file on disk → defaults, but stamped current (not a legacy 0 that
        // would needlessly trigger migration on the very first real load).
        let path = temp_path("fresh_nonexistent");
        let _ = std::fs::remove_file(&path);
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.meta.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn load_save_load_round_trips_and_persists_version() {
        // The full lifecycle the app uses: load a legacy file, save it back
        // (which writes the whole struct including the stamped [meta]), then
        // reload and confirm the version is now on disk and data survived.
        let path = temp_path("roundtrip");
        std::fs::write(&path, "[general]\ntheme = \"light\"\n[ai]\nmodel = \"m\"\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        cfg.save(&path).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("schema_version"),
            "save must persist [meta] schema_version to disk: {raw}"
        );
        // The managed-file header must be present and must precede the data, so
        // a user opening config.toml sees the "don't hand-edit schema_version"
        // note before the field itself.
        assert!(
            raw.starts_with("# Lychi configuration"),
            "save must prepend the guidance header: {raw}"
        );
        // The guidance banner (all comment lines) must precede the actual data.
        // Anchor on `schema_version = ` — the field ASSIGNMENT — which appears
        // only in the serialized body, never in the comment prose (the header
        // spells it in backticks, no `=`). Everything before it must be comment
        // or blank, so the note is unmissable above the value.
        let field_at = raw.find("schema_version = ").expect("field written");
        for line in raw[..field_at].lines() {
            let t = line.trim();
            // Comment, blank, or the `[meta]` table header — never a data
            // assignment. If any `key = value` appeared before the note, the
            // banner would no longer be the first thing a reader sees.
            assert!(
                t.is_empty() || t.starts_with('#') || t.starts_with('['),
                "only comments/blanks/section-headers may precede the data; found: {line:?}"
            );
        }
        assert!(
            raw.starts_with("# Lychi configuration"),
            "the banner must be the very first thing in the file"
        );

        let reloaded = Config::load(&path).unwrap();
        assert_eq!(reloaded.meta.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(reloaded.general.theme, "light");
        assert_eq!(reloaded.ai.model, "m");
        let _ = std::fs::remove_file(&path);
    }

    /// Developer guard: the set of top-level config sections is snapshotted here.
    /// If a section is added, renamed, or removed, this test fails — a prompt to
    /// decide whether the change needs a migration + a `CURRENT_SCHEMA_VERSION`
    /// bump (a rename/removal does; a pure addition does not, but you still
    /// acknowledge it by updating this list). It cannot detect field-level
    /// changes inside a section, so it is a reminder, not a proof — but it
    /// catches the coarse structural changes most likely to drop user data.
    #[test]
    fn top_level_sections_are_accounted_for() {
        let default = toml::Value::try_from(Config::default()).unwrap();
        let table = default.as_table().expect("config serializes to a table");
        let mut sections: Vec<&str> = table.keys().map(String::as_str).collect();
        sections.sort_unstable();
        assert_eq!(
            sections,
            [
                "ai",
                "commands",
                "file_search",
                "general",
                "history",
                "keybindings",
                "meta",
                "privacy",
                "projects",
                "suggestions",
                "weather",
            ],
            "top-level config sections changed — if this is a rename/removal, add \
             a migration in config::migrate and bump CURRENT_SCHEMA_VERSION; if a \
             pure addition, just update this list"
        );
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
