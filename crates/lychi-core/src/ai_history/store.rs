use std::path::PathBuf;

use crate::ai_history::{Conversation, ConversationSummary, derive_title};
use crate::db;
use crate::error::LychiError;
use crate::providers::{ChatMessage, Role};

/// Retention: keep conversations from the last N days; anything older is pruned
/// on upsert so history doesn't grow unbounded.
pub const RETENTION_DAYS: u64 = 90;
const RETENTION_MS: u64 = RETENTION_DAYS * 24 * 60 * 60 * 1000;

/// How many conversations to keep — the newest `MAX_CONVERSATIONS` survive, the
/// rest are pruned on upsert (`list` is newest-first, so this is "keep the most
/// recent"). The retention window still prunes *older* conversations even when
/// there are fewer than this, so both rules only ever remove — never keep more.
pub const MAX_CONVERSATIONS: usize = 200;

/// Persisted AI chat transcripts, one JSON file per conversation under
/// [`crate::paths::ai_history_dir`].
///
/// Chat history is large, append-y, and arguably device-local — not the
/// user-authored content the database is for — so it lives in files. A
/// conversation is a whole-object snapshot (`<id>.json`): an upsert rewrites its
/// one file atomically, a delete unlinks it (reclaiming disk immediately, which
/// the database never did), and retention is a directory sweep that unlinks the
/// oldest / stalest files. Listing reads each file's header WITHOUT the
/// `messages` array (see [`crate::ai_history::ConversationMeta`]), so it never
/// parses transcript bodies or inline base64 images.
pub struct AiHistoryStore {
    dir: PathBuf,
}

impl Default for AiHistoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AiHistoryStore {
    pub fn new() -> Self {
        Self {
            dir: crate::paths::ai_history_dir(),
        }
    }

