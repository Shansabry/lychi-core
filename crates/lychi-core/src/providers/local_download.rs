//! Model downloader for local AI — fetches a model's GGUF weights to
//! `paths::models_dir()`, streaming progress via a callback. Tauri-free (core):
//! the bridge wraps `on_progress` into a `lychi://model-download-progress` event.
//!
//! Only the GGUF is downloaded: llama.cpp reads the tokenizer and architecture
//! from the GGUF itself, so there's no separate tokenizer file to fetch.
//!
//! Best-standards notes:
//! - Streamed download (constant memory) with total-size progress.
//! - SHA-256 integrity check (skipped, with a warning, while a spec's hash is the
//!   all-zero placeholder — so the pipeline works before hashes are pinned).
//! - Atomic install: download to a `.part` file, verify, then rename into place —
//!   a failed/aborted download never leaves a corrupt model that "exists".
//! - Idempotent: an already-present, correct file is a no-op.

use std::io::Write;
use std::path::Path;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};

use crate::error::LychiError;
use crate::paths;
use crate::providers::local_models::{self, ModelSpec};

/// Progress update for a download. `total` is `None` when the server didn't send
/// a Content-Length.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub model_id: String,
    pub file_label: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub done: bool,
}

/// Whether the model's GGUF is already present on disk.
pub fn is_downloaded(spec: &ModelSpec) -> bool {
    paths::models_dir().join(spec.gguf_filename()).exists()
}

const ZERO_SHA: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Download `spec`'s GGUF weights into `models_dir()`, reporting progress.
/// Idempotent. `on_progress` is called repeatedly (throttled by the caller if
/// desired). Errors leave no partial file in place.
pub async fn download(
    spec: &ModelSpec,
    on_progress: impl Fn(DownloadProgress),
) -> Result<(), LychiError> {
    let dir = paths::models_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| LychiError::Ai(format!("create models dir: {e}")))?;

    download_file(
        spec,
        "weights",
        spec.gguf_url,
        &dir.join(spec.gguf_filename()),
        Some(spec.gguf_sha256),
        &on_progress,
    )
    .await?;

    Ok(())
}

async fn download_file(
    spec: &ModelSpec,
    label: &str,
    url: &str,
    dest: &Path,
    expected_sha: Option<&str>,
    on_progress: &impl Fn(DownloadProgress),
) -> Result<(), LychiError> {
    // Idempotent: skip if already present (and, when a real hash is set, valid).
    if dest.exists() {
        match expected_sha {
            Some(sha) if sha != ZERO_SHA => {
                if file_sha256(dest)?.eq_ignore_ascii_case(sha) {
                    return Ok(());
                }
                tracing::warn!("[local-ai] {} checksum mismatch — re-downloading", label);
            }
            _ => return Ok(()),
        }
    }

    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| LychiError::Ai(format!("download {label}: {e}")))?;
    if !resp.status().is_success() {
        return Err(LychiError::Ai(format!(
            "download {label}: HTTP {}",
            resp.status()
        )));
    }
    let total = resp.content_length();

    // Stream to a temp .part file, then atomically rename on success.
    let part = dest.with_extension("part");
    let mut file = std::fs::File::create(&part)
        .map_err(|e| LychiError::Ai(format!("create {}: {e}", part.display())))?;
    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| LychiError::Ai(format!("stream {label}: {e}")))?;
        file.write_all(&chunk)
            .map_err(|e| LychiError::Ai(format!("write {label}: {e}")))?;
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;
        on_progress(DownloadProgress {
            model_id: spec.id.to_string(),
            file_label: label.to_string(),
            downloaded,
            total,
            done: false,
        });
    }
    file.flush().ok();
    drop(file);

    // Verify integrity when a real hash is pinned.
    if let Some(sha) = expected_sha
        && sha != ZERO_SHA
    {
        let got = format!("{:x}", hasher.finalize());
        if !got.eq_ignore_ascii_case(sha) {
            let _ = std::fs::remove_file(&part);
            return Err(LychiError::Ai(format!(
                "{label} checksum mismatch: expected {sha}, got {got}"
            )));
        }
    } else if expected_sha == Some(ZERO_SHA) {
        tracing::warn!("[local-ai] {label} downloaded WITHOUT integrity check (hash not pinned)");
    }

    std::fs::rename(&part, dest)
        .map_err(|e| LychiError::Ai(format!("install {label}: {e}")))?;

    on_progress(DownloadProgress {
        model_id: spec.id.to_string(),
        file_label: label.to_string(),
        downloaded,
        total,
        done: true,
    });
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String, LychiError> {
    let mut file =
        std::fs::File::open(path).map_err(|e| LychiError::Ai(format!("open for hash: {e}")))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| LychiError::Ai(format!("hash read: {e}")))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Look up a spec by id (thin re-export so the download command doesn't import
/// two modules).
pub fn find(id: &str) -> Option<&'static ModelSpec> {
    local_models::find(id)
}
