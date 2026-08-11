use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable};

use crate::ai_history::{Conversation, ConversationSummary, derive_title};
use crate::db;
use crate::error::LychiError;
use crate::providers::{ChatMessage, Role};

/// Retention: keep conversations from the last N days; anything older is pruned
/// on upsert so the DB doesn't grow unbounded (cf. the DB-growth item in
/// release-readiness).
pub const RETENTION_DAYS: u64 = 30;
const RETENTION_MS: u64 = RETENTION_DAYS * 24 * 60 * 60 * 1000;

/// Hard ceiling as a backstop, so a single very busy period can't blow up the DB
/// even within the retention window. Time is the primary rule; this only bites in
/// the extreme.
pub const MAX_CONVERSATIONS: usize = 500;

#[derive(Default)]
pub struct AiHistoryStore;

impl AiHistoryStore {
    pub fn new() -> Self {
        Self
    }

    /// List conversation summaries, newest first (by `updated_at`). Genuinely
    /// cheap now: deserializes a `ConversationMeta` that SKIPS the `messages`
    /// array, so listing after every agent turn never parses message bodies or
    /// inline base64 images. An undecodable row is skipped-and-warned (key only,
    /// never the value) rather than failing the whole list — the same
    /// resilience `decode_row` gives the postcard stores, which this serde_json
    /// store had missed.
    pub fn list(&self, db: &Arc<Database>) -> Result<Vec<ConversationSummary>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::AI_CONVERSATIONS)?;
        let mut out = Vec::new();
        for result in table.iter()? {
            let (key, val) = result?;
            let meta: crate::ai_history::ConversationMeta =
                match crate::db::json_body_of(val.value()).and_then(|b| {
                    serde_json::from_slice(b).map_err(|e| LychiError::Database(e.to_string()))
                }) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(
                            "[history] skipping row `{}`: {e} — the rest of the list is unaffected",
                            key.value()
                        );
                        continue;
                    }
                };
            out.push(ConversationSummary {
                id: meta.id,
                title: meta.title,
                turn_count: meta.turn_count,
                created_at: meta.created_at,
                updated_at: meta.updated_at,
            });
        }
        out.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
        Ok(out)
    }

    /// Fetch a full conversation (with all messages) by id.
    pub fn get(&self, db: &Arc<Database>, id: &str) -> Result<Option<Conversation>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::AI_CONVERSATIONS)?;
        match table.get(id)? {
            Some(val) => {
                let conv: Conversation =
                    serde_json::from_slice(crate::db::json_body_of(val.value())?)
                        .map_err(|e| LychiError::Database(e.to_string()))?;
                Ok(Some(conv))
            }
            None => Ok(None),
        }
    }

    /// Upsert a conversation by id: insert a new one or update an existing one's
    /// messages + `updated_at` (a follow-up extends the same thread). Returns the
    /// stored `Conversation`. Prunes to `MAX_CONVERSATIONS` afterwards.
    ///
    /// A conversation with no real turns (only a system prompt, or empty) is NOT
    /// saved — nothing worth recalling. Returns `None` in that case.
    pub fn upsert(
        &self,
        db: &Arc<Database>,
        id: &str,
        messages: &[ChatMessage],
    ) -> Result<Option<Conversation>, LychiError> {
        if count_turns(messages) == 0 {
            return Ok(None);
        }

        let now = db::now_millis();
        // Preserve the original created_at if this id already exists — but a
        // corrupt/unreadable existing row must NOT block writing the new turn.
        // Read-then-write here used `?`, so a single stranded row (a bogus
        // pre-envelope tag, seen in the field) failed the whole persist and the
        // conversation was silently lost ("[history] failed to persist"). The
        // created_at is a nicety, not a reason to drop the transcript: on an
        // unreadable prior row, fall back to `now` and overwrite it clean.
        let created_at = self
            .get(db, id)
            .ok()
            .flatten()
            .map(|c| c.created_at)
            .unwrap_or(now);

        let conv = Conversation {
            id: id.to_string(),
            title: derive_title(messages),
            turn_count: count_turns(messages),
            messages: messages.to_vec(),
            created_at,
            updated_at: now,
        };

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::AI_CONVERSATIONS)?;
            let bytes = crate::db::wrap_body(
                &serde_json::to_vec(&conv).map_err(|e| LychiError::Database(e.to_string()))?,
            );
            table.insert(id, bytes.as_slice())?;
        }
        txn.commit()?;

        self.prune(db)?;
        Ok(Some(conv))
    }

    pub fn delete(&self, db: &Arc<Database>, id: &str) -> Result<(), LychiError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::AI_CONVERSATIONS)?;
            table.remove(id)?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn clear(&self, db: &Arc<Database>) -> Result<(), LychiError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::AI_CONVERSATIONS)?;
            let ids: Vec<String> = table
                .iter()?
                .filter_map(|r| r.ok().map(|(k, _)| k.value().to_string()))
                .collect();
            for id in ids {
                table.remove(id.as_str())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Prune conversations older than the retention window (by `updated_at`),
    /// plus a hard-count backstop. Time is the primary rule.
    fn prune(&self, db: &Arc<Database>) -> Result<(), LychiError> {
        let summaries = self.list(db)?; // already newest-first
        let now = db::now_millis();
        let cutoff = now.saturating_sub(RETENTION_MS);

        let to_remove: Vec<String> = summaries
            .into_iter()
            .enumerate()
            .filter(|(idx, s)| {
                // Older than the retention window, OR beyond the hard ceiling.
                s.updated_at < cutoff || *idx >= MAX_CONVERSATIONS
            })
            .map(|(_, s)| s.id)
            .collect();

        if to_remove.is_empty() {
            return Ok(());
        }
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::AI_CONVERSATIONS)?;
            for id in to_remove {
                table.remove(id.as_str())?;
            }
        }
        txn.commit()?;
        Ok(())
    }
}