    /// Store rooted at an explicit directory — for tests, so they never touch the
    /// real `ai_history/` dir or race each other.
    #[cfg(test)]
    fn with_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Path of one conversation's file. The id is a UUID v7 string (opaque, no
    /// path separators), so it maps straight to a filename.
    fn conv_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Enumerate conversation files (`*.json`) in the history dir. A missing dir
    /// yields an empty list, not an error.
    fn conv_files(&self) -> Vec<PathBuf> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                tracing::warn!("[history] cannot read {}: {e}", self.dir.display());
                return Vec::new();
            }
        };
        entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .collect()
    }

    /// List conversation summaries, newest first (by `updated_at`).
    ///
    /// Reads each file and deserializes a `ConversationMeta` that SKIPS the
    /// `messages` array, so listing after every agent turn never parses message
    /// bodies or inline base64 images. An undecodable file is skipped-and-warned
    /// (the rest of the list is unaffected) — the same resilience the database
    /// stores get from `decode_row`.
    pub fn list(&self) -> Result<Vec<ConversationSummary>, LychiError> {
        let mut out = Vec::new();
        for path in self.conv_files() {
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("[history] skipping {}: {e}", path.display());
                    continue;
                }
            };
            match serde_json::from_slice::<crate::ai_history::ConversationMeta>(&bytes) {
                Ok(meta) => out.push(ConversationSummary {
                    id: meta.id,
                    title: meta.title,
                    turn_count: meta.turn_count,
                    created_at: meta.created_at,
                    updated_at: meta.updated_at,
                }),
                Err(e) => tracing::warn!(
                    "[history] skipping {}: {e} — the rest of the list is unaffected",
                    path.display()
                ),
            }
        }
        out.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
        Ok(out)
    }

    /// Fetch a full conversation (with all messages) by id.
    pub fn get(&self, id: &str) -> Result<Option<Conversation>, LychiError> {
        crate::filestore::load_snapshot(&self.conv_path(id))
    }

    /// Upsert a conversation by id: insert a new one or update an existing one's
    /// messages + `updated_at` (a follow-up extends the same thread). Returns the
    /// stored `Conversation`. Prunes to policy afterwards.
    ///
    /// A conversation with no real turns (only a system prompt, or empty) is NOT
    /// saved — nothing worth recalling. Returns `None` in that case.
    pub fn upsert(
        &self,
        id: &str,
        messages: &[ChatMessage],
    ) -> Result<Option<Conversation>, LychiError> {
        if count_turns(messages) == 0 {
            return Ok(None);
        }

        let now = db::now_millis();
        // Preserve the original created_at if this id already exists — but a
        // corrupt/unreadable existing file must NOT block writing the new turn.
        // The created_at is a nicety, not a reason to drop the transcript: on an
        // unreadable prior file, fall back to `now` and overwrite it clean.
        let created_at = self
            .get(id)
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

        crate::filestore::snapshot(&self.conv_path(id), &conv)?;
        self.prune()?;
        Ok(Some(conv))
    }

    /// Delete one conversation. Unlinks its file, reclaiming the disk at once. A
    /// missing file is success.
    pub fn delete(&self, id: &str) -> Result<(), LychiError> {
        match std::fs::remove_file(self.conv_path(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete every conversation.
    pub fn clear(&self) -> Result<(), LychiError> {
        for path in self.conv_files() {
            if let Err(e) = std::fs::remove_file(&path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!("[history] failed to remove {}: {e}", path.display());
            }
        }
        Ok(())
    }

    /// Prune the history down to policy on every upsert. Two rules, either of
    /// which removes a conversation: it is beyond the newest `MAX_CONVERSATIONS`
    /// (the hard count cap — `list` is newest-first, so index >= the cap means
    /// "older than the ones we keep"), or it is older than the retention window.
    /// Neither rule can ever KEEP more than `MAX_CONVERSATIONS`.
    fn prune(&self) -> Result<(), LychiError> {
        let summaries = self.list()?; // already newest-first
        let now = db::now_millis();
        let cutoff = now.saturating_sub(RETENTION_MS);

        let to_remove: Vec<String> = summaries
            .into_iter()
            .enumerate()
            .filter(|(idx, s)| *idx >= MAX_CONVERSATIONS || s.updated_at < cutoff)
            .map(|(_, s)| s.id)
            .collect();

        for id in to_remove {
            let _ = self.delete(&id);
        }
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

/// Write a conversation file directly (test helper for planting rows / legacy
/// shapes). Not part of the public API.
#[cfg(test)]
fn write_conv_file(dir: &std::path::Path, id: &str, bytes: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(format!("{id}.json")), bytes).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A store rooted at a unique temp dir, isolated per test.
    fn temp_store() -> AiHistoryStore {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lychi_aihist_test_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        AiHistoryStore::with_dir(dir)
    }

    fn conv_messages(user: &str, assistant: &str) -> Vec<ChatMessage> {
        vec![
            ChatMessage::system("sys"),
            ChatMessage::user(user),
            ChatMessage::assistant(assistant),
        ]
    }

    #[test]
    fn upsert_and_list_and_get() {
        let store = temp_store();
        assert!(store.list().unwrap().is_empty());

        let msgs = conv_messages("what is rust?", "A systems language.");
        let saved = store.upsert("c1", &msgs).unwrap().unwrap();
        assert_eq!(saved.title, "what is rust?");

        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].turn_count, 2); // user + assistant

        let got = store.get("c1").unwrap().unwrap();
        assert_eq!(got.messages.len(), 3);
        store.clear().unwrap();
    }

    #[test]
    fn upsert_same_id_updates_not_duplicates() {
        let store = temp_store();
        store.upsert("c1", &conv_messages("q1", "a1")).unwrap();
        let mut extended = conv_messages("q1", "a1");
        extended.push(ChatMessage::user("follow up"));
        extended.push(ChatMessage::assistant("sure"));
        store.upsert("c1", &extended).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].turn_count, 4);
        store.clear().unwrap();
    }

    #[test]
    fn empty_conversation_not_saved() {
        let store = temp_store();
        let res = store.upsert("c1", &[ChatMessage::system("sys")]).unwrap();
        assert!(res.is_none());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn delete_and_clear() {
        let store = temp_store();
        store.upsert("c1", &conv_messages("q", "a")).unwrap();
        store.upsert("c2", &conv_messages("q2", "a2")).unwrap();
        store.delete("c1").unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        store.clear().unwrap();
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn delete_reclaims_the_file() {
        let store = temp_store();
        store.upsert("c1", &conv_messages("q", "a")).unwrap();
        assert!(store.conv_path("c1").exists());
        store.delete("c1").unwrap();
        assert!(!store.conv_path("c1").exists(), "delete unlinks the file");
        // Deleting again is fine.
        store.delete("c1").unwrap();
        store.clear().unwrap();
    }

    #[test]
    fn prunes_conversations_older_than_retention() {
        let store = temp_store();
        store.upsert("recent", &conv_messages("q", "a")).unwrap();

        // Plant an ancient conversation file directly (updated_at at epoch).
        let ancient = Conversation {
            id: "ancient".into(),
            title: "old".into(),
            turn_count: 2,
            messages: conv_messages("old q", "old a"),
            created_at: 0,
            updated_at: 0, // far older than the retention window
        };
        write_conv_file(
            &store.dir,
            "ancient",
            &serde_json::to_vec(&ancient).unwrap(),
        );
        assert_eq!(store.list().unwrap().len(), 2);

        // Any upsert runs prune → the ancient one is dropped.
        store.upsert("recent2", &conv_messages("q2", "a2")).unwrap();
        let ids: Vec<String> = store.list().unwrap().into_iter().map(|s| s.id).collect();
        assert!(!ids.contains(&"ancient".to_string()));
        assert!(ids.contains(&"recent".to_string()));
        store.clear().unwrap();
    }

    #[test]
    fn keeps_only_the_newest_max_conversations() {
        let store = temp_store();

        // Plant MAX_CONVERSATIONS + 5 files with strictly increasing updated_at.
        let now = db::now_millis();
        let total = MAX_CONVERSATIONS + 5;
        for i in 0..total {
            let conv = Conversation {
                id: format!("c{i}"),
                title: format!("t{i}"),
                turn_count: 2,
                messages: conv_messages(&format!("q{i}"), "a"),
                created_at: now,
                updated_at: now + i as u64, // strictly increasing → newest last
            };
            write_conv_file(&store.dir, &conv.id, &serde_json::to_vec(&conv).unwrap());
        }
        assert_eq!(store.list().unwrap().len(), total, "all planted first");

        // Any upsert triggers prune → the store is bounded to the hard cap.
        store.upsert("trigger", &conv_messages("q", "a")).unwrap();

        let ids: std::collections::HashSet<String> =
            store.list().unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(
            ids.len(),
            MAX_CONVERSATIONS,
            "the hard cap must bound the stored conversation count"
        );
        assert!(
            ids.contains("trigger"),
            "the just-added conversation survives"
        );
        assert!(!ids.contains("c0"), "the oldest conversation was pruned");
        assert!(
            ids.contains(&format!("c{}", total - 1)),
            "the most recent planted conversation survives"
        );
        store.clear().unwrap();
    }

    #[test]
    fn a_corrupt_existing_file_does_not_block_the_upsert() {
        let store = temp_store();

        // Plant a garbage file under "c1".
        write_conv_file(&store.dir, "c1", &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert!(store.get("c1").is_err(), "the planted file is corrupt");

        // The upsert must still succeed, overwriting the corrupt file.
        let saved = store
            .upsert("c1", &conv_messages("recover me", "done"))
            .unwrap()
            .expect("a real conversation is saved");
        assert_eq!(saved.title, "recover me");
        let got = store.get("c1").unwrap().unwrap();
        assert_eq!(got.messages.len(), 3);
        store.clear().unwrap();
    }

    #[test]
    fn list_skips_a_corrupt_file_and_keeps_the_good_ones() {
        let store = temp_store();
        store.upsert("good", &conv_messages("q", "a")).unwrap();

        // Plant garbage under a second id.
        write_conv_file(&store.dir, "bad", &[1u8, 2, 3]);

        let list = store.list().unwrap();
        let ids: Vec<&str> = list.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["good"], "corrupt file skipped, good file kept");

        // A subsequent upsert (which prunes via list) still succeeds.
        store.upsert("good2", &conv_messages("q2", "a2")).unwrap();
        assert_eq!(store.list().unwrap().len(), 2);
        store.clear().unwrap();
    }

    #[test]
    fn turn_count_survives_a_pre_field_file() {
        let store = temp_store();

        // A legacy JSON body with NO turn_count key.
        let legacy = serde_json::json!({
            "id": "c1", "title": "legacy",
            "messages": [
                {"role":"user","content":[{"type":"text","text":"hi"}]},
                {"role":"assistant","content":[{"type":"text","text":"hello"}]}
            ],
            "created_at": 100, "updated_at": 100
        });
        write_conv_file(&store.dir, "c1", &serde_json::to_vec(&legacy).unwrap());

        // Pre-field file lists with turn_count 0 (no bodies parsed), but is still
        // fully readable via get().
        assert_eq!(store.list().unwrap()[0].turn_count, 0);
        assert_eq!(store.get("c1").unwrap().unwrap().messages.len(), 2);
        store.clear().unwrap();
    }
}
