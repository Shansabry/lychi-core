//! Reading COPIED FILES from the clipboard — Ctrl+C on a file in Nautilus,
//! Dolphin, Thunar, or any file manager.
//!
//! This is distinct from the two things the clipboard brick already handles:
//! copied *text* and copied *image data*. A file copy puts neither on the
//! clipboard — it puts a list of `file://` URIs under a dedicated MIME type, and
//! the bytes stay on disk. So this reader returns PATHS, which the attachment
//! layer then classifies exactly like any other attached file
//! (`files::attachment`) — no second notion of what a file is.
//!
//! MIME types, in preference order:
//!   - `x-special/gnome-copied-files` — GTK (Nautilus, Thunar, Nemo). First line
//!     is the verb (`copy`/`cut`), the rest are URIs.
//!   - `text/uri-list` — the freedesktop standard (Dolphin and most others).
//!     `#`-prefixed lines are comments per the spec.
//!
//! Read-only and lazy, like `selection`: called on an explicit paste gesture,
//! never polled. Wayland uses `wl-paste`, X11 `xclip`; the other is tried as a
//! fallback regardless, since XWayland apps still answer X11 requests.

use std::path::PathBuf;
use std::process::Command;

/// Cap on how many files one paste can stage, so a Ctrl+A in a huge folder
/// can't flood the attachment tray.
const MAX_FILES: usize = 25;

/// The MIME types a file manager uses to advertise a file copy.
const FILE_MIMES: [&str; 2] = ["x-special/gnome-copied-files", "text/uri-list"];

/// Read file paths currently copied to the clipboard, in order. Returns an empty
/// vec when the clipboard holds text/an image/nothing — i.e. "no files here" is
/// not an error. Paths are returned as-is (existence is the caller's concern;
/// `files::attachment` already reports unreadable files as a chip with a note).
pub fn read_clipboard_files(is_wayland: bool) -> Vec<PathBuf> {
    let tools: [&str; 2] = if is_wayland {
        ["wl-paste", "xclip"]
    } else {
        ["xclip", "wl-paste"]
    };
    for tool in tools {
        for mime in FILE_MIMES {
            if let Some(raw) = read_mime(tool, mime) {
                let paths = parse_uri_list(&raw);
                if !paths.is_empty() {
                    return paths;
                }
            }
        }
    }
    Vec::new()
}

/// Ask one tool for one MIME type. `None` when the tool is missing, the target
/// isn't offered, or the payload is empty.
fn read_mime(tool: &str, mime: &str) -> Option<String> {
    let output = match tool {
        "wl-paste" => Command::new("wl-paste")
            .args(["--no-newline", "--type", mime])
            .output()
            .ok()?,
        "xclip" => Command::new("xclip")
            .args(["-selection", "clipboard", "-t", mime, "-o"])
            .output()
            .ok()?,
        _ => return None,
    };
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Parse a URI list into local paths.
///
/// Handles both payload shapes: the GNOME variant's leading `copy`/`cut` verb
/// line, and the uri-list spec's `#` comments. Non-`file://` URIs (http, trash,
/// smb) are skipped — we can only attach something on the local disk.
pub fn parse_uri_list(raw: &str) -> Vec<PathBuf> {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        // The GNOME verb line and uri-list comments are not URIs.
        .filter(|l| !l.starts_with('#') && *l != "copy" && *l != "cut")
        .filter_map(uri_to_path)
        .take(MAX_FILES)
        .collect()
}

/// Convert one `file://` URI to a path, percent-decoding it. Returns `None` for
/// any other scheme.
fn uri_to_path(line: &str) -> Option<PathBuf> {
    let rest = line.strip_prefix("file://")?;
    // `file:///home/x` → authority is empty, path starts at the third slash.
    // A non-empty authority (a remote host) isn't a local file.
    let stripped = rest.strip_prefix('/')?;
    Some(PathBuf::from(percent_decode(&format!("/{stripped}"))))
}

/// Decode `%XX` escapes. File managers percent-encode spaces and non-ASCII, so
/// `My%20Report.pdf` must become `My Report.pdf` or the path won't resolve.
/// Byte-wise (not char-wise) so multi-byte UTF-8 escapes reassemble correctly.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(b) = hex {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gnome_copied_files_with_verb_line() {
        let raw = "copy\nfile:///home/sab/a.png\nfile:///home/sab/b.pdf";
        let paths = parse_uri_list(raw);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/home/sab/a.png"),
                PathBuf::from("/home/sab/b.pdf")
            ]
        );
    }

    #[test]
    fn cut_verb_is_also_stripped() {
        let paths = parse_uri_list("cut\nfile:///tmp/x.txt");
        assert_eq!(paths, vec![PathBuf::from("/tmp/x.txt")]);
    }

    #[test]
    fn parses_plain_uri_list_ignoring_comments() {
        let raw = "# a comment\nfile:///tmp/one.txt\n\nfile:///tmp/two.txt";
        assert_eq!(parse_uri_list(raw).len(), 2);
    }

    #[test]
    fn percent_escapes_are_decoded() {
        let paths = parse_uri_list("file:///home/sab/My%20Report%20(final).pdf");
        assert_eq!(
            paths,
            vec![PathBuf::from("/home/sab/My Report (final).pdf")]
        );
    }

    #[test]
    fn multibyte_escapes_reassemble() {
        // "café.txt" — é is two bytes (C3 A9), so decoding must be byte-wise.
        let paths = parse_uri_list("file:///tmp/caf%C3%A9.txt");
        assert_eq!(paths, vec![PathBuf::from("/tmp/café.txt")]);
    }

    #[test]
    fn non_file_schemes_are_skipped() {
        let raw = "https://example.com/x.png\ntrash:///thing\nfile:///tmp/real.txt";
        assert_eq!(parse_uri_list(raw), vec![PathBuf::from("/tmp/real.txt")]);
    }

    #[test]
    fn remote_authority_is_not_a_local_file() {
        assert!(parse_uri_list("file://otherhost/share/x.txt").is_empty());
    }

    #[test]
    fn plain_text_clipboard_yields_no_files() {
        // Text that merely mentions a path is not a file copy.
        assert!(parse_uri_list("just some copied prose\n/home/sab/notes.md").is_empty());
    }

    #[test]
    fn a_malformed_escape_is_left_alone_not_dropped() {
        // `%zz` isn't valid hex — keep the literal rather than mangling the path.
        let paths = parse_uri_list("file:///tmp/100%zz.txt");
        assert_eq!(paths, vec![PathBuf::from("/tmp/100%zz.txt")]);
    }

    #[test]
    fn the_batch_is_capped() {
        let raw: String = (0..100)
            .map(|i| format!("file:///tmp/f{i}.txt\n"))
            .collect();
        assert_eq!(parse_uri_list(&raw).len(), MAX_FILES);
    }
}
