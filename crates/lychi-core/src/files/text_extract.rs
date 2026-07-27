//! Document → plain-text extraction for the AI "analyze a document" path.
//!
//! The strategy mirrors how Claude/Raycast handle a dropped file: a small plain
//! text/source file is read inline verbatim; a large or binary document (PDF,
//! docx, …) has its *text* extracted so the model sees content, not bytes. This
//! module owns that threshold switch ([`extract_text`]) and the per-format
//! extractors. All pure-Rust, synchronous, testable without Tauri — handlers wrap
//! it in `spawn_blocking`.

use std::io::Read;
use std::path::Path;

use super::{FileKind, classify_path};

/// Files at or below this size that classify as `Text` are read inline verbatim
/// (no extraction step). Above it we still read text files, but truncate — see
/// [`MAX_EXTRACT_BYTES`]. 32 KiB is a few thousand words: enough for most notes,
/// configs, and source files to go to the model whole.
pub const SMALL_INLINE_BYTES: u64 = 32 * 1024;

/// Hard ceiling on extracted text handed back to a caller. Guards the AI request
/// body (and the user's token bill) from a giant document. The extractors stop
/// accumulating once they reach this; callers may note the truncation.
pub const MAX_EXTRACT_BYTES: usize = 256 * 1024;

/// The outcome of extracting text from a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extraction {
    /// The extracted (or inlined) plain text.
    pub text: String,
    /// True when the source was larger than we kept — the text is a prefix.
    pub truncated: bool,
    /// The `FileKind` the source classified as (for the caller's messaging).
    pub kind: FileKind,
}

/// Extract plain text from a document at `path`, applying the threshold switch:
///
/// - `Text`/source files: read verbatim (truncated to [`MAX_EXTRACT_BYTES`]).
/// - `Doc` files (pdf/docx/odt/…): run the format-specific text extractor.
/// - `Image`: refused here — images go through the vision path, not text.
/// - `Archive`/`Other`: refused with a helpful message.
///
/// Best-effort and non-panicking: any extractor failure becomes an `Err(String)`
/// the caller can surface.
pub fn extract_text(path: &Path) -> Result<Extraction, String> {
    let ft = classify_path(path);
    match ft.kind {
        FileKind::Text => read_text_file(path, FileKind::Text),
        FileKind::Doc => extract_doc(path, &ft.mime),
        FileKind::Image => {
            Err("That's an image — analyze it with the vision path, not text extraction.".into())
        }
        FileKind::Archive => {
            Err("That's an archive — extract it first, then analyze the files inside.".into())
        }
        FileKind::Other => Err(format!(
            "Don't know how to read text from this file ({}).",
            ft.mime
        )),
    }
}

/// Read a plain-text/source file, capping at [`MAX_EXTRACT_BYTES`]. Lossy UTF-8 so
/// a stray non-UTF-8 byte doesn't fail the whole read.
fn read_text_file(path: &Path, kind: FileKind) -> Result<Extraction, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("Couldn't open file: {e}"))?;
    let mut buf = Vec::new();
    // Read one byte past the cap so we can tell whether truncation happened.
    let limit = MAX_EXTRACT_BYTES as u64 + 1;
    f.take(limit)
        .read_to_end(&mut buf)
        .map_err(|e| format!("Couldn't read file: {e}"))?;
    let truncated = buf.len() > MAX_EXTRACT_BYTES;
    buf.truncate(MAX_EXTRACT_BYTES);
    let text = String::from_utf8_lossy(&buf).into_owned();
    Ok(Extraction {
        text,
        truncated,
        kind,
    })
}

