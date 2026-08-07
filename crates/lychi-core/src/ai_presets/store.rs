use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable};

use crate::ai_presets::AiPresetItem;
use crate::db::{
    self,
    schema::{AiPresetEntry, SYNC_LOCAL},
};
use crate::error::LychiError;

pub const MAX_PRESETS: usize = 50;
pub const MAX_KEYWORD: usize = 24;
pub const MAX_NAME: usize = 40;
pub const MAX_TEMPLATE: usize = 5000;

/// The built-in presets seeded on first run — the re-homed AI text transforms.
/// Each is `(keyword, name, template)`. They're normal editable presets, just
/// installed by default so the muscle-memory verbs keep working.
pub const BUILTIN_PRESETS: &[(&str, &str, &str)] = &[
    (
        "translate",
        "Translate",
        "Translate the following text to English (or, if it is already English, to Spanish). Reply with only the translation, no commentary:\n\n{input}",
    ),
    (
        "summarize",
        "Summarize",
        "Summarize the following text in 2-3 concise sentences:\n\n{input}",
    ),
    (
        "rewrite",
        "Rewrite",
        "Rewrite the following text to be clear, concise, and grammatically correct. Preserve the meaning and reply with only the rewritten text:\n\n{input}",
    ),
];

#[derive(Default)]
pub struct AiPresetsStore;

impl AiPresetsStore {
    pub fn new() -> Self {
        Self
    }

