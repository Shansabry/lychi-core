//! Clipboard content type detection.
//!
//! Reads the current system clipboard on summon and classifies the content
//! so the suggestion engine can offer contextual actions (open URL, ping IP, etc.).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Detected content type of clipboard text.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(tag = "type", content = "value")]
pub enum ClipboardContentType {
    /// A URL (http/https).
    Url(String),
    /// An existing file or directory path on disk.
    FilePath(String),
    /// An IPv4 or IPv6 address.
    IpAddress(String),
    /// A UUID (v1–v7).
    Uuid(String),
    /// A git commit hash (7–40 hex chars).
    GitHash(String),
    /// Valid JSON.
    Json,
    /// A stack trace or error message. Carries the key error line
    /// (truncated) so suggestions can offer a meaningful search query.
    ErrorTrace(String),
    /// Plain text with no special classification.
    Plain,
}

/// Snapshot the current clipboard and classify its content.
///
/// Returns `None` if the clipboard is empty or unreadable.
/// The returned tuple is `(text, content_type)`.
pub fn detect() -> Option<(String, ClipboardContentType)> {
    let text = read_clipboard()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        tracing::debug!("clipboard_detect: clipboard empty");
        return None;
    }

    // Only classify the first 4 KB to avoid spending time on huge clipboard
    // payloads.
    let sample = sample_for_classify(trimmed);

    let content_type = classify(sample);
    tracing::debug!("clipboard_detect: classified as {:?}", content_type);
    Some((trimmed.to_string(), content_type))
}

/// Take at most the first ~4 KB of `text`, truncated on a UTF-8 char boundary.
///
/// A naive `&text[..4096]` panics when byte 4096 lands inside a multi-byte char
/// (emoji, CJK glyph, etc.) — which crashed the context detector on real-world
/// clipboard contents. Walk back to the nearest char boundary instead.
fn sample_for_classify(text: &str) -> &str {
    if text.len() <= 4096 {
        return text;
    }
    let mut end = 4096;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Classify a clipboard text sample. Most-specific patterns checked first.
fn classify(text: &str) -> ClipboardContentType {
    // Single-line content gets richer classification.
    let first_line = text.lines().next().unwrap_or(text);
    let is_single_line = text.lines().count() <= 1;

    // 1. URL — starts with http(s):// or www.
    if is_single_line && looks_like_url(first_line) {
        return ClipboardContentType::Url(first_line.to_string());
    }

    // 2. File path — starts with / or ~/ and exists on disk
    if is_single_line && looks_like_file_path(first_line) {
        return ClipboardContentType::FilePath(first_line.to_string());
    }

    // 3. IP address
    if is_single_line && looks_like_ip(first_line) {
        return ClipboardContentType::IpAddress(first_line.to_string());
    }

    // 4. UUID
    if is_single_line && looks_like_uuid(first_line) {
        return ClipboardContentType::Uuid(first_line.to_string());
    }

    // 5. Git hash (only single-line, 7–40 hex chars)
    if is_single_line && looks_like_git_hash(first_line) {
        return ClipboardContentType::GitHash(first_line.to_string());
    }

    // 6. JSON (can be multi-line)
    if looks_like_json(text) {
        return ClipboardContentType::Json;
    }

    // 7. Error / stack trace
    if let Some(key_line) = extract_error_line(text) {
        return ClipboardContentType::ErrorTrace(key_line);
    }

    ClipboardContentType::Plain
}

/// Read text from the system clipboard.
///
/// Tries arboard first. On Wayland, if arboard fails (which can happen when
/// called from a non-main thread without a display context), falls back to
/// `wl-paste --no-newline`.
pub fn read_clipboard() -> Option<String> {
    // Try arboard first (works on X11, sometimes on Wayland)
    match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
        Ok(text) if !text.trim().is_empty() => {
            tracing::debug!("clipboard_detect: arboard read ok ({} chars)", text.len());
            return Some(text);
        }
        Ok(_) => {
            tracing::debug!("clipboard_detect: arboard returned empty");
        }
        Err(e) => {
            tracing::debug!("clipboard_detect: arboard failed: {e}");
        }
    }

    // Wayland fallback: wl-paste
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        match std::process::Command::new("wl-paste")
            .arg("--no-newline")
            .output()
        {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout).to_string();
                if !text.trim().is_empty() {
                    tracing::debug!(
                        "clipboard_detect: wl-paste fallback ok ({} chars)",
                        text.len()
                    );
                    return Some(text);
                }
            }
            Ok(output) => {
                tracing::debug!(
                    "clipboard_detect: wl-paste exit code {:?}",
                    output.status.code()
                );
            }
            Err(e) => {
                tracing::debug!("clipboard_detect: wl-paste not available: {e}");
            }
        }
    }

    None
}