/// Dispatch a `Doc` file to its format extractor by MIME.
fn extract_doc(path: &Path, mime: &str) -> Result<Extraction, String> {
    let text = match mime {
        "application/pdf" => extract_pdf(path)?,
        // Office Open XML (docx) + OpenDocument (odt) + presentations (pptx).
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            extract_ooxml(path, OoxmlKind::Docx)?
        }
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            extract_ooxml(path, OoxmlKind::Pptx)?
        }
        "application/vnd.oasis.opendocument.text" => extract_ooxml(path, OoxmlKind::Odt)?,
        other => {
            // Fall back on extension for the name-only cases (classify_path may
            // hand us the guessed MIME without magic bytes).
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            match ext.as_str() {
                "pdf" => extract_pdf(path)?,
                "docx" => extract_ooxml(path, OoxmlKind::Docx)?,
                "pptx" => extract_ooxml(path, OoxmlKind::Pptx)?,
                "odt" => extract_ooxml(path, OoxmlKind::Odt)?,
                _ => return Err(format!("No text extractor for document type {other}.")),
            }
        }
    };
    let truncated = text.len() > MAX_EXTRACT_BYTES;
    let text = cap(text);
    Ok(Extraction {
        text,
        truncated,
        kind: FileKind::Doc,
    })
}

/// Truncate to [`MAX_EXTRACT_BYTES`] on a char boundary (never split a UTF-8
/// codepoint — that would panic).
fn cap(mut s: String) -> String {
    if s.len() <= MAX_EXTRACT_BYTES {
        return s;
    }
    let mut end = MAX_EXTRACT_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s
}

/// Which OOXML/ODF flavor we're pulling text from — selects the inner XML part(s)
/// and the text element to collect.
#[derive(Debug, Clone, Copy)]
enum OoxmlKind {
    /// `word/document.xml`, text in `<w:t>`.
    Docx,
    /// `ppt/slides/slideN.xml`, text in `<a:t>`.
    Pptx,
    /// `content.xml`, text in `<text:p>`/`<text:span>` (we collect all text nodes).
    Odt,
}

/// Extract visible text from a ZIP-container document (docx/pptx/odt). Reads the
/// relevant XML part(s) and concatenates the text runs. Streaming pull-parser, so
/// a large document never fully materializes its XML tree.
fn extract_ooxml(path: &Path, kind: OoxmlKind) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Couldn't open document: {e}"))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| format!("Not a valid document container: {e}"))?;

    // The XML parts to read, in order. Pptx has one per slide; enumerate them.
    let parts: Vec<String> = match kind {
        OoxmlKind::Docx => vec!["word/document.xml".to_string()],
        OoxmlKind::Odt => vec!["content.xml".to_string()],
        OoxmlKind::Pptx => {
            let mut slides: Vec<String> = (0..zip.len())
                .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
                .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
                .collect();
            // slide1, slide2, … in numeric order (lexical "slide10" < "slide2").
            slides.sort_by_key(|n| slide_number(n));
            slides
        }
    };

    // The text-carrying element for this flavor.
    let text_tag: &[u8] = match kind {
        OoxmlKind::Docx => b"w:t",
        OoxmlKind::Pptx => b"a:t",
        OoxmlKind::Odt => b"text:p", // collected specially (see below)
    };

    let mut out = String::new();
    for part in parts {
        let mut xml = String::new();
        match zip.by_name(&part) {
            Ok(mut f) => {
                f.read_to_string(&mut xml)
                    .map_err(|e| format!("Couldn't read {part}: {e}"))?;
            }
            Err(_) => continue, // part absent — skip (empty slide, etc.)
        }
        collect_xml_text(&xml, text_tag, matches!(kind, OoxmlKind::Odt), &mut out);
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if out.len() > MAX_EXTRACT_BYTES {
            break;
        }
    }
    let out = out.trim().to_string();
    if out.is_empty() {
        return Err("The document has no extractable text.".into());
    }
    Ok(out)
}

/// The slide's numeric index from a `ppt/slides/slideN.xml` name (for ordering).
fn slide_number(name: &str) -> u32 {
    name.trim_start_matches("ppt/slides/slide")
        .trim_end_matches(".xml")
        .parse()
        .unwrap_or(0)
}