    pub fn get_presets(&self, db: &Arc<Database>) -> Result<Vec<AiPresetItem>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::AI_PRESETS)?;
        let mut presets = Vec::new();
        for result in table.iter()? {
            let (key, val) = result?;
            // One unreadable row must not hide the rest of the list.
            let Some(entry) =
                db::decode_row::<AiPresetEntry>("ai_presets", key.value(), val.value())
            else {
                continue;
            };
            if entry.deleted_at.is_none() {
                presets.push(AiPresetItem {
                    id: key.value().to_string(),
                    is_builtin: is_builtin_keyword(&entry.keyword),
                    keyword: entry.keyword,
                    name: entry.name,
                    template: entry.template,
                    created_at: entry.created_at,
                    updated_at: entry.updated_at,
                });
            }
        }
        // Stable order for the UI: by keyword.
        presets.sort_by(|a, b| a.keyword.cmp(&b.keyword));
        Ok(presets)
    }

    /// Look up a preset by its invocation keyword (case-insensitive).
    pub fn get_preset_by_keyword(
        &self,
        db: &Arc<Database>,
        keyword: &str,
    ) -> Result<Option<AiPresetItem>, LychiError> {
        let lower = keyword.trim().to_lowercase();
        Ok(self
            .get_presets(db)?
            .into_iter()
            .find(|p| p.keyword.to_lowercase() == lower))
    }

    pub fn add_preset(
        &self,
        db: &Arc<Database>,
        keyword: &str,
        name: &str,
        template: &str,
    ) -> Result<AiPresetItem, LychiError> {
        let keyword = normalize_keyword(keyword)?;
        let name = name.trim().to_string();
        let template = template.trim().to_string();

        if name.is_empty() {
            return Err(LychiError::AiPreset("Preset name cannot be empty".into()));
        }
        if name.len() > MAX_NAME {
            return Err(LychiError::AiPreset(format!(
                "Preset name exceeds {MAX_NAME} character limit"
            )));
        }
        if template.is_empty() {
            return Err(LychiError::AiPreset(
                "Preset template cannot be empty".into(),
            ));
        }
        if template.len() > MAX_TEMPLATE {
            return Err(LychiError::AiPreset(format!(
                "Preset template exceeds {MAX_TEMPLATE} character limit"
            )));
        }
        if self.get_preset_by_keyword(db, &keyword)?.is_some() {
            return Err(LychiError::AiPreset(format!(
                "A preset with keyword '{keyword}' already exists"
            )));
        }
        if self.presets_count(db)? >= MAX_PRESETS {
            return Err(LychiError::AiPreset(format!(
                "Preset limit reached ({MAX_PRESETS}/{MAX_PRESETS}). Delete one to make room."
            )));
        }

        let now = db::now_millis();
        let id = db::new_id();
        let entry = AiPresetEntry {
            keyword: keyword.clone(),
            name: name.clone(),
            template: template.clone(),
            created_at: now,
            updated_at: now,
            deleted_at: None,
            sync_status: SYNC_LOCAL,
        };

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::AI_PRESETS)?;
            let bytes =
                postcard::to_allocvec(&entry).map_err(|e| LychiError::Database(e.to_string()))?;
            table.insert(id.as_str(), bytes.as_slice())?;
        }
        txn.commit()?;

        Ok(AiPresetItem {
            id,
            is_builtin: is_builtin_keyword(&keyword),
            keyword,
            name,
            template,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn update_preset(
        &self,
        db: &Arc<Database>,
        id: &str,
        keyword: &str,
        name: &str,
        template: &str,
    ) -> Result<(), LychiError> {
        let keyword = normalize_keyword(keyword)?;
        let name = name.trim();
        let template = template.trim();

        if name.is_empty() {
            return Err(LychiError::AiPreset("Preset name cannot be empty".into()));
        }
        if name.len() > MAX_NAME {
            return Err(LychiError::AiPreset(format!(
                "Preset name exceeds {MAX_NAME} character limit"
            )));
        }
        if template.is_empty() {
            return Err(LychiError::AiPreset(
                "Preset template cannot be empty".into(),
            ));
        }
        if template.len() > MAX_TEMPLATE {
            return Err(LychiError::AiPreset(format!(
                "Preset template exceeds {MAX_TEMPLATE} character limit"
            )));
        }
        // Keyword must stay unique (ignoring this same preset).
        if let Some(other) = self.get_preset_by_keyword(db, &keyword)?
            && other.id != id
        {
            return Err(LychiError::AiPreset(format!(
                "A preset with keyword '{keyword}' already exists"
            )));
        }

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::AI_PRESETS)?;
            let existing_val = table
                .get(id)?
                .ok_or_else(|| LychiError::AiPreset(format!("Preset not found: {id}")))?;
            let mut entry: AiPresetEntry = postcard::from_bytes(existing_val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if entry.deleted_at.is_some() {
                return Err(LychiError::AiPreset(format!("Preset not found: {id}")));
            }
            // Checked against the STORED keyword, not the submitted one: the
            // question is "is the thing being edited a default", and reading
            // the new keyword would let a rename slip a built-in through.
            reject_if_builtin(&entry.keyword)?;
            entry.keyword = keyword;
            entry.name = name.to_string();
            entry.template = template.to_string();
            entry.updated_at = db::now_millis();
            let bytes =
                postcard::to_allocvec(&entry).map_err(|e| LychiError::Database(e.to_string()))?;
            drop(existing_val);
            table.insert(id, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn delete_preset(&self, db: &Arc<Database>, id: &str) -> Result<(), LychiError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::AI_PRESETS)?;
            let existing_val = table
                .get(id)?
                .ok_or_else(|| LychiError::AiPreset(format!("Preset not found: {id}")))?;
            let mut entry: AiPresetEntry = postcard::from_bytes(existing_val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            if entry.deleted_at.is_some() {
                return Err(LychiError::AiPreset(format!("Preset not found: {id}")));
            }
            reject_if_builtin(&entry.keyword)?;
            entry.deleted_at = Some(db::now_millis());
            let bytes =
                postcard::to_allocvec(&entry).map_err(|e| LychiError::Database(e.to_string()))?;
            drop(existing_val);
            table.insert(id, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn presets_count(&self, db: &Arc<Database>) -> Result<usize, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::AI_PRESETS)?;
        let mut count = 0;
        for result in table.iter()? {
            let (_, val) = result?;
            // One unreadable row must not hide the rest of the list.
            let Some(entry) = db::decode_row::<AiPresetEntry>("ai_presets", "?", val.value())
            else {
                continue;
            };
            if entry.deleted_at.is_none() {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Seed the built-in presets on first run. Idempotent: only installs a
    /// builtin whose keyword isn't already present (so it never clobbers a user's
    /// edits or re-creates one they deleted-then-... actually a deleted builtin
    /// WOULD reappear; that's acceptable for defaults and matches "reset to
    /// defaults" expectations). Called once at startup.
    pub fn seed_builtins(&self, db: &Arc<Database>) -> Result<(), LychiError> {
        for &(keyword, name, template) in BUILTIN_PRESETS {
            if self.get_preset_by_keyword(db, keyword)?.is_none() {
                // Ignore individual failures (e.g. limit reached) so one bad seed
                // doesn't block the rest.
                let _ = self.add_preset(db, keyword, name, template);
            }
        }
        Ok(())
    }
}

/// Is this keyword one of the shipped defaults?
///
/// Derived from `BUILTIN_PRESETS` rather than a second list — adding a default
/// there makes it protected here with no further edit, and the two can never
/// disagree about which presets are built in.
pub fn is_builtin_keyword(keyword: &str) -> bool {
    let k = keyword.trim().to_lowercase();
    BUILTIN_PRESETS.iter().any(|(kw, _, _)| *kw == k)
}

/// Refuse an edit or delete aimed at a shipped default.
///
/// Enforced HERE, not in the settings UI, because presets are also reachable
/// over IPC — hiding a button would make the protection cosmetic. The message
/// says what to do instead, since "you can't" without "here's how" is the
/// unhelpful half of a refusal.
fn reject_if_builtin(keyword: &str) -> Result<(), LychiError> {
    if is_builtin_keyword(keyword) {
        return Err(LychiError::AiPreset(format!(
            "\"{keyword}\" is a built-in command and cannot be changed or removed. \
             Add your own command with a different keyword to customise this."
        )));
    }
    Ok(())
}

/// Normalize + validate an invocation keyword: trimmed, lowercased, a single
/// token (no spaces), within the length cap. Keywords are how presets are
/// invoked, so they must be typeable as a first word.
fn normalize_keyword(keyword: &str) -> Result<String, LychiError> {
    let k = keyword.trim().to_lowercase();
    if k.is_empty() {
        return Err(LychiError::AiPreset(
            "Preset keyword cannot be empty".into(),
        ));
    }
    if k.split_whitespace().count() != 1 {
        return Err(LychiError::AiPreset(
            "Preset keyword must be a single word (no spaces)".into(),
        ));
    }
    if k.len() > MAX_KEYWORD {
        return Err(LychiError::AiPreset(format!(
            "Preset keyword exceeds {MAX_KEYWORD} character limit"
        )));
    }
    Ok(k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_test_database;

    #[test]
    fn add_and_list() {
        let db = open_test_database();
        let store = AiPresetsStore::new();
        assert!(store.get_presets(&db).unwrap().is_empty());
        let item = store
            .add_preset(&db, "tr", "Translate", "Translate: {input}")
            .unwrap();
        let presets = store.get_presets(&db).unwrap();
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].keyword, "tr");
        assert!(!item.id.is_empty());
    }

    #[test]
    fn keyword_normalized_lowercase() {
        let db = open_test_database();
        let store = AiPresetsStore::new();
        store
            .add_preset(&db, "  TRans  ", "T", "x {input}")
            .unwrap();
        assert!(store.get_preset_by_keyword(&db, "trans").unwrap().is_some());
        // Duplicate (case-insensitive) rejected.
        let err = store.add_preset(&db, "TRANS", "T2", "y").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn keyword_must_be_single_word() {
        let db = open_test_database();
        let store = AiPresetsStore::new();
        assert!(store.add_preset(&db, "two words", "T", "x").is_err());
    }

    #[test]
    fn render_substitutes_placeholder() {
        let item = AiPresetItem {
            id: "1".into(),
            is_builtin: false,
            keyword: "tr".into(),
            name: "T".into(),
            template: "Translate to French: {input}".into(),
            created_at: 0,
            updated_at: 0,
        };
        assert_eq!(item.render("hello"), "Translate to French: hello");
    }

    #[test]
    fn render_appends_when_no_placeholder() {
        let item = AiPresetItem {
            id: "1".into(),
            is_builtin: false,
            keyword: "sum".into(),
            name: "S".into(),
            template: "Summarize this:".into(),
            created_at: 0,
            updated_at: 0,
        };
        assert_eq!(item.render("long text"), "Summarize this:\n\nlong text");
    }

    #[test]
    fn update_and_delete() {
        let db = open_test_database();
        let store = AiPresetsStore::new();
        let item = store.add_preset(&db, "a", "A", "old {input}").unwrap();
        store
            .update_preset(&db, &item.id, "b", "B", "new {input}")
            .unwrap();
        let p = store.get_preset_by_keyword(&db, "b").unwrap().unwrap();
        assert_eq!(p.template, "new {input}");
        store.delete_preset(&db, &item.id).unwrap();
        assert_eq!(store.presets_count(&db).unwrap(), 0);
    }

    #[test]
    fn seed_builtins_is_idempotent() {
        let db = open_test_database();
        let store = AiPresetsStore::new();
        store.seed_builtins(&db).unwrap();
        let n = store.presets_count(&db).unwrap();
        assert_eq!(n, BUILTIN_PRESETS.len());
        // Re-seeding doesn't duplicate.
        store.seed_builtins(&db).unwrap();
        assert_eq!(store.presets_count(&db).unwrap(), n);
    }

    #[test]
    fn update_keyword_collision_rejected() {
        let db = open_test_database();
        let store = AiPresetsStore::new();
        store.add_preset(&db, "a", "A", "x").unwrap();
        let b = store.add_preset(&db, "b", "B", "y").unwrap();
        // Renaming b's keyword to "a" collides.
        let err = store.update_preset(&db, &b.id, "a", "B", "y").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }
}

#[cfg(test)]
mod builtin_protection {
    use super::*;
    use crate::db::open_test_database;

    fn seeded() -> (Arc<Database>, AiPresetsStore) {
        let db = open_test_database();
        let store = AiPresetsStore::new();
        store.seed_builtins(&db).unwrap();
        (db, store)
    }

    fn preset(store: &AiPresetsStore, db: &Arc<Database>, keyword: &str) -> AiPresetItem {
        store
            .get_preset_by_keyword(db, keyword)
            .unwrap()
            .unwrap_or_else(|| panic!("{keyword} should have been seeded"))
    }

    #[test]
    fn the_shipped_defaults_are_marked_builtin() {
        let (db, store) = seeded();
        for (keyword, _, _) in BUILTIN_PRESETS {
            assert!(
                preset(&store, &db, keyword).is_builtin,
                "{keyword} should be flagged built-in"
            );
        }
    }

    #[test]
    fn a_user_preset_is_not_marked_builtin() {
        let (db, store) = seeded();
        let mine = store
            .add_preset(&db, "haiku", "Haiku", "Write a haiku about {input}")
            .unwrap();
        assert!(!mine.is_builtin);
    }

    #[test]
    fn a_builtin_cannot_be_deleted() {
        let (db, store) = seeded();
        let p = preset(&store, &db, "translate");
        let err = store.delete_preset(&db, &p.id).unwrap_err();
        assert!(
            format!("{err}").contains("built-in"),
            "the refusal should say why: {err}"
        );
        // And it is still there.
        assert!(
            store
                .get_preset_by_keyword(&db, "translate")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_builtin_cannot_be_edited() {
        let (db, store) = seeded();
        let p = preset(&store, &db, "summarize");
        let err = store
            .update_preset(&db, &p.id, "summarize", "Summarise", "Changed: {input}")
            .unwrap_err();
        assert!(format!("{err}").contains("built-in"), "{err}");
        assert_eq!(preset(&store, &db, "summarize").template, p.template);
    }

    /// The bypass worth closing: renaming a built-in to a free keyword would
    /// let an edit through if the guard read the SUBMITTED keyword instead of
    /// the stored one.
    #[test]
    fn a_builtin_cannot_be_renamed_out_of_protection() {
        let (db, store) = seeded();
        let p = preset(&store, &db, "rewrite");
        let err = store
            .update_preset(&db, &p.id, "myrewrite", "Mine", "Anything: {input}")
            .unwrap_err();
        assert!(format!("{err}").contains("built-in"), "{err}");
        assert!(
            store
                .get_preset_by_keyword(&db, "rewrite")
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .get_preset_by_keyword(&db, "myrewrite")
                .unwrap()
                .is_none()
        );
    }

    /// Protection must not leak onto the user's own presets.
    #[test]
    fn user_presets_stay_editable_and_deletable() {
        let (db, store) = seeded();
        let mine = store
            .add_preset(&db, "haiku", "Haiku", "Write a haiku about {input}")
            .unwrap();
        store
            .update_preset(&db, &mine.id, "haiku", "Haiku v2", "A haiku on {input}")
            .expect("a user preset must stay editable");
        store
            .delete_preset(&db, &mine.id)
            .expect("a user preset must stay deletable");
    }

    /// A user may still take a built-in's KEYWORD if they delete nothing —
    /// guard against the check accidentally blocking creation.
    #[test]
    fn the_builtin_check_reads_the_keyword_not_the_name() {
        assert!(is_builtin_keyword("translate"));
        assert!(is_builtin_keyword("  TRANSLATE  "), "trim + lowercase");
        assert!(!is_builtin_keyword("translated"));
        assert!(!is_builtin_keyword("Translate this"));
    }
}
