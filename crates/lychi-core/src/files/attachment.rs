//! Attachment DTO — the shape the launcher UI renders as a chip, and the ONE
//! place that decides how an attached file will reach the model.
//!
//! Phases 2 and 3 built the two backend pipes: images become base64 vision
//! blocks (`image_ops::encode_image_for_vision`), documents become inlined text
//! (`text_extract::extract_text`). This module classifies a user-supplied path
//! into whichever pipe applies and hands the frontend everything a chip needs to
//! draw itself — so the FE never re-derives file types or extraction rules.
//!
//! Pure + synchronous (no Tauri, no async). Callers wrap in `spawn_blocking`.

use serde::{Deserialize, Serialize};

use super::{FileKind, classify_path, image_ops, paths::expand_home};

/// How an attachment reaches the model. The frontend switches on this to decide
/// which argument of `agent_chat_start` a chip feeds, so the routing rule lives
/// in core rather than being duplicated in TypeScript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "kebab-case")]
pub enum AttachmentRoute {
    /// Encoded as a base64 image block (vision). Needs a vision-capable model.
    Vision,
    /// Text is extracted and inlined into the prompt.
    Text,
    /// We can't feed this to the model — `note` explains why.
    Unsupported,
}

/// One attached file, fully classified and ready to render as a chip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct FileAttachment {
    /// Absolute path (`~` already expanded) — what gets sent back on submit.
    pub path: String,
    /// Base name, shown on the chip.
    pub name: String,
    pub kind: FileKind,
    pub mime: String,
    pub route: AttachmentRoute,
    /// Size on disk, `None` if unreadable.
    pub size_bytes: Option<u64>,
    /// A tiny PNG `data:` URI preview — images only, `None` otherwise.
    pub thumbnail: Option<String>,
    /// Why an attachment is `Unsupported`, or a caveat worth showing. `None`
    /// when the file is plainly usable.
    pub note: Option<String>,
}

/// Classify one path into a chip-ready attachment. Never fails: an unreadable
/// or unusable file comes back as `Unsupported` with a human-readable `note`,
/// because the UI still needs to render *something* the user can remove.
pub fn classify_attachment(raw_path: &str) -> FileAttachment {
    let path = expand_home(raw_path);
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| raw_path.to_string());
    let path_str = path.to_string_lossy().to_string();

    let unsupported = |mime: String, kind: FileKind, note: &str| FileAttachment {
        path: path_str.clone(),
        name: name.clone(),
        kind,
        mime,
        route: AttachmentRoute::Unsupported,
        size_bytes: None,
        thumbnail: None,
        note: Some(note.to_string()),
    };

    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) => {
            return unsupported(
                "application/octet-stream".into(),
                FileKind::Other,
                &format!("Can't read this file: {e}"),
            );
        }
    };
    if meta.is_dir() {
        return unsupported(
            "inode/directory".into(),
            FileKind::Other,
            "Folders can't be attached — pick a file inside it.",
        );
    }

    let ft = classify_path(&path);
    let size = Some(meta.len());

    let (route, note) = match ft.kind {
        FileKind::Image => (AttachmentRoute::Vision, None),
        FileKind::Doc | FileKind::Text => (AttachmentRoute::Text, None),
        FileKind::Archive => (
            AttachmentRoute::Unsupported,
            Some("Archives can't be read directly — extract it first.".to_string()),
        ),
        FileKind::Other => (
            AttachmentRoute::Unsupported,
            Some("Lychi can't read this file type.".to_string()),
        ),
    };

    // Thumbnails are best-effort: a corrupt or exotic image still attaches (the
    // vision encoder gets its own shot at it), it just shows a generic chip.
    let thumbnail = if matches!(ft.kind, FileKind::Image) {
        image_ops::encode_thumbnail(&path).ok()
    } else {
        None
    };

    FileAttachment {
        path: path_str,
        name,
        kind: ft.kind,
        mime: ft.mime,
        route,
        size_bytes: size,
        thumbnail,
        note,
    }
}

/// Classify a batch of paths, preserving order. Deduplicates by resolved path so
/// dropping the same file twice yields one chip.
pub fn classify_attachments(paths: &[String]) -> Vec<FileAttachment> {
    let mut seen: Vec<String> = Vec::with_capacity(paths.len());
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let att = classify_attachment(p);
        if seen.contains(&att.path) {
            continue;
        }
        seen.push(att.path.clone());
        out.push(att);
    }
    out
}