/// Pull-parse `xml`, appending the text content of every `text_tag` element to
/// `out`. When `all_text` is true (ODT), collect text from ALL elements (ODT
/// nests runs under many tags) and insert a newline at each paragraph boundary.
fn collect_xml_text(xml: &str, text_tag: &[u8], all_text: bool, out: &mut String) {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut depth_in_tag = 0u32;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if e.name().as_ref() == text_tag {
                    depth_in_tag += 1;
                } else if all_text && e.name().as_ref() == b"text:p" {
                    // ODT paragraph — ensure a newline separates it.
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == text_tag && depth_in_tag > 0 {
                    depth_in_tag -= 1;
                }
            }
            Ok(Event::Text(t)) => {
                // For docx/pptx: only text inside <w:t>/<a:t>. For ODT: all text
                // (the whole content.xml body is prose).
                if (all_text || depth_in_tag > 0)
                    && let Ok(s) = t.unescape()
                {
                    out.push_str(s.as_ref());
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break, // malformed XML — stop, keep what we have
            _ => {}
        }
        if out.len() > MAX_EXTRACT_BYTES {
            break;
        }
    }
}

/// Extract text from a PDF. Backend chosen by Phase-3 research (pending); this
/// seam keeps the dispatch above stable while the crate is decided.
fn extract_pdf(path: &Path) -> Result<String, String> {
    pdf_backend::extract(path)
}

/// PDF extraction backend, isolated so swapping the crate touches one module.
///
/// Uses `pdf-extract` (pure-Rust, no native libs). That crate is known to PANIC
/// on some malformed/unusual PDFs rather than returning an error, so every call
/// is wrapped in `catch_unwind` — a bad PDF becomes an `Err`, never a crash.
mod pdf_backend {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::Path;

    pub fn extract(path: &Path) -> Result<String, String> {
        // Guard against pdf-extract's panic-on-malformed-input paths.
        let result = catch_unwind(AssertUnwindSafe(|| pdf_extract::extract_text(path)));
        match result {
            Ok(Ok(text)) if !text.trim().is_empty() => Ok(text),
            Ok(Ok(_)) => Err(
                "The PDF has no extractable text (it may be scanned images — try image analysis)."
                    .into(),
            ),
            Ok(Err(e)) => Err(format!("Couldn't read the PDF: {e}")),
            Err(_) => Err("Couldn't read the PDF (the file appears to be malformed).".into()),
        }
    }
}

/// A document reference found in an AI prompt: the original `@token` and the
/// resolved absolute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocRef {
    /// The verbatim token as it appeared in the prompt (e.g. `@~/report.pdf`).
    pub token: String,
    /// The tilde-expanded path.
    pub path: std::path::PathBuf,
}

/// Scan a prompt for `@`-referenced paths that resolve to a **document** on disk
/// (`FileKind::Doc`). Text/image/archive/`Other` refs are ignored here — text
/// files inline fine as paths, images go through the vision param, and the rest
/// aren't analyzable as text. Mirrors the executor's `@`-token rules (a `@` that
/// begins a token and is followed by a path-like char; tilde-expanded).
pub fn find_doc_refs(prompt: &str) -> Vec<DocRef> {
    if !prompt.contains('@') {
        return Vec::new();
    }
    let mut refs = Vec::new();
    for tok in prompt.split_whitespace() {
        let Some(rest) = tok.strip_prefix('@') else {
            continue;
        };
        let looks_like_path = rest
            .chars()
            .next()
            .is_some_and(|c| c == '~' || c == '/' || c == '.' || c.is_alphanumeric());
        if !looks_like_path {
            continue;
        }
        let path = super::paths::expand_home(rest);
        if path.is_file() && classify_path(&path).kind == FileKind::Doc {
            refs.push(DocRef {
                token: tok.to_string(),
                path,
            });
        }
    }
    refs
}

/// A non-file `@`-reference that pulls in ambient context: `@clipboard` and
/// `@selection`. These name a SOURCE rather than a path, so they resolve through
/// a supplied reader instead of the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextSource {
    /// What's on the clipboard right now (Ctrl+C).
    Clipboard,
    /// What's highlighted in the focused window (PRIMARY selection).
    Selection,
}

impl ContextSource {
    /// The label shown on the inlined block.
    fn label(self) -> &'static str {
        match self {
            Self::Clipboard => "clipboard",
            Self::Selection => "selection",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        match word.to_ascii_lowercase().as_str() {
            "clipboard" | "clip" => Some(Self::Clipboard),
            "selection" | "selected" => Some(Self::Selection),
            _ => None,
        }
    }
}

