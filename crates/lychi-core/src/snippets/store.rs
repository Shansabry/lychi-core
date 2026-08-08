use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable};

use crate::db::{
    self,
    schema::{SYNC_LOCAL, SnippetEntry},
};
use crate::error::LychiError;
use crate::snippets::SnippetItem;

pub const MAX_SNIPPETS: usize = 50;
pub const MAX_SNIPPET_NAME: usize = 40;
pub const MAX_SNIPPET_BODY: usize = 5000;

#[derive(Default)]
pub struct SnippetsStore;

impl SnippetsStore {
    pub fn new() -> Self {
        Self
    }

    pub fn get_snippets(&self, db: &Arc<Database>) -> Result<Vec<SnippetItem>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::SNIPPETS)?;
        let mut snippets = Vec::new();
        for result in table.iter()? {
            let (key, val) = result?;
            // One unreadable row must not hide the rest of the list.
            let Some(entry) = db::decode_row::<SnippetEntry>("snippets", key.value(), val.value())
            else {
                continue;
            };
            if entry.deleted_at.is_none() {
                snippets.push(SnippetItem {
                    id: key.value().to_string(),
                    name: entry.name,
                    body: entry.body,
                    created_at: entry.created_at,
                    updated_at: entry.updated_at,
                });
            }
        }
        Ok(snippets)
    }

    pub fn get_snippet_by_name(
        &self,
        db: &Arc<Database>,
        name: &str,
    ) -> Result<Option<SnippetItem>, LychiError> {
        let lower = name.to_lowercase();
        let snippets = self.get_snippets(db)?;
        Ok(snippets
            .into_iter()
            .find(|s| s.name.to_lowercase() == lower))
    }

    pub fn add_snippet(
        &self,
        db: &Arc<Database>,
        name: &str,
        body: &str,
    ) -> Result<SnippetItem, LychiError> {
        let name = name.trim().to_string();
        let body = body.trim().to_string();

        if name.is_empty() {
            return Err(LychiError::Snippet("Snippet name cannot be empty".into()));
        }
        if name.len() > MAX_SNIPPET_NAME {
            return Err(LychiError::Snippet(format!(
                "Snippet name exceeds {MAX_SNIPPET_NAME} character limit"
            )));
        }
        if body.is_empty() {
            return Err(LychiError::Snippet("Snippet body cannot be empty".into()));
        }
        if body.len() > MAX_SNIPPET_BODY {
            return Err(LychiError::Snippet(format!(
                "Snippet body exceeds {MAX_SNIPPET_BODY} character limit"
            )));
        }

        // Check for duplicate name
        if self.get_snippet_by_name(db, &name)?.is_some() {
            return Err(LychiError::Snippet(format!(
                "Snippet already exists: {name}"
            )));
        }

        // Check count limit
        let current_count = self.snippets_count(db)?;
        if current_count >= MAX_SNIPPETS {
            return Err(LychiError::Snippet(format!(
                "Snippet limit reached ({MAX_SNIPPETS}/{MAX_SNIPPETS}). Delete a snippet to make room."
            )));
        }

        let now = db::now_millis();
        let id = db::new_id();
        let entry = SnippetEntry {
            name: name.clone(),
            body: body.clone(),
            created_at: now,
            updated_at: now,
            deleted_at: None,
            sync_status: SYNC_LOCAL,
        };

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::SNIPPETS)?;
            let bytes = crate::db::encode_row(&entry)?;
            table.insert(id.as_str(), bytes.as_slice())?;
        }
        txn.commit()?;

        Ok(SnippetItem {
            id,
            name,
            body,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn update_snippet(
        &self,
        db: &Arc<Database>,
        id: &str,
        name: &str,
        body: &str,
    ) -> Result<(), LychiError> {
        let name = name.trim();
        let body = body.trim();

        if name.is_empty() {
            return Err(LychiError::Snippet("Snippet name cannot be empty".into()));
        }
        if name.len() > MAX_SNIPPET_NAME {
            return Err(LychiError::Snippet(format!(
                "Snippet name exceeds {MAX_SNIPPET_NAME} character limit"
            )));
        }
        if body.is_empty() {
            return Err(LychiError::Snippet("Snippet body cannot be empty".into()));
        }
        if body.len() > MAX_SNIPPET_BODY {
            return Err(LychiError::Snippet(format!(
                "Snippet body exceeds {MAX_SNIPPET_BODY} character limit"
            )));
        }

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::SNIPPETS)?;
            let existing_val = table
                .get(id)?
                .ok_or_else(|| LychiError::Snippet(format!("Snippet not found: {id}")))?;
            let mut entry: SnippetEntry = crate::db::decode_value(existing_val.value())?;
            if entry.deleted_at.is_some() {
                return Err(LychiError::Snippet(format!("Snippet not found: {id}")));
            }
            entry.name = name.to_string();
            entry.body = body.to_string();
            entry.updated_at = db::now_millis();
            let bytes = crate::db::encode_row(&entry)?;
            drop(existing_val);
            table.insert(id, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn delete_snippet(&self, db: &Arc<Database>, id: &str) -> Result<(), LychiError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::SNIPPETS)?;
            let existing_val = table
                .get(id)?
                .ok_or_else(|| LychiError::Snippet(format!("Snippet not found: {id}")))?;
            let mut entry: SnippetEntry = crate::db::decode_value(existing_val.value())?;
            if entry.deleted_at.is_some() {
                return Err(LychiError::Snippet(format!("Snippet not found: {id}")));
            }
            entry.deleted_at = Some(db::now_millis());
            let bytes = crate::db::encode_row(&entry)?;
            drop(existing_val);
            table.insert(id, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn snippets_count(&self, db: &Arc<Database>) -> Result<usize, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::SNIPPETS)?;
        let mut count = 0;
        for result in table.iter()? {
            let (_, val) = result?;
            // One unreadable row must not hide the rest of the list.
            let Some(entry) = db::decode_row::<SnippetEntry>("snippets", "?", val.value()) else {
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
    fn snippet_add_and_list() {
        let db = open_test_database();
        let store = SnippetsStore::new();
        assert!(store.get_snippets(&db).unwrap().is_empty());

        let item = store.add_snippet(&db, "greeting", "Hello, world!").unwrap();
        let snippets = store.get_snippets(&db).unwrap();
        assert_eq!(snippets.len(), 1);
        assert_eq!(snippets[0].name, "greeting");
        assert_eq!(snippets[0].body, "Hello, world!");
        assert!(!item.id.is_empty());
    }

    #[test]
    fn snippet_duplicate_name_rejected() {
        let db = open_test_database();
        let store = SnippetsStore::new();
        store.add_snippet(&db, "test", "body1").unwrap();
        let err = store.add_snippet(&db, "test", "body2").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn snippet_name_limit() {
        let db = open_test_database();
        let store = SnippetsStore::new();
        let long_name = "x".repeat(41);
        assert!(store.add_snippet(&db, &long_name, "body").is_err());
        let exact = "x".repeat(40);
        assert!(store.add_snippet(&db, &exact, "body").is_ok());
    }

    #[test]
    fn snippet_body_limit() {
        let db = open_test_database();
        let store = SnippetsStore::new();
        let long_body = "x".repeat(5001);
        assert!(store.add_snippet(&db, "big", &long_body).is_err());
        let exact = "x".repeat(5000);
        assert!(store.add_snippet(&db, "big", &exact).is_ok());
    }

    #[test]
    fn snippet_update() {
        let db = open_test_database();
        let store = SnippetsStore::new();
        let item = store.add_snippet(&db, "old", "old body").unwrap();
        store
            .update_snippet(&db, &item.id, "new", "new body")
            .unwrap();
        let snippets = store.get_snippets(&db).unwrap();
        assert_eq!(snippets[0].name, "new");
        assert_eq!(snippets[0].body, "new body");
    }

    #[test]
    fn snippet_delete() {
        let db = open_test_database();
        let store = SnippetsStore::new();
        let item = store.add_snippet(&db, "temp", "temporary").unwrap();
        assert_eq!(store.snippets_count(&db).unwrap(), 1);
        store.delete_snippet(&db, &item.id).unwrap();
        assert_eq!(store.snippets_count(&db).unwrap(), 0);
    }

    #[test]
    fn snippet_get_by_name() {
        let db = open_test_database();
        let store = SnippetsStore::new();
        store
            .add_snippet(&db, "email-intro", "Hello there")
            .unwrap();
        let found = store.get_snippet_by_name(&db, "email-intro").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().body, "Hello there");
        // Case-insensitive
        let found = store.get_snippet_by_name(&db, "EMAIL-INTRO").unwrap();
        assert!(found.is_some());
    }
}