/// Count user + assistant turns (the "real" conversation length), ignoring the
/// system prompt and tool-result messages.
fn count_turns(messages: &[ChatMessage]) -> u32 {
    messages
        .iter()
        .filter(|m| matches!(m.role, Role::User | Role::Assistant))
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_test_database;

    fn conv_messages(user: &str, assistant: &str) -> Vec<ChatMessage> {
        vec![
            ChatMessage::system("sys"),
            ChatMessage::user(user),
            ChatMessage::assistant(assistant),
        ]
    }

    #[test]
    fn upsert_and_list_and_get() {
        let db = open_test_database();
        let store = AiHistoryStore::new();
        assert!(store.list(&db).unwrap().is_empty());

        let msgs = conv_messages("what is rust?", "A systems language.");
        let saved = store.upsert(&db, "c1", &msgs).unwrap().unwrap();
        assert_eq!(saved.title, "what is rust?");

        let list = store.list(&db).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].turn_count, 2); // user + assistant

        let got = store.get(&db, "c1").unwrap().unwrap();
        assert_eq!(got.messages.len(), 3);
    }

    #[test]
    fn upsert_same_id_updates_not_duplicates() {
        let db = open_test_database();
        let store = AiHistoryStore::new();
        store.upsert(&db, "c1", &conv_messages("q1", "a1")).unwrap();
        let mut extended = conv_messages("q1", "a1");
        extended.push(ChatMessage::user("follow up"));
        extended.push(ChatMessage::assistant("sure"));
        store.upsert(&db, "c1", &extended).unwrap();
        let list = store.list(&db).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].turn_count, 4);
    }

    #[test]
    fn empty_conversation_not_saved() {
        let db = open_test_database();
        let store = AiHistoryStore::new();
        // Only a system prompt → no real turns → not saved.
        let res = store
            .upsert(&db, "c1", &[ChatMessage::system("sys")])
            .unwrap();
        assert!(res.is_none());
        assert!(store.list(&db).unwrap().is_empty());
    }

    #[test]
    fn delete_and_clear() {
        let db = open_test_database();
        let store = AiHistoryStore::new();
        store.upsert(&db, "c1", &conv_messages("q", "a")).unwrap();
        store.upsert(&db, "c2", &conv_messages("q2", "a2")).unwrap();
        store.delete(&db, "c1").unwrap();
        assert_eq!(store.list(&db).unwrap().len(), 1);
        store.clear(&db).unwrap();
        assert!(store.list(&db).unwrap().is_empty());
    }

    #[test]
    fn prunes_conversations_older_than_retention() {
        let db = open_test_database();
        let store = AiHistoryStore::new();
        // Save a recent one, then hand-write an ancient one directly into the
        // table (updated_at older than the retention window). A subsequent upsert
        // triggers prune, which should drop the ancient one.
        store
            .upsert(&db, "recent", &conv_messages("q", "a"))
            .unwrap();

        let ancient = Conversation {
            id: "ancient".into(),
            title: "old".into(),
            turn_count: 2,
            messages: conv_messages("old q", "old a"),
            created_at: 0,
            updated_at: 0, // epoch — far older than 30 days ago
        };
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(db::AI_CONVERSATIONS).unwrap();
            let bytes = crate::db::wrap_body(&serde_json::to_vec(&ancient).unwrap());
            table.insert("ancient", bytes.as_slice()).unwrap();
        }
        txn.commit().unwrap();
        assert_eq!(store.list(&db).unwrap().len(), 2);

        // Any upsert runs prune → the ancient one is dropped.
        store
            .upsert(&db, "recent2", &conv_messages("q2", "a2"))
            .unwrap();
        let ids: Vec<String> = store.list(&db).unwrap().into_iter().map(|s| s.id).collect();
        assert!(!ids.contains(&"ancient".to_string()));
        assert!(ids.contains(&"recent".to_string()));
    }

    /// The field bug ("[history] failed to persist conversation: schema v123"):
    /// a corrupt/undecodable existing row for a conversation id must NOT block
    /// writing the new turn. Before the fix, `upsert` read the old row with `?`
    /// to preserve created_at, so one stranded row silently dropped the whole
    /// conversation. Now the read failure falls back to a fresh created_at.
    #[test]
    fn a_corrupt_existing_row_does_not_block_the_upsert() {
        let db = open_test_database();
        let store = AiHistoryStore::new();

        // Plant a garbage row under "c1" — a bogus leading tag that neither the
        // envelope nor the JSON-shape fallback can decode.
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(db::AI_CONVERSATIONS).unwrap();
            table
                .insert("c1", [0xDE, 0xAD, 0xBE, 0xEF].as_slice())
                .unwrap();
        }
        txn.commit().unwrap();
        assert!(store.get(&db, "c1").is_err(), "the planted row is corrupt");

        // The upsert must still succeed, overwriting the corrupt row.
        let saved = store
            .upsert(&db, "c1", &conv_messages("recover me", "done"))
            .unwrap()
            .expect("a real conversation is saved");
        assert_eq!(saved.title, "recover me");
        // And it's now readable.
        let got = store.get(&db, "c1").unwrap().unwrap();
        assert_eq!(got.messages.len(), 3);
    }

    /// `list()` must skip an undecodable row, not fail the whole list — the C4
    /// resilience the postcard stores got but this serde_json store had missed.
    /// This also unblocks `upsert` (which calls `prune → list`).
    #[test]
    fn list_skips_a_corrupt_row_and_keeps_the_good_ones() {
        let db = open_test_database();
        let store = AiHistoryStore::new();
        store.upsert(&db, "good", &conv_messages("q", "a")).unwrap();

        // Plant garbage under a second id.
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(db::AI_CONVERSATIONS).unwrap();
            table.insert("bad", [1u8, 2, 3].as_slice()).unwrap();
        }
        txn.commit().unwrap();

        let list = store.list(&db).unwrap();
        let ids: Vec<&str> = list.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["good"], "corrupt row skipped, good row kept");

        // And a subsequent upsert (which prunes via list) still succeeds.
        store
            .upsert(&db, "good2", &conv_messages("q2", "a2"))
            .unwrap();
        assert_eq!(store.list(&db).unwrap().len(), 2);
    }

    /// The listing summary reads `turn_count` from the stored field WITHOUT
    /// deserializing message bodies — a row written before the field existed
    /// (no `turn_count` key) decodes to 0, then self-corrects on next upsert.
    #[test]
    fn turn_count_survives_a_pre_field_row() {
        let db = open_test_database();
        let store = AiHistoryStore::new();

        // A legacy JSON body with NO turn_count key.
        let legacy = serde_json::json!({
            "id": "c1", "title": "legacy",
            "messages": [
                {"role":"user","content":[{"type":"text","text":"hi"}]},
                {"role":"assistant","content":[{"type":"text","text":"hello"}]}
            ],
            "created_at": 100, "updated_at": 100
        });
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(db::AI_CONVERSATIONS).unwrap();
            let bytes = crate::db::wrap_body(&serde_json::to_vec(&legacy).unwrap());
            table.insert("c1", bytes.as_slice()).unwrap();
        }
        txn.commit().unwrap();

        // Pre-field row lists with turn_count 0 (no bodies parsed), but is still
        // fully readable via get().
        assert_eq!(store.list(&db).unwrap()[0].turn_count, 0);
        assert_eq!(store.get(&db, "c1").unwrap().unwrap().messages.len(), 2);

        // Re-upserting recomputes and stores the count.
        store
            .upsert(&db, "c1", &store.get(&db, "c1").unwrap().unwrap().messages)
            .unwrap();
        assert_eq!(store.list(&db).unwrap()[0].turn_count, 2);
    }

    #[test]
    fn title_truncates_long_first_line() {
        let db = open_test_database();
        let store = AiHistoryStore::new();
        let long = "x".repeat(100);
        let saved = store
            .upsert(&db, "c1", &conv_messages(&long, "ok"))
            .unwrap()
            .unwrap();
        assert!(saved.title.ends_with('…'));
        assert!(saved.title.chars().count() <= 61);
    }
}
