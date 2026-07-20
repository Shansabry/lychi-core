use crate::state::AppState;
use crate::window;
use tauri::Manager;
use tokio::io::AsyncBufReadExt;
use tokio::net::UnixListener;

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
