//! Shared AI-stream cancellation. The tool-calling agent (`agent_chat`) is the
//! single AI path; this holds the `cancel_ai_chat` command it (and the frontend
//! Esc handler) uses to stop an in-flight run.
//!
//! Note: Rust 2024 reserves `gen`, so generation ids are `generation` in Rust.

use std::sync::atomic::Ordering;

use lychi_core::error::LychiError;
use tauri::State;

use crate::state::AppState;

/// Cancel any in-flight agent run: bump the generation (so late events drop on
/// both ends) and cancel the current token (so the stream stops at its source).
#[tauri::command]
#[specta::specta]
pub async fn cancel_ai_chat(state: State<'_, AppState>) -> Result<(), LychiError> {
    let cur = state.ai_generation.load(Ordering::Relaxed);
    state
        .ai_generation
        .store(cur.wrapping_add(1), Ordering::Relaxed);
    if let Some(token) = state.ai_cancel.write().await.take() {
        token.cancel();
    }
    Ok(())
}
