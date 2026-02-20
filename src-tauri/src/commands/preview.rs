use base64::Engine;
use lychi_core::error::LychiError;
use std::path::Path;

const MAX_TEXT_BYTES: u64 = 100_000; // 100KB
const MAX_TEXT_LINES: usize = 500;
const MAX_IMAGE_BYTES: u64 = 10_000_000; // 10MB

#[derive(serde::Serialize)]
#[serde(tag = "kind")]
pub enum FilePreviewData {
    Text {
        content: String,
        language: String,
        truncated: bool,
    },
    Image {
        base64: String,
        mime: String,
    },
    Unsupported {
        mime: String,
        size_bytes: u64,
    },
    Directory {
        item_count: usize,
        children: Vec<DirChild>,
    },
}

#[derive(Clone, serde::Serialize)]
pub struct DirChild {
    pub name: String,
    pub is_dir: bool,
}

/// Derive a language identifier from a file extension.
fn extension_to_language(ext: &str) -> Option<&'static str> {
    match ext {
        "md" | "markdown" => Some("markdown"),
        "json" => Some("json"),
        "txt" | "text" | "log" => Some("text"),
        "rs" => Some("rust"),
        "py" | "pyw" => Some("python"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "svelte" => Some("svelte"),
        "go" => Some("go"),
        "c" => Some("c"),
        "cpp" | "cc" | "cxx" => Some("cpp"),
        "h" | "hpp" => Some("c"),
        "css" => Some("css"),
        "html" | "htm" => Some("html"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        "sh" | "bash" | "zsh" | "fish" => Some("shell"),
        "conf" | "cfg" | "ini" | "env" => Some("config"),
        "xml" | "svg" => Some("xml"),
        "java" => Some("java"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "lua" => Some("lua"),
        "sql" => Some("sql"),
        "dockerfile" => Some("dockerfile"),
        "makefile" => Some("makefile"),
        _ => None,
    }
}

/// Check if a filename (without extension) is a known text file.
fn known_text_filename(name: &str) -> Option<&'static str> {
    match name.to_lowercase().as_str() {
        "makefile" | "gnumakefile" => Some("makefile"),
        "dockerfile" => Some("dockerfile"),
        "license" | "licence" | "copying" => Some("text"),
        "readme" => Some("text"),
        "changelog" | "changes" => Some("text"),
        "authors" | "contributors" => Some("text"),
        "gitignore" | "dockerignore" | "editorconfig" => Some("config"),
        "justfile" => Some("makefile"),
        _ => None,
    }
}

/// Map extension to MIME type for images.
fn extension_to_image_mime(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

#[tauri::command]
pub async fn get_file_preview(path: String) -> Result<FilePreviewData, LychiError> {
    let file_path = Path::new(&path);

    if !file_path.exists() {
        return Err(LychiError::ExecutionFailed(format!(
            "File not found: {path}"
        )));
    }
    if file_path.is_dir() {
        let mut children: Vec<DirChild> = std::fs::read_dir(file_path)?
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                if name.starts_with('.') {
                    return None;
                }
                let is_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                Some(DirChild { name, is_dir })
            })
            .collect();
        children.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
        let item_count = children.len();
        children.truncate(20);
        return Ok(FilePreviewData::Directory {
            item_count,
            children,
        });
    }
    if !file_path.is_file() {
        return Err(LychiError::ExecutionFailed(format!("Not a file: {path}")));
    }

    let metadata = std::fs::metadata(file_path)?;
    let size = metadata.len();

    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");

    // Check for image files
    if let Some(mime) = extension_to_image_mime(&ext) {
        if size > MAX_IMAGE_BYTES {
            return Ok(FilePreviewData::Unsupported {
                mime: mime.to_string(),
                size_bytes: size,
            });
        }
        let bytes = std::fs::read(file_path)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        return Ok(FilePreviewData::Image {
            base64: b64,
            mime: mime.to_string(),
        });
    }

    // Check for text files by extension
    let language = extension_to_language(&ext).or_else(|| {
        // Try filename without extension for known files
        let stem = file_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        // If no extension, check the full filename
        if ext.is_empty() {
            known_text_filename(file_name)
        } else {
            known_text_filename(stem)
        }
    });

    if let Some(lang) = language {
        if size > MAX_TEXT_BYTES {
            // Read only up to the limit
            let content = read_text_limited(file_path, MAX_TEXT_BYTES as usize, MAX_TEXT_LINES)?;
            return Ok(FilePreviewData::Text {
                content,
                language: lang.to_string(),
                truncated: true,
            });
        }
        let raw = std::fs::read(file_path)?;
        // Check if it looks like binary (contains null bytes in first 8KB)
        let check_len = raw.len().min(8192);
        if raw[..check_len].contains(&0) {
            return Ok(FilePreviewData::Unsupported {
                mime: "application/octet-stream".to_string(),
                size_bytes: size,
            });
        }
        let full = String::from_utf8_lossy(&raw);
        let mut truncated = false;
        let content = if full.lines().count() > MAX_TEXT_LINES {
            truncated = true;
            full.lines()
                .take(MAX_TEXT_LINES)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            full.into_owned()
        };
        return Ok(FilePreviewData::Text {
            content,
            language: lang.to_string(),
            truncated,
        });
    }

    // No recognized extension — try binary detection on small files
    if size <= MAX_TEXT_BYTES {
        let raw = std::fs::read(file_path)?;
        let check_len = raw.len().min(8192);
        if !raw[..check_len].contains(&0) {
            // Looks like text
            let full = String::from_utf8_lossy(&raw);
            let mut truncated = false;
            let content = if full.lines().count() > MAX_TEXT_LINES {
                truncated = true;
                full.lines()
                    .take(MAX_TEXT_LINES)
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                full.into_owned()
            };
            return Ok(FilePreviewData::Text {
                content,
                language: "text".to_string(),
                truncated,
            });
        }
    }

    Ok(FilePreviewData::Unsupported {
        mime: "application/octet-stream".to_string(),
        size_bytes: size,
    })
}

/// Read a text file up to a byte and line limit.
fn read_text_limited(
    path: &Path,
    max_bytes: usize,
    max_lines: usize,
) -> Result<String, LychiError> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut result = String::new();
    let mut byte_count = 0usize;

    for line in reader.lines().take(max_lines) {
        let line = line?;
        byte_count += line.len() + 1; // +1 for newline
        if byte_count > max_bytes {
            break;
        }
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&line);
    }

    Ok(result)
}