fn looks_like_url(s: &str) -> bool {
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("ftp://")
        || (s.starts_with("www.") && s.len() > 6)
}

fn looks_like_file_path(s: &str) -> bool {
    if s.starts_with('/') || s.starts_with("~/") {
        // Expand ~ for existence check
        let expanded = if let Some(rest) = s.strip_prefix("~/") {
            if let Ok(home) = std::env::var("HOME") {
                format!("{home}/{rest}")
            } else {
                return false;
            }
        } else {
            s.to_string()
        };
        Path::new(&expanded).exists()
    } else {
        false
    }
}

fn looks_like_ip(s: &str) -> bool {
    // IPv4: 1.2.3.4 (no port)
    if s.split('.').count() == 4 && s.split('.').all(|octet| octet.parse::<u8>().is_ok()) {
        return true;
    }
    // IPv6: contains :: or 3+ colons with hex segments
    if s.contains("::") || s.matches(':').count() >= 3 {
        let s = s.trim_start_matches('[').trim_end_matches(']');
        return s
            .split(':')
            .all(|seg| seg.is_empty() || u16::from_str_radix(seg, 16).is_ok());
    }
    false
}

fn looks_like_uuid(s: &str) -> bool {
    // 8-4-4-4-12 hex pattern
    let s = s.trim();
    if s.len() != 36 {
        return false;
    }
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    let expected_lens = [8, 4, 4, 4, 12];
    parts
        .iter()
        .zip(expected_lens)
        .all(|(part, len)| part.len() == len && part.chars().all(|c| c.is_ascii_hexdigit()))
}