/// Split classified attachments into the two backend pipes: image paths for the
/// vision encoder, text-bearing paths for prompt inlining. `Unsupported` entries
/// are dropped — they were only ever chips.
pub fn split_routes(atts: &[FileAttachment]) -> (Vec<String>, Vec<String>) {
    let mut images = Vec::new();
    let mut texts = Vec::new();
    for a in atts {
        match a.route {
            AttachmentRoute::Vision => images.push(a.path.clone()),
            AttachmentRoute::Text => texts.push(a.path.clone()),
            AttachmentRoute::Unsupported => {}
        }
    }
    (images, texts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d =
            std::env::temp_dir().join(format!("lychi-atttest-{}-{tag}-{n}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn image_routes_to_vision_with_thumbnail() {
        let dir = tmp_dir("img");
        let p = dir.join("shot.png");
        let buf = image::RgbImage::from_pixel(200, 100, image::Rgb([9, 9, 9]));
        image::DynamicImage::ImageRgb8(buf).save(&p).unwrap();

        let a = classify_attachment(p.to_str().unwrap());
        assert_eq!(a.kind, FileKind::Image);
        assert_eq!(a.route, AttachmentRoute::Vision);
        assert_eq!(a.name, "shot.png");
        assert!(a.size_bytes.unwrap() > 0);
        let thumb = a.thumbnail.expect("image should get a thumbnail");
        assert!(thumb.starts_with("data:image/png;base64,"));
        assert!(a.note.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn text_file_routes_to_text_without_thumbnail() {
        let dir = tmp_dir("txt");
        let p = dir.join("notes.md");
        std::fs::write(&p, "# hello").unwrap();

        let a = classify_attachment(p.to_str().unwrap());
        assert_eq!(a.kind, FileKind::Text);
        assert_eq!(a.route, AttachmentRoute::Text);
        assert!(a.thumbnail.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_unsupported_not_an_error() {
        let a = classify_attachment("/nonexistent/nope.png");
        assert_eq!(a.route, AttachmentRoute::Unsupported);
        assert!(a.note.is_some());
        assert_eq!(a.name, "nope.png");
    }

    #[test]
    fn directory_is_refused() {
        let dir = tmp_dir("dir");
        let a = classify_attachment(dir.to_str().unwrap());
        assert_eq!(a.route, AttachmentRoute::Unsupported);
        assert!(a.note.unwrap().contains("Folders"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn archive_is_refused_with_a_hint() {
        let dir = tmp_dir("zip");
        let p = dir.join("bundle.zip");
        // Minimal empty-zip bytes so `infer` sniffs it as an archive.
        std::fs::write(
            &p,
            [
                0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        )
        .unwrap();

        let a = classify_attachment(p.to_str().unwrap());
        assert_eq!(a.kind, FileKind::Archive);
        assert_eq!(a.route, AttachmentRoute::Unsupported);
        assert!(a.note.unwrap().contains("extract"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn batch_dedupes_and_splits_routes() {
        let dir = tmp_dir("batch");
        let img = dir.join("a.png");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 4, image::Rgb([1, 2, 3])))
            .save(&img)
            .unwrap();
        let doc = dir.join("b.txt");
        std::fs::write(&doc, "text").unwrap();

        let paths = vec![
            img.to_string_lossy().to_string(),
            doc.to_string_lossy().to_string(),
            // Same image again — must collapse to one chip.
            img.to_string_lossy().to_string(),
        ];
        let atts = classify_attachments(&paths);
        assert_eq!(atts.len(), 2);

        let (images, texts) = split_routes(&atts);
        assert_eq!(images.len(), 1);
        assert_eq!(texts.len(), 1);
        assert!(images[0].ends_with("a.png"));
        assert!(texts[0].ends_with("b.txt"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unsupported_entries_are_dropped_from_routes() {
        let atts = classify_attachments(&["/nope/missing.bin".to_string()]);
        let (images, texts) = split_routes(&atts);
        assert!(images.is_empty() && texts.is_empty());
    }
}
