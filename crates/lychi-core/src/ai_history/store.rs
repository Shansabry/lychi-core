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

    /// List conversation summaries, newest first (by `updated_at`). Cheap — reads
    /// each entry but returns only metadata + a turn count, not message bodies.
    pub fn list(&self, db: &Arc<Database>) -> Result<Vec<ConversationSummary>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::AI_CONVERSATIONS)?;
        let mut out = Vec::new();
        for result in table.iter()? {
            let (_, val) = result?;
            let conv: Conversation = serde_json::from_slice(val.value())
                .map_err(|e| LychiError::Database(e.to_string()))?;
            out.push(ConversationSummary {
                id: conv.id,
                title: conv.title,
                turn_count: count_turns(&conv.messages),
                created_at: conv.created_at,
                updated_at: conv.updated_at,
            });
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    /// Fetch a full conversation (with all messages) by id.
    pub fn get(&self, db: &Arc<Database>, id: &str) -> Result<Option<Conversation>, LychiError> {
        let txn = db.begin_read()?;
        let table = txn.open_table(db::AI_CONVERSATIONS)?;
        match table.get(id)? {
            Some(val) => {
                let conv: Conversation = serde_json::from_slice(val.value())
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
        // Preserve the original created_at if this id already exists.
        let created_at = self.get(db, id)?.map(|c| c.created_at).unwrap_or(now);

        let conv = Conversation {
            id: id.to_string(),
            title: derive_title(messages),
            messages: messages.to_vec(),
            created_at,
            updated_at: now,
        };

        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(db::AI_CONVERSATIONS)?;
            let bytes =
                serde_json::to_vec(&conv).map_err(|e| LychiError::Database(e.to_string()))?;
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
            messages: conv_messages("old q", "old a"),
            created_at: 0,
            updated_at: 0, // epoch — far older than 30 days ago
        };
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(db::AI_CONVERSATIONS).unwrap();
            let bytes = serde_json::to_vec(&ancient).unwrap();
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