fn looks_like_git_hash(s: &str) -> bool {
    let s = s.trim();
    (7..=40).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn looks_like_json(s: &str) -> bool {
    let trimmed = s.trim();
    // Quick check: must start with { or [
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
}

/// Detect an error/stack trace and extract its key message line.
///
/// Deliberately strict: matching is case-sensitive and message tokens must
/// anchor a line start. The old substring/lowercase matching classified any
/// prose that merely *mentioned* "error:" (chat logs, docs) as a stack
/// trace, producing junk "search this error" suggestions.
fn extract_error_line(s: &str) -> Option<String> {
    // A line starting with one of these IS the error message.
    const LINE_START_TOKENS: [&str; 10] = [
        "Error:",
        "TypeError:",
        "SyntaxError:",
        "ReferenceError:",
        "RuntimeError:",
        "ValueError:",
        "FATAL:",
        "panic!",
        "thread '",
        "error[E",
    ];
    // Multi-line trace shapes; the message line is found separately.
    const TRACE_SIGNALS: [&str; 5] = [
        "Traceback (most recent",
        "Exception in thread",
        "panicked at",
        "Caused by:",
        "stack trace:",
    ];

    if let Some(line) = s
        .lines()
        .map(str::trim)
        .find(|l| LINE_START_TOKENS.iter().any(|t| l.starts_with(t)))
    {
        return Some(truncate_line(line));
    }

    if TRACE_SIGNALS.iter().any(|t| s.contains(t)) {
        let msg = s
            .lines()
            .map(str::trim)
            .find(|l| l.contains("Error:") || l.contains("Exception") || l.contains("panicked at"))
            .or_else(|| s.lines().map(str::trim).find(|l| !l.is_empty()))?;
        return Some(truncate_line(msg));
    }

    None
}

/// Cap the extracted line for use as a completion label / search query.
fn truncate_line(line: &str) -> String {
    crate::text::truncate_display(line, 80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_for_classify_never_panics_on_multibyte_boundary() {
        // Build a string where a 3-byte char straddles byte 4096. "本" is 3 bytes;
        // 4095 ASCII bytes + "本" puts the char across the 4096 cut point.
        let mut s = "a".repeat(4095);
        s.push('本'); // bytes 4095..4098
        s.push_str(&"b".repeat(100));
        // Must not panic, and must return valid UTF-8 truncated before the char.
        let sample = sample_for_classify(&s);
        assert!(sample.len() <= 4096);
        assert_eq!(sample.len(), 4095, "should stop before the straddling char");
        assert!(sample.chars().all(|c| c == 'a'));

        // Emoji (4 bytes) straddling the boundary must also be safe.
        let mut e = "x".repeat(4094);
        e.push('😀'); // 4 bytes, bytes 4094..4098
        let sample = sample_for_classify(&e);
        assert!(sample.len() <= 4096 && sample.is_char_boundary(sample.len()));

        // Short strings pass through unchanged.
        assert_eq!(sample_for_classify("hello"), "hello");
    }

    #[test]
    fn test_url_detection() {
        assert!(matches!(
            classify("https://github.com/user/repo"),
            ClipboardContentType::Url(_)
        ));
        assert!(matches!(
            classify("http://localhost:3000"),
            ClipboardContentType::Url(_)
        ));
        assert!(matches!(
            classify("ftp://files.example.com/data"),
            ClipboardContentType::Url(_)
        ));
    }

    #[test]
    fn test_ip_detection() {
        assert!(matches!(
            classify("192.168.1.1"),
            ClipboardContentType::IpAddress(_)
        ));
        assert!(matches!(
            classify("10.0.0.1"),
            ClipboardContentType::IpAddress(_)
        ));
        assert!(matches!(
            classify("::1"),
            ClipboardContentType::IpAddress(_)
        ));
        assert!(matches!(
            classify("2001:0db8:85a3:0000:0000:8a2e:0370:7334"),
            ClipboardContentType::IpAddress(_)
        ));
    }

    #[test]
    fn test_uuid_detection() {
        assert!(matches!(
            classify("550e8400-e29b-41d4-a716-446655440000"),
            ClipboardContentType::Uuid(_)
        ));
        assert!(matches!(
            classify("01936b7a-e8f2-7abc-9def-0123456789ab"),
            ClipboardContentType::Uuid(_)
        ));
    }

    #[test]
    fn test_git_hash_detection() {
        assert!(matches!(
            classify("abc1234"),
            ClipboardContentType::GitHash(_)
        ));
        assert!(matches!(
            classify("abc1234def5678901234567890abcdef12345678"),
            ClipboardContentType::GitHash(_)
        ));
        // Not a git hash — too short
        assert!(!matches!(
            classify("abc12"),
            ClipboardContentType::GitHash(_)
        ));
        // Not a git hash — contains non-hex
        assert!(!matches!(
            classify("xyz1234"),
            ClipboardContentType::GitHash(_)
        ));
    }

    #[test]
    fn test_json_detection() {
        assert!(matches!(
            classify(r#"{"key": "value"}"#),
            ClipboardContentType::Json
        ));
        assert!(matches!(
            classify(r#"[1, 2, 3]"#),
            ClipboardContentType::Json
        ));
        // Invalid JSON
        assert!(!matches!(
            classify(r#"{key: value}"#),
            ClipboardContentType::Json
        ));
    }

    #[test]
    fn test_error_detection() {
        assert!(matches!(
            classify("Traceback (most recent call last):\n  File \"main.py\", line 5"),
            ClipboardContentType::ErrorTrace(_)
        ));
        assert!(matches!(
            classify("thread 'main' panicked at 'index out of bounds'"),
            ClipboardContentType::ErrorTrace(_)
        ));
        assert!(matches!(
            classify("TypeError: Cannot read properties of undefined"),
            ClipboardContentType::ErrorTrace(_)
        ));
    }

    #[test]
    fn test_error_key_line_extraction() {
        // The payload is the message line, even when preceded by trace noise
        let trace =
            "Traceback (most recent call last):\n  File \"main.py\", line 5\nValueError: bad input";
        match classify(trace) {
            ClipboardContentType::ErrorTrace(msg) => assert_eq!(msg, "ValueError: bad input"),
            other => panic!("expected ErrorTrace, got {other:?}"),
        }
        // Long message lines are truncated with an ellipsis
        let long = format!("TypeError: {}", "x".repeat(200));
        match classify(&long) {
            ClipboardContentType::ErrorTrace(msg) => {
                assert!(msg.chars().count() <= 81);
                assert!(msg.ends_with('…'));
            }
            other => panic!("expected ErrorTrace, got {other:?}"),
        }
    }

    #[test]
    fn test_prose_mentioning_errors_is_plain() {
        // Regression: the old lowercase/substring matcher classified any prose
        // that mentioned "error:" or "at line" as a stack trace.
        assert!(matches!(
            classify("we saw an error: the thing failed at line 3 of the doc"),
            ClipboardContentType::Plain
        ));
        assert!(matches!(
            classify("Error 71 (Protocol error) dispatching to Wayland display."),
            ClipboardContentType::Plain
        ));
        assert!(matches!(
            classify("The error: handling section explains retries."),
            ClipboardContentType::Plain
        ));
    }

    #[test]
    fn test_plain_fallback() {
        assert!(matches!(
            classify("just some normal text"),
            ClipboardContentType::Plain
        ));
        assert!(matches!(
            classify("hello world"),
            ClipboardContentType::Plain
        ));
    }

    #[test]
    fn test_file_path_with_home() {
        // ~/Desktop likely exists on most Linux systems
        let home = std::env::var("HOME").unwrap_or_default();
        if Path::new(&format!("{home}/Desktop")).exists() {
            assert!(matches!(
                classify("~/Desktop"),
                ClipboardContentType::FilePath(_)
            ));
        }
    }

    #[test]
    fn test_root_path() {
        // /tmp always exists
        assert!(matches!(
            classify("/tmp"),
            ClipboardContentType::FilePath(_)
        ));
    }
}
