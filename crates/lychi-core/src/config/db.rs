use std::collections::HashMap;
use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata};

use crate::config::Config;
use crate::db::{
    self,
    schema::{SYNC_LOCAL, SettingEntry},
};
use crate::error::LychiError;

/// Settings keys that are syncable to cloud. Device-local settings stay in TOML only.
#[allow(dead_code)]
const SYNCABLE_KEYS: &[&str] = &[
    "general.theme",
    "general.hide_on_blur",
    "general.show_duration_ms",
    "commands.default_search_engine",
    "commands.youtube_url",
    "commands.terminal",
    "commands.terminal_routing",
    "history.max_entries",
    "history.deduplicate",
    "ai.mode",
    "ai.provider",
    "ai.model",
    "ai.base_url",
    "ai.wire_format",
    "ai.ollama_url",
    "ai.ollama_model",
    "weather.unit",
    "weather.default_location",
];

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