/// Find `@clipboard` / `@selection` tokens in a prompt, in order, de-duplicated.
/// Matched exactly (not as a path prefix) so a real file named `clipboard.txt`
/// still resolves as a document.
pub fn find_context_refs(prompt: &str) -> Vec<(String, ContextSource)> {
    if !prompt.contains('@') {
        return Vec::new();
    }
    let mut out: Vec<(String, ContextSource)> = Vec::new();
    for tok in prompt.split_whitespace() {
        let Some(rest) = tok.strip_prefix('@') else {
            continue;
        };
        // Trailing punctuation is common in prose ("summarize @clipboard.").
        let word = rest.trim_end_matches(['.', ',', '?', '!']);
        if let Some(src) = ContextSource::parse(word)
            && !out.iter().any(|(_, s)| *s == src)
        {
            out.push((tok.to_string(), src));
        }
    }
    out
}

/// Expand `@clipboard` / `@selection` into inlined text, using `read` to fetch
/// each source. Kept separate from the filesystem so core stays testable without
/// a real clipboard — the caller supplies the reader.
///
/// A source that's empty or unreadable is reported inline rather than dropped,
/// so the model doesn't silently answer about nothing.
pub fn expand_context_refs(prompt: &str, read: &dyn Fn(ContextSource) -> Option<String>) -> String {
    let refs = find_context_refs(prompt);
    if refs.is_empty() {
        return prompt.to_string();
    }

    let mut instruction = prompt.to_string();
    for (token, _) in &refs {
        instruction = instruction.replace(token, "");
    }
    // Collapse the gaps left by removed tokens.
    let instruction = instruction.split_whitespace().collect::<Vec<_>>().join(" ");

    let mut out = if instruction.is_empty() {
        String::new()
    } else {
        format!("{instruction}\n")
    };
    for (_, src) in &refs {
        let label = src.label();
        match read(*src).filter(|s| !s.trim().is_empty()) {
            Some(body) => {
                let original_len = body.len();
                let capped = cap(body);
                let trunc = if capped.len() < original_len {
                    " (truncated)"
                } else {
                    ""
                };
                out.push_str(&format!("\n--- {label}{trunc} ---\n{capped}\n"));
            }
            None => {
                out.push_str(&format!("\n--- {label} ---\n[nothing in the {label}]\n"));
            }
        }
    }
    out.trim_end().to_string()
}

