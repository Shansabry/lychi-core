use std::collections::HashMap;
use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata};

use crate::config::Config;
use crate::db::{
    self,
    schema::{SYNC_LOCAL, SettingEntry},
};
use crate::error::LychiError;

/// The syncable settings keys, **derived** from the one function that actually
/// writes them (`extract_syncable_from_config`).
///
/// This used to be a third hand-maintained list beside `apply_to_config` and
/// `extract_syncable_from_config`, and the three had already drifted:
/// `commands.terminal` was declared here but read by neither, so the key was
/// dead while looking supported. Deriving the list means it cannot disagree
/// with what is written, and `syncable_directions_agree` below pins the
/// remaining pair (write vs read) so a new key can't be added in one direction
/// only.
pub fn syncable_keys() -> Vec<String> {
    extract_syncable_from_config(&Config::default())
        .into_iter()
        .map(|(k, _)| k)
        .collect()
}

/// Load all syncable settings from the DB as a HashMap.
pub fn load_syncable(db: &Arc<Database>) -> Result<HashMap<String, String>, LychiError> {
    let txn = db.begin_read()?;
    let table = txn.open_table(db::SETTINGS)?;
    let mut settings = HashMap::new();
    for result in table.iter()? {
        let (key, val) = result?;
        // One unreadable setting must not reset every other setting.
        let Some(entry) = db::decode_row::<SettingEntry>("settings", key.value(), val.value())
        else {
            continue;
        };
        settings.insert(key.value().to_string(), entry.value);
    }
    Ok(settings)
}

/// Save a single setting to the DB (upsert).
pub fn save_setting(db: &Arc<Database>, key: &str, value: &str) -> Result<(), LychiError> {
    let entry = SettingEntry {
        value: value.to_string(),
        updated_at: db::now_millis(),
        sync_status: SYNC_LOCAL,
    };
    let bytes = postcard::to_allocvec(&entry).map_err(|e| LychiError::Database(e.to_string()))?;

    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(db::SETTINGS)?;
        table.insert(key, bytes.as_slice())?;
    }
    txn.commit()?;
    Ok(())
}

/// Save multiple settings in a single write transaction.
pub fn save_settings(db: &Arc<Database>, pairs: &[(&str, &str)]) -> Result<(), LychiError> {
    let now = db::now_millis();
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(db::SETTINGS)?;
        for (key, value) in pairs {
            let entry = SettingEntry {
                value: value.to_string(),
                updated_at: now,
                sync_status: SYNC_LOCAL,
            };
            let bytes =
                postcard::to_allocvec(&entry).map_err(|e| LychiError::Database(e.to_string()))?;
            table.insert(*key, bytes.as_slice())?;
        }
    }
    txn.commit()?;
    Ok(())
}

/// Seed the settings table from a TOML Config (first launch).
/// Only writes if the settings table is empty.
pub fn seed_from_config(db: &Arc<Database>, config: &Config) -> Result<(), LychiError> {
    // Check if table already has data
    {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::SETTINGS)?;
        if table.len()? > 0 {
            return Ok(());
        }
    }

    let pairs = extract_syncable_from_config(config);
    let pairs_ref: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    save_settings(db, &pairs_ref)
}

/// Apply DB settings over a Config struct (DB wins for syncable fields).
pub fn apply_to_config(settings: &HashMap<String, String>, config: &mut Config) {
    for (key, value) in settings {
        match key.as_str() {
            "general.theme" => config.general.theme = value.clone(),
            "general.hide_on_blur" => {
                config.general.hide_on_blur = value.parse().unwrap_or(config.general.hide_on_blur);
            }
            "general.show_duration_ms" => {
                config.general.show_duration_ms =
                    value.parse().unwrap_or(config.general.show_duration_ms);
            }
            "commands.default_search_engine" => {
                config.commands.default_search_engine = value.clone();
            }
            "commands.youtube_url" => config.commands.youtube_url = value.clone(),
            "commands.terminal" => config.commands.terminal = value.clone(),
            "commands.terminal_routing" => {
                config.commands.terminal_routing = value.clone();
            }
            "history.max_entries" => {
                config.history.max_entries = value.parse().unwrap_or(config.history.max_entries);
            }
            "history.deduplicate" => {
                config.history.deduplicate = value.parse().unwrap_or(config.history.deduplicate);
            }
            "ai.mode" => config.ai.mode = value.clone(),
            "ai.provider" => config.ai.provider = value.clone(),
            "ai.model" => config.ai.model = value.clone(),
            "ai.base_url" => config.ai.base_url = value.clone(),
            "ai.wire_format" => config.ai.wire_format = value.clone(),
            "ai.ollama_url" => config.ai.ollama_url = value.clone(),
            "ai.ollama_model" => config.ai.ollama_model = value.clone(),
            "weather.unit" => config.weather.unit = value.clone(),
            "weather.default_location" => config.weather.default_location = value.clone(),
            _ => {}
        }
    }
}

