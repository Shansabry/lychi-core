use crate::state::AppState;
use crate::window;
use tauri::Manager;
use tokio::io::AsyncBufReadExt;
use tokio::net::UnixListener;

/// Payload for `lychi://ai-on-selection` — the frontend renders it into the
/// normal AI surface.
#[derive(Clone, serde::Serialize)]
struct AiOnSelectionPayload {
    /// The fully rendered prompt to send to the model.
    prompt: String,
    /// What the user sees as their message (the instruction, not the payload).
    display: String,
    /// The selected text, folded into a collapsed chip on the bubble.
    body: String,
    /// A short note when the text came from somewhere unexpected, else empty.
    note: String,
}

/// Read the user's selected text and start an AI turn on it, opening the
/// launcher to show the answer.
///
/// `preset_keyword` picks a saved AI preset (`--ai summarize`); empty runs an
/// open-ended "what about this?" through the full agent.
async fn handle_ai_on_selection(handle: &tauri::AppHandle, preset_keyword: String) {
    use lychi_core::clipboard::selection::{SelectionSource, read_for_ai};
    use tauri::Emitter;

    let is_wayland = lychi_core::context::is_wayland();
    // Process spawn + a possible clipboard read — keep off the async runtime.
    let captured = tauri::async_runtime::spawn_blocking(move || read_for_ai(is_wayland))
        .await
        .ok()
        .flatten();

    let Some((text, source)) = captured else {
        // Nothing to act on. Say so where the user is looking — they pressed a
        // global hotkey, so they may not have the launcher open.
        lychi_core::notify::toast(
            "Lychi",
            "Nothing selected. Highlight some text and try again.",
        );
        return;
    };

    // A clipboard read means PRIMARY was unavailable (GNOME Wayland). Tell the
    // user, because what they highlighted may not be what we read — visible
    // degradation beats silently answering about the wrong text.
    let note = match source {
        SelectionSource::Primary => String::new(),
        SelectionSource::Clipboard => {
            "Used your clipboard — this desktop doesn't share the highlighted text.".to_string()
        }
    };

    // Resolve the preset, if one was named. An unknown keyword falls back to the
    // open-ended ask rather than failing: the user asked for AI, so give them AI.
    let state = handle.state::<AppState>();
    let preset = if preset_keyword.is_empty() {
        None
    } else {
        let db = state.db.clone();
        let kw = preset_keyword.clone();
        tauri::async_runtime::spawn_blocking(move || {
            lychi_core::ai_presets::store::AiPresetsStore::new()
                .get_preset_by_keyword(&db, &kw)
                .ok()
                .flatten()
        })
        .await
        .ok()
        .flatten()
    };

    let (prompt, display) = match &preset {
        Some(p) => (
            p.render(&text),
            // The bubble shows the instruction; the selection rides in a chip.
            p.template.replace("{input}", "…").trim().to_string(),
        ),
        None => (
            format!("Here is some text I selected:\n\n{text}\n\nWhat can you tell me about it?"),
            "About the selected text".to_string(),
        ),
    };

    // Show the launcher, then hand the turn to the frontend. It owns the AI
    // surface, so the answer streams into the same place every other answer does.
    if let Some(w) = handle.get_webview_window("main") {
        window::show_window(&w);
    }
    let _ = handle.emit(
        "lychi://ai-on-selection",
        AiOnSelectionPayload {
            prompt,
            display,
            body: text,
            note,
        },
    );
}

pub async fn run(handle: tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let path = crate::platform::ipc_path();

    // Clean up stale socket
    if path.exists() {
        std::fs::remove_file(&path)?;
    }

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&path)?;
    tracing::info!("IPC server listening on {}", path.display());

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let handle = handle.clone();
                tokio::spawn(async move {
                    let reader = tokio::io::BufReader::new(stream);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let trimmed = line.trim();
                        match trimmed {
                            "toggle" => {
                                if let Some(w) = handle.get_webview_window("main") {
                                    window::toggle_window(&w);
                                }
                            }
                            // Global screenshot trigger — fired via `lychi
                            // --screenshot [mode]` bound to a desktop shortcut.
                            // Runs the capture through the executor without ever
                            // showing the launcher window.
                            _ if trimmed.starts_with("screenshot") => {
                                let mode = trimmed
                                    .strip_prefix("screenshot")
                                    .unwrap_or("")
                                    .trim()
                                    .to_string();
                                let cmd = format!("screenshot {mode}");
                                let state = handle.state::<AppState>();
                                let executor = state.executor.read().await;
                                let privacy = state.config.read().await.privacy.clone();
                                if let Err(e) = executor
                                    .run(
                                        cmd.trim(),
                                        true,
                                        &privacy,
                                        &lychi_core::executor::RunInputs::default(),
                                    )
                                    .await
                                {
                                    tracing::warn!("[ipc] screenshot failed: {e}");
                                }
                            }
                            // "AI on the selected text" — fired via `lychi --ai
                            // [preset]` bound to a desktop shortcut. Reads what
                            // the user has highlighted in whatever app has
                            // focus, then opens the launcher with the answer
                            // streaming into the normal AI surface.
                            //
                            // The launcher IS the answer surface (rather than a
                            // new floating window) so streaming, copy,
                            // regenerate and follow-up all work unchanged — and
                            // there's one place an AI answer ever appears.
                            _ if trimmed == "ai" || trimmed.starts_with("ai ") => {
                                let preset =
                                    trimmed.strip_prefix("ai").unwrap_or("").trim().to_string();
                                handle_ai_on_selection(&handle, preset).await;
                            }
                            other => {
                                tracing::warn!("Unknown IPC command: {other}");
                            }
                        }
                    }
                });
            }
            Err(e) => {
                tracing::error!("IPC accept error: {e}");
            }
        }
    }
}