/// Expand any `@`-referenced documents in `prompt` into inlined text for the AI:
/// each doc's `@token` is removed from the sentence and its extracted text is
/// appended as a fenced, labeled block. Returns the rewritten prompt unchanged
/// when there are no doc refs. Extraction failures are surfaced inline (a short
/// note) rather than dropped silently, so the model — and the user — know a
/// document couldn't be read.
///
/// Pure aside from reading the referenced files; call from `spawn_blocking`.
pub fn expand_doc_refs(prompt: &str) -> String {
    let refs = find_doc_refs(prompt);
    if refs.is_empty() {
        return prompt.to_string();
    }
    // Drop the `@doc` tokens from the instruction line (their content moves below).
    let mut instruction = prompt.to_string();
    for r in &refs {
        instruction = instruction.replace(&r.token, "").replace("  ", " ");
    }
    let instruction = instruction.trim().to_string();

    let mut out = if instruction.is_empty() {
        String::new()
    } else {
        format!("{instruction}\n")
    };
    for r in &refs {
        let name = r
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("document");
        match extract_text(&r.path) {
            Ok(ex) => {
                let trunc = if ex.truncated { " (truncated)" } else { "" };
                out.push_str(&format!("\n--- {name}{trunc} ---\n{}\n", ex.text));
            }
            Err(e) => {
                out.push_str(&format!(
                    "\n--- {name} ---\n[couldn't read this document: {e}]\n"
                ));
            }
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("lychi-textextract-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn small_text_file_is_inlined() {
        let p = tmp("notes.md");
        std::fs::write(&p, "# Title\n\nhello world").unwrap();
        let ex = extract_text(&p).unwrap();
        assert_eq!(ex.kind, FileKind::Text);
        assert!(!ex.truncated);
        assert!(ex.text.contains("hello world"));
    }

    #[test]
    fn oversized_text_file_is_truncated() {
        let p = tmp("big.txt");
        let big = "x".repeat(MAX_EXTRACT_BYTES + 5000);
        std::fs::write(&p, &big).unwrap();
        let ex = extract_text(&p).unwrap();
        assert!(ex.truncated);
        assert_eq!(ex.text.len(), MAX_EXTRACT_BYTES);
    }

    #[test]
    fn image_is_refused_for_text() {
        let p = tmp("pic.png");
        let img = image::RgbImage::from_pixel(4, 4, image::Rgb([1, 2, 3]));
        image::DynamicImage::ImageRgb8(img).save(&p).unwrap();
        assert!(extract_text(&p).is_err());
    }

    #[test]
    fn docx_text_is_extracted() {
        // Build a minimal valid .docx (a ZIP with word/document.xml).
        let p = tmp("doc.docx");
        let f = std::fs::File::create(&p).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zw.start_file("word/document.xml", opts).unwrap();
        zw.write_all(
            br#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t> Docx</w:t></w:r></w:p></w:body></w:document>"#,
        )
        .unwrap();
        zw.finish().unwrap();

        let ex = extract_text(&p).unwrap();
        assert_eq!(ex.kind, FileKind::Doc);
        assert_eq!(ex.text, "Hello Docx");
    }

    #[test]
    fn docx_without_text_errors() {
        let p = tmp("empty.docx");
        let f = std::fs::File::create(&p).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zw.start_file("word/document.xml", opts).unwrap();
        zw.write_all(
            br#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body></w:body></w:document>"#,
        )
        .unwrap();
        zw.finish().unwrap();
        assert!(extract_text(&p).is_err());
    }

    #[test]
    fn slide_number_orders_numerically() {
        assert_eq!(slide_number("ppt/slides/slide2.xml"), 2);
        assert_eq!(slide_number("ppt/slides/slide10.xml"), 10);
    }

    // Write a minimal valid .docx with the given body text; return its path.
    fn docx_with(name: &str, body: &str) -> std::path::PathBuf {
        let p = tmp(name);
        let f = std::fs::File::create(&p).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zw.start_file("word/document.xml", opts).unwrap();
        let xml = format!(
            r#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body><w:p><w:r><w:t>{body}</w:t></w:r></w:p></w:body></w:document>"#
        );
        zw.write_all(xml.as_bytes()).unwrap();
        zw.finish().unwrap();
        p
    }

    #[test]
    fn find_doc_refs_only_matches_documents() {
        let doc = docx_with("ref.docx", "content");
        let prompt = format!("summarize @{} please", doc.display());
        let refs = find_doc_refs(&prompt);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, doc);
        // A non-doc @ref (nonexistent path) is ignored.
        assert!(find_doc_refs("hello @world and email a@b.com").is_empty());
    }

    #[test]
    fn expand_doc_refs_inlines_extracted_text() {
        let doc = docx_with("summary.docx", "Quarterly numbers up 20%.");
        let prompt = format!("summarize @{}", doc.display());
        let expanded = expand_doc_refs(&prompt);
        // Instruction kept, @token stripped, doc text fenced in below.
        assert!(expanded.starts_with("summarize"));
        assert!(!expanded.contains('@'));
        assert!(expanded.contains("--- summary.docx ---"));
        assert!(expanded.contains("Quarterly numbers up 20%."));
    }

    #[test]
    fn expand_doc_refs_noop_without_refs() {
        assert_eq!(expand_doc_refs("just a question"), "just a question");
    }

    // Build a minimal, byte-correct single-page PDF drawing `text` with Helvetica.
    // Computes the xref byte offsets from the actual assembled bytes so the table
    // is always valid (pdf-extract rejects a wrong xref). No external fixture.
    fn minimal_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 24 Tf 100 700 Td ({text}) Tj ET");
        let bodies: [String; 5] = [
            "<</Type/Catalog/Pages 2 0 R>>".into(),
            "<</Type/Pages/Kids[3 0 R]/Count 1>>".into(),
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>".into(),
            format!("<</Length {}>>stream\n{stream}\nendstream", stream.len()),
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".into(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in bodies.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj{body}endobj\n", i + 1).as_bytes());
        }
        let xref_pos = out.len();
        let n = bodies.len() + 1;
        out.extend_from_slice(format!("xref\n0 {n}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer<</Size {n}/Root 1 0 R>>\nstartxref\n{xref_pos}\n%%EOF").as_bytes(),
        );
        out
    }

    #[test]
    fn pdf_text_is_extracted() {
        let p = tmp("doc.pdf");
        std::fs::write(&p, minimal_pdf("Hello PDF")).unwrap();

        let ex = extract_text(&p).unwrap();
        assert_eq!(ex.kind, FileKind::Doc);
        assert!(
            ex.text.contains("Hello PDF"),
            "expected extracted text to contain 'Hello PDF', got: {:?}",
            ex.text
        );
    }

    #[test]
    fn malformed_pdf_errors_without_panicking() {
        // Garbage that only looks like a PDF by extension — must degrade to Err,
        // never crash (pdf-extract can panic; we catch_unwind it).
        let p = tmp("broken.pdf");
        std::fs::write(&p, b"%PDF-1.4\nnot a real pdf at all \x00\x01\x02").unwrap();
        assert!(extract_text(&p).is_err());
    }

    /// A reader that answers with canned text, so the expansion is testable
    /// without a real clipboard or X display.
    fn reader(
        clip: Option<&'static str>,
        sel: Option<&'static str>,
    ) -> impl Fn(ContextSource) -> Option<String> {
        move |src| match src {
            ContextSource::Clipboard => clip.map(str::to_string),
            ContextSource::Selection => sel.map(str::to_string),
        }
    }

    #[test]
    fn context_refs_are_found_and_deduped() {
        let refs = find_context_refs("summarize @clipboard and @selection and @clipboard again");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].1, ContextSource::Clipboard);
        assert_eq!(refs[1].1, ContextSource::Selection);
    }

    #[test]
    fn trailing_punctuation_still_matches() {
        let refs = find_context_refs("what is @clipboard?");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].1, ContextSource::Clipboard);
    }

    #[test]
    fn a_file_named_clipboard_is_not_a_context_ref() {
        // Exact-match only — `@clipboard.txt` is a path, not the clipboard.
        assert!(find_context_refs("read @clipboard.txt").is_empty());
    }

    #[test]
    fn context_expansion_inlines_the_body_and_strips_the_token() {
        let out = expand_context_refs("summarize @clipboard", &reader(Some("hello world"), None));
        assert!(out.starts_with("summarize\n"));
        assert!(!out.contains("@clipboard"));
        assert!(out.contains("--- clipboard ---"));
        assert!(out.contains("hello world"));
    }

    #[test]
    fn an_empty_source_is_reported_not_silently_dropped() {
        // Answering about nothing is worse than saying there was nothing.
        let out = expand_context_refs("summarize @selection", &reader(None, None));
        assert!(out.contains("[nothing in the selection]"));

        let blank = expand_context_refs("summarize @clipboard", &reader(Some("   "), None));
        assert!(blank.contains("[nothing in the clipboard]"));
    }

    #[test]
    fn a_prompt_without_context_refs_is_returned_verbatim() {
        let p = "what is rust?";
        assert_eq!(expand_context_refs(p, &reader(Some("x"), None)), p);
    }

    #[test]
    fn both_sources_expand_in_order() {
        let out = expand_context_refs(
            "compare @clipboard with @selection",
            &reader(Some("AAA"), Some("BBB")),
        );
        let clip_at = out.find("AAA").unwrap();
        let sel_at = out.find("BBB").unwrap();
        assert!(clip_at < sel_at);
        assert!(out.starts_with("compare with\n"));
    }

    #[test]
    fn cap_respects_char_boundary() {
        // A string of multi-byte chars just over the cap must not panic and must
        // stay valid UTF-8.
        let s = "é".repeat(MAX_EXTRACT_BYTES); // 2 bytes each → well over the cap
        let out = cap(s);
        assert!(out.len() <= MAX_EXTRACT_BYTES);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }
}
