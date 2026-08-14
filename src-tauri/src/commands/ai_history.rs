//! AI conversation-history commands (Phase 4). Thin wrappers over the core
//! `AiHistoryStore`. Completed agent conversations are persisted (in `drive`)
//! and recalled here via the `chat` keyword: list summaries, load one to
//! continue, delete, or clear all.

use tauri::State;

use lychi_core::ai_history::store::AiHistoryStore;
use lychi_core::ai_history::{Conversation, ConversationSummary};
use lychi_core::error::LychiError;

use crate::state::AppState;

#[tauri::command]
#[specta::specta]
pub async fn get_conversations() -> Result<Vec<ConversationSummary>, LychiError> {
    AiHistoryStore::new().list()
}

#[tauri::command]
#[specta::specta]
pub async fn get_conversation(id: String) -> Result<Option<Conversation>, LychiError> {
    AiHistoryStore::new().get(&id)
}

#[tauri::command]
#[specta::specta]
pub async fn delete_conversation(id: String) -> Result<(), LychiError> {
    AiHistoryStore::new().delete(&id)
}

#[tauri::command]
#[specta::specta]
pub async fn clear_conversations() -> Result<(), LychiError> {
    AiHistoryStore::new().clear()
}

/// Load a stored conversation into the active agent session so the next message
/// continues it (recall). Primes `agent_session` + `agent_conversation_id` with
/// the stored history, and returns the full conversation for the UI to render.
#[tauri::command]
#[specta::specta]
pub async fn load_conversation(
    id: String,
    state: State<'_, AppState>,
) -> Result<Option<Conversation>, LychiError> {
    let Some(conv) = AiHistoryStore::new().get(&id)? else {
        return Ok(None);
    };
    let session = lychi_core::coordinator::Session {
        messages: conv.messages.clone(),
        pending: Vec::new(),
    };
    *state.agent_session.write().await = Some(session);
    *state.agent_conversation_id.write().await = Some(id);
    Ok(Some(conv))
}