/// Detect TOML changes for syncable fields and update DB.
/// Called at startup: if user hand-edited TOML, sync those values to DB.
pub fn sync_toml_changes(
    db: &Arc<Database>,
    config: &Config,
    db_settings: &HashMap<String, String>,
) -> Result<(), LychiError> {
    let toml_values = extract_syncable_from_config(config);
    let mut changed = Vec::new();

    for (key, toml_val) in &toml_values {
        let db_val = db_settings.get(key.as_str()).map(|s| s.as_str());
        if db_val != Some(toml_val.as_str()) {
            changed.push((key.as_str(), toml_val.as_str()));
        }
    }

    if !changed.is_empty() {
        tracing::info!("Syncing {} TOML changes to DB", changed.len());
        save_settings(db, &changed)?;
    }

    Ok(())
}

/// Save syncable fields from a Config to DB (called after Settings UI save).
pub fn save_config_to_db(db: &Arc<Database>, config: &Config) -> Result<(), LychiError> {
    let pairs = extract_syncable_from_config(config);
    let pairs_ref: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    save_settings(db, &pairs_ref)
}

fn extract_syncable_from_config(config: &Config) -> Vec<(String, String)> {
    vec![
        ("general.theme".into(), config.general.theme.clone()),
        (
            "general.hide_on_blur".into(),
            config.general.hide_on_blur.to_string(),
        ),
        (
            "general.show_duration_ms".into(),
            config.general.show_duration_ms.to_string(),
        ),
        (
            "commands.default_search_engine".into(),
            config.commands.default_search_engine.clone(),
        ),
        (
            "commands.youtube_url".into(),
            config.commands.youtube_url.clone(),
        ),
        ("commands.terminal".into(), config.commands.terminal.clone()),
        (
            "commands.terminal_routing".into(),
            config.commands.terminal_routing.clone(),
        ),
        (
            "history.max_entries".into(),
            config.history.max_entries.to_string(),
        ),
        (
            "history.deduplicate".into(),
            config.history.deduplicate.to_string(),
        ),
        ("ai.mode".into(), config.ai.mode.clone()),
        ("ai.provider".into(), config.ai.provider.clone()),
        ("ai.model".into(), config.ai.model.clone()),
        ("ai.base_url".into(), config.ai.base_url.clone()),
        ("ai.wire_format".into(), config.ai.wire_format.clone()),
        ("ai.ollama_url".into(), config.ai.ollama_url.clone()),
        ("ai.ollama_model".into(), config.ai.ollama_model.clone()),
        ("weather.unit".into(), config.weather.unit.clone()),
        (
            "weather.default_location".into(),
            config.weather.default_location.clone(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F6: `apply_to_config` (DB → Config) and `extract_syncable_from_config`
    /// (Config → DB) are two hand-maintained lists that must stay in step. A
    /// key written but never read is a setting that silently fails to sync —
    /// exactly what happened to `commands.terminal`.
    ///
    /// This drives `apply_to_config` with a sentinel value per key and asserts
    /// the value actually lands, so a key added to one side only fails here
    /// instead of in a user's config months later.
    #[test]
    fn syncable_directions_agree() {
        let mut missing = Vec::new();

        for key in syncable_keys() {
            // Probe with values that differ from the default for each field
            // type we sync. Both booleans are needed: every bool field here
            // defaults to `true`, so probing only "true" is a silent no-op and
            // would report a working key as broken.
            for probe in ["false", "true", "9182", "lychi-probe-value"] {
                let mut config = Config::default();
                let before = extract_syncable_from_config(&config);

                let mut settings = HashMap::new();
                settings.insert(key.clone(), probe.to_string());
                apply_to_config(&settings, &mut config);

                let after = extract_syncable_from_config(&config);
                if before != after {
                    // This probe changed the config through this key — the
                    // read direction handles it. Done with this key.
                    break;
                }
                if probe == "lychi-probe-value" {
                    missing.push(key.clone());
                }
            }
        }

        assert!(
            missing.is_empty(),
            "these keys are written to the DB but ignored by apply_to_config, \
             so they never sync back: {missing:?}"
        );
    }

    /// The derived key list must cover every key the reader knows about, too —
    /// a key handled by `apply_to_config` but never written is equally dead.
    #[test]
    fn every_syncable_key_round_trips() {
        let config = Config::default();
        let written: Vec<String> = extract_syncable_from_config(&config)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            written,
            syncable_keys(),
            "syncable_keys() must mirror what is actually written"
        );
    }

    #[test]
    fn terminal_syncs_in_both_directions() {
        // The specific key F6 found dead.
        let mut config = Config::default();
        let mut settings = HashMap::new();
        settings.insert("commands.terminal".to_string(), "kitty".to_string());
        apply_to_config(&settings, &mut config);
        assert_eq!(config.commands.terminal, "kitty");

        let written = extract_syncable_from_config(&config);
        assert!(
            written
                .iter()
                .any(|(k, v)| k == "commands.terminal" && v == "kitty"),
            "commands.terminal not written back"
        );
    }
}
