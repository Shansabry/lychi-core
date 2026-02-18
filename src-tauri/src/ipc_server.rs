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
                        match line.trim() {
                            "toggle" => {
                                if let Some(w) = handle.get_webview_window("main") {
                                    window::toggle_window(&w);
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
