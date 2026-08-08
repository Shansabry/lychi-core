use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use redb::{Database, ReadableDatabase, ReadableTable};

use crate::aliases::AliasItem;
use crate::db::{
    self,
    schema::{AliasEntry, SYNC_LOCAL},
};
use crate::error::LychiError;

/// Maximum number of aliases.
pub const MAX_ALIASES: usize = 50;

/// Global alias cache for fast lookup in the router.
static ALIAS_CACHE: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

/// Prefixes that cannot be used as alias names (would conflict with handlers).
const RESERVED_NAMES: &[&str] = &[
    "ask",
    "bm",
    "bookmark",
    "browse",
    "clip",
    "clipboard",
    "close",
    "emoji",
    "focus",
    "kill",
    "open",
    "sym",
    "unicode",
    "web",
    "yt",
    "run",
    "calc",
    "file",
    "url",
    "media",
    "project",
    "quit",
    "system",
    "note",
    "notes",
    "todo",
    "todos",
    "weather",
    "sysinfo",
    "ip",
    "cpu",
    "mem",
    "disk",
    "temp",
    "gpu",
    "battery",
    "net",
    "audio",
    "display",
    "os",
    "speedtest",
    "time",
    "tz",
    "clock",
    "alias",
    "aliases",
];

/// Warm the alias cache from redb. Call at startup.
pub fn warm_cache(db: &Arc<Database>) {
    let store = AliasesStore::new();
    let aliases = store.get_aliases(db).unwrap_or_default();
    let map: HashMap<String, String> = aliases
        .into_iter()
        .map(|a| (a.name.to_lowercase(), a.command))
        .collect();
    let _ = ALIAS_CACHE.set(RwLock::new(map));
}

/// Look up an alias from cache. Returns the expanded command if found.
pub fn lookup(name: &str) -> Option<String> {
    ALIAS_CACHE.get()?.read().ok()?.get(name).cloned()
}

/// Refresh the cache after CRUD operations.
fn refresh_cache(db: &Arc<Database>) {
    if let Some(lock) = ALIAS_CACHE.get() {
        let store = AliasesStore::new();
        if let Ok(aliases) = store.get_aliases(db) {
            let map: HashMap<String, String> = aliases
                .into_iter()
                .map(|a| (a.name.to_lowercase(), a.command))
                .collect();
            if let Ok(mut cache) = lock.write() {
                *cache = map;
            }
        }
    }
}

#[derive(Default)]
pub struct AliasesStore;

impl AliasesStore {
    pub fn new() -> Self {
        Self
    }

