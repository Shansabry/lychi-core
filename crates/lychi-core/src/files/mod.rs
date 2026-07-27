//! File utilities brick — the shared foundation for every file-aware action
//! (resize, convert, zip, extract, and later document/image analysis).
//!
//! This module owns the ONE MIME/type router the whole feature shares
//! (`classify_path`), the path helpers (`paths`), image operations
//! (`image_ops`), and archive operations (`archive`). Handlers are thin wrappers
//! over these functions so the logic stays testable without Tauri.

pub mod archive;
pub mod attachment;
pub mod image_ops;
pub mod paths;
pub mod text_extract;

use std::path::Path;

use serde::{Deserialize, Serialize};

/// A coarse classification of what a file *is*, used to route it to the right
/// action (resize/convert an image, extract an archive, analyze a document).
/// Determined by magic bytes first (`infer`), extension second (`mime_guess`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Image,
    /// A rich document we extract text from (PDF, docx, …).
    Doc,
    /// Plain text / source code — read directly.
    Text,
    Archive,
    Other,
}

/// The type verdict for a path: its MIME string and coarse `FileKind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct FileType {
    pub mime: String,
    pub kind: FileKind,
}

/// Extensions we treat as plain text / source even though they have no magic
/// signature (so `infer` returns `None` and `mime_guess` may call them
/// `application/octet-stream`). Kept deliberately small and generic — this is a
/// fallback, not an exhaustive language list.
const TEXT_EXTS: &[&str] = &[
    "txt", "md", "markdown", "csv", "tsv", "log", "json", "yaml", "yml", "toml", "ini", "conf",
    "xml", "html", "htm", "css", "js", "ts", "tsx", "jsx", "rs", "py", "go", "c", "h", "cpp",
    "hpp", "java", "kt", "rb", "php", "sh", "bash", "zsh", "sql", "svelte", "vue",
];

/// Document extensions we extract text from rather than reading raw.
const DOC_EXTS: &[&str] = &["pdf", "docx", "pptx", "odt", "rtf", "epub"];

/// Classify a file by content (magic bytes) with an extension fallback. Reads
/// only the first several KB for sniffing. Returns `Other` for anything we don't
/// recognize — callers decide whether that's actionable.
pub fn classify_path(path: &Path) -> FileType {
    // 1. Magic-byte sniff (authoritative for images/archives/known docs).
    if let Ok(Some(t)) = infer::get_from_path(path) {
        let mime = t.mime_type().to_string();
        let kind = if t.matcher_type() == infer::MatcherType::Image {
            FileKind::Image
        } else if is_archive_mime(&mime) {
            FileKind::Archive
        } else if is_doc_mime(&mime) {
            FileKind::Doc
        } else {
            FileKind::Other
        };
        return FileType { mime, kind };
    }

    // 2. Extension fallback (text/code and name-only cases).
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    if DOC_EXTS.contains(&ext.as_str()) {
        let mime = mime_guess::from_path(path)
            .first_raw()
            .unwrap_or("application/octet-stream")
            .to_string();
        return FileType {
            mime,
            kind: FileKind::Doc,
        };
    }
    if TEXT_EXTS.contains(&ext.as_str()) {
        let mime = mime_guess::from_path(path)
            .first_raw()
            .unwrap_or("text/plain")
            .to_string();
        return FileType {
            mime,
            kind: FileKind::Text,
        };
    }

    // 3. Unknown — hand back the guessed MIME (or octet-stream) as `Other`.
    let mime = mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream")
        .to_string();
    FileType {
        mime,
        kind: FileKind::Other,
    }
}

fn is_archive_mime(mime: &str) -> bool {
    matches!(
        mime,
        "application/zip"
            | "application/gzip"
            | "application/x-tar"
            | "application/x-bzip2"
            | "application/x-xz"
            | "application/vnd.rar"
            | "application/x-7z-compressed"
            | "application/zstd"
    )
}

fn is_doc_mime(mime: &str) -> bool {
    matches!(
        mime,
        "application/pdf"
            | "application/msword"
            | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            | "application/vnd.oasis.opendocument.text"
            | "application/epub+zip"
            | "application/rtf"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn classifies_text_by_extension() {
        // No such file on disk → infer returns None → extension fallback.
        let t = classify_path(Path::new("/nonexistent/notes.md"));
        assert_eq!(t.kind, FileKind::Text);
        assert!(t.mime.starts_with("text/"));
    }

    #[test]
    fn classifies_code_by_extension() {
        assert_eq!(classify_path(Path::new("/x/main.rs")).kind, FileKind::Text);
    }

    #[test]
    fn classifies_pdf_by_extension() {
        assert_eq!(
            classify_path(Path::new("/x/report.pdf")).kind,
            FileKind::Doc
        );
    }

    #[test]
    fn unknown_extension_is_other() {
        assert_eq!(
            classify_path(Path::new("/x/thing.xyzzy")).kind,
            FileKind::Other
        );
    }
}