    /// List all non-deleted aliases.
    pub fn get_aliases(&self, db: &Arc<Database>) -> Result<Vec<AliasItem>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::ALIASES)?;
        let mut aliases = Vec::new();
        for result in table.iter()? {
            let (_, val) = result?;
            // One unreadable row must not hide the rest of the list.
            let Some(entry) = db::decode_row::<AliasEntry>("aliases", "?", val.value()) else {
                continue;
            };
            if entry.deleted_at.is_none() {
                aliases.push(AliasItem {
                    name: entry.name,
                    command: entry.command,
                    created_at: entry.created_at,
                    updated_at: entry.updated_at,
                });
            }
        }
        Ok(aliases)
    }

    /// Look up a single alias by name.
    pub fn get_alias(
        &self,
        db: &Arc<Database>,
        name: &str,
    ) -> Result<Option<AliasItem>, LychiError> {
        let key = name.to_lowercase();
        let txn = db.begin_read()?;
        let table = txn.open_table(db::ALIASES)?;
        match table.get(key.as_str())? {
            Some(val) => {
                let entry: AliasEntry = crate::db::decode_value(val.value())?;
                if entry.deleted_at.is_some() {
                    return Ok(None);
                }
                Ok(Some(AliasItem {
                    name: entry.name,
                    command: entry.command,
                    created_at: entry.created_at,
                    updated_at: entry.updated_at,
                }))
            }
            None => Ok(None),
        }
    }

    /// Add a new alias.
    pub fn add_alias(
        &self,
        db: &Arc<Database>,
        name: &str,
        command: &str,
    ) -> Result<AliasItem, LychiError> {
        let name_lower = name.trim().to_lowercase();
        let command = command.trim().to_string();

        // Validate name
        if name_lower.is_empty() {
            return Err(LychiError::Alias("Alias name cannot be empty".into()));
        }
        if name_lower.contains(char::is_whitespace) {
            return Err(LychiError::Alias("Alias name cannot contain spaces".into()));
        }
        if command.is_empty() {
            return Err(LychiError::Alias("Alias command cannot be empty".into()));
        }
        if RESERVED_NAMES.contains(&name_lower.as_str()) {
            return Err(LychiError::Alias(format!(
                "'{name_lower}' is a reserved command name"
            )));
        }

        // Check for existing (non-deleted) alias with same name
        if self.get_alias(db, &name_lower)?.is_some() {
            return Err(LychiError::Alias(format!(
                "Alias '{name_lower}' already exists"
            )));
        }

        // Check alias count
        let count = self.alias_count(db)?;
        if count >= MAX_ALIASES {
            return Err(LychiError::Alias(format!(
                "Maximum of {MAX_ALIASES} aliases reached"
            )));
        }

        let now = db::now_millis();
        let entry = AliasEntry {
            name: name_lower.clone(),
            command: command.clone(),
            created_at: now,
            updated_at: now,
            deleted_at: None,
            sync_status: SYNC_LOCAL,
        };

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::ALIASES)?;
            let bytes = crate::db::encode_row(&entry)?;
            table.insert(name_lower.as_str(), bytes.as_slice())?;
        }
        txn.commit()?;

        refresh_cache(db);

        Ok(AliasItem {
            name: name_lower,
            command,
            created_at: now,
            updated_at: now,
        })
    }

    /// Update the command for an existing alias.
    pub fn update_alias(
        &self,
        db: &Arc<Database>,
        name: &str,
        command: &str,
    ) -> Result<(), LychiError> {
        let key = name.to_lowercase();
        let command = command.trim().to_string();
        if command.is_empty() {
            return Err(LychiError::Alias("Alias command cannot be empty".into()));
        }

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::ALIASES)?;
            let existing_val = table
                .get(key.as_str())?
                .ok_or_else(|| LychiError::Alias(format!("Alias not found: {key}")))?;
            let mut entry: AliasEntry = crate::db::decode_value(existing_val.value())?;
            if entry.deleted_at.is_some() {
                return Err(LychiError::Alias(format!("Alias not found: {key}")));
            }
            entry.command = command;
            entry.updated_at = db::now_millis();
            let bytes = crate::db::encode_row(&entry)?;
            drop(existing_val);
            table.insert(key.as_str(), bytes.as_slice())?;
        }
        txn.commit()?;

        refresh_cache(db);
        Ok(())
    }

    /// Soft-delete an alias.
    pub fn delete_alias(&self, db: &Arc<Database>, name: &str) -> Result<(), LychiError> {
        let key = name.to_lowercase();
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::ALIASES)?;
            let existing_val = table
                .get(key.as_str())?
                .ok_or_else(|| LychiError::Alias(format!("Alias not found: {key}")))?;
            let mut entry: AliasEntry = crate::db::decode_value(existing_val.value())?;
            if entry.deleted_at.is_some() {
                return Err(LychiError::Alias(format!("Alias not found: {key}")));
            }
            entry.deleted_at = Some(db::now_millis());
            let bytes = crate::db::encode_row(&entry)?;
            drop(existing_val);
            table.insert(key.as_str(), bytes.as_slice())?;
        }
        txn.commit()?;

        refresh_cache(db);
        Ok(())
    }

    fn alias_count(&self, db: &Arc<Database>) -> Result<usize, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::ALIASES)?;
        let mut count = 0;
        for result in table.iter()? {
            let (_, val) = result?;
            // One unreadable row must not hide the rest of the list.
            let Some(entry) = db::decode_row::<AliasEntry>("aliases", "?", val.value()) else {
                continue;
            };
            if entry.deleted_at.is_none() {
                count += 1;
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_test_database;

    #[test]
    fn alias_add_and_list() {
        let db = open_test_database();
        let store = AliasesStore::new();
        assert!(store.get_aliases(&db).unwrap().is_empty());

        let item = store
            .add_alias(&db, "deploy", "run ssh prod cd /app && git pull")
            .unwrap();
        assert_eq!(item.name, "deploy");
        assert_eq!(item.command, "run ssh prod cd /app && git pull");

        let aliases = store.get_aliases(&db).unwrap();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].name, "deploy");
    }

    #[test]
    fn alias_duplicate_rejected() {
        let db = open_test_database();
        let store = AliasesStore::new();
        store
            .add_alias(&db, "gh", "open https://github.com")
            .unwrap();
        let err = store
            .add_alias(&db, "gh", "open https://gitlab.com")
            .unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn alias_delete() {
        let db = open_test_database();
        let store = AliasesStore::new();
        store
            .add_alias(&db, "gh", "open https://github.com")
            .unwrap();
        assert_eq!(store.alias_count(&db).unwrap(), 1);
        store.delete_alias(&db, "gh").unwrap();
        assert_eq!(store.alias_count(&db).unwrap(), 0);
        assert!(store.get_alias(&db, "gh").unwrap().is_none());
    }

    #[test]
    fn alias_update() {
        let db = open_test_database();
        let store = AliasesStore::new();
        store
            .add_alias(&db, "gh", "open https://github.com")
            .unwrap();
        store
            .update_alias(&db, "gh", "open https://github.com/my-repo")
            .unwrap();
        let alias = store.get_alias(&db, "gh").unwrap().unwrap();
        assert_eq!(alias.command, "open https://github.com/my-repo");
    }

    #[test]
    fn alias_name_validation() {
        let db = open_test_database();
        let store = AliasesStore::new();

        // Empty name
        assert!(store.add_alias(&db, "", "run foo").is_err());
        // Spaces in name
        assert!(store.add_alias(&db, "my alias", "run foo").is_err());
        // Empty command
        assert!(store.add_alias(&db, "foo", "").is_err());
        // Reserved name
        assert!(store.add_alias(&db, "open", "run foo").is_err());
        assert!(store.add_alias(&db, "web", "run foo").is_err());
        assert!(store.add_alias(&db, "alias", "run foo").is_err());
    }

    #[test]
    fn alias_case_insensitive() {
        let db = open_test_database();
        let store = AliasesStore::new();
        store
            .add_alias(&db, "GH", "open https://github.com")
            .unwrap();
        let alias = store.get_alias(&db, "gh").unwrap().unwrap();
        assert_eq!(alias.name, "gh"); // stored lowercase
    }
}
