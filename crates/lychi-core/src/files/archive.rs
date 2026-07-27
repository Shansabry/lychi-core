//! Archive operations — create/extract zip and tar.gz. Pure, synchronous;
//! handlers wrap these in `spawn_blocking`.
//!
//! Security: extraction is guarded against zip-slip (path traversal via `..` or
//! absolute entries) with `guard_under`, rejects symlink entries, and enforces
//! per-file and total uncompressed-size caps (zip-bomb defense). We use the
//! maintained `zip` crate (>= 2.3.0, past the RUSTSEC symlink zip-slip) but never
//! trust its extraction — every entry is re-checked here.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use zip::write::SimpleFileOptions;

use super::paths::guard_under;

/// Per-entry uncompressed cap (500 MB) and total cap (2 GB) — a pragmatic
/// zip-bomb guard. Legitimate archives a launcher extracts are far smaller.
const MAX_ENTRY_BYTES: u64 = 500 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The result of an extraction: where it landed and how many files were written.
#[derive(Debug, Clone)]
pub struct Extracted {
    pub dest: PathBuf,
    pub files: usize,
}

/// Zip one or more input paths (files or directories) into `out_zip`.
/// Directories are added recursively with paths relative to their parent.
pub fn zip_paths(inputs: &[PathBuf], out_zip: &Path) -> Result<PathBuf, String> {
    if inputs.is_empty() {
        return Err("Nothing to zip".to_string());
    }
    let file =
        File::create(out_zip).map_err(|e| format!("Couldn't create {}: {e}", out_zip.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for input in inputs {
        if !input.exists() {
            return Err(format!("No such path: {}", input.display()));
        }
        let base = input.parent().unwrap_or(Path::new(""));
        if input.is_dir() {
            add_dir_recursive(&mut zip, input, base, opts)?;
        } else {
            let name = input
                .strip_prefix(base)
                .unwrap_or(input)
                .to_string_lossy()
                .into_owned();
            zip.start_file(name, opts)
                .map_err(|e| format!("zip error: {e}"))?;
            let mut f = File::open(input).map_err(|e| format!("read error: {e}"))?;
            io::copy(&mut f, &mut zip).map_err(|e| format!("zip write error: {e}"))?;
        }
    }
    zip.finish()
        .map_err(|e| format!("zip finalize error: {e}"))?;
    Ok(out_zip.to_path_buf())
}

fn add_dir_recursive(
    zip: &mut zip::ZipWriter<File>,
    dir: &Path,
    base: &Path,
    opts: SimpleFileOptions,
) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("read dir error: {e}"))? {
        let entry = entry.map_err(|e| format!("read dir error: {e}"))?;
        let path = entry.path();
        let rel = path
            .strip_prefix(base)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        if path.is_dir() {
            add_dir_recursive(zip, &path, base, opts)?;
        } else {
            zip.start_file(rel, opts)
                .map_err(|e| format!("zip error: {e}"))?;
            let mut f = File::open(&path).map_err(|e| format!("read error: {e}"))?;
            io::copy(&mut f, zip).map_err(|e| format!("zip write error: {e}"))?;
        }
    }
    Ok(())
}

/// Detect the archive kind by extension and extract into `dest_dir` (created if
/// absent). Supports `.zip`, `.tar.gz`/`.tgz`, and plain `.tar`.
pub fn extract_archive(src: &Path, dest_dir: &Path) -> Result<Extracted, String> {
    fs::create_dir_all(dest_dir)
        .map_err(|e| format!("Couldn't create {}: {e}", dest_dir.display()))?;
    // Canonicalize the root once; entries are checked against this.
    let dest = dest_dir
        .canonicalize()
        .map_err(|e| format!("Couldn't resolve {}: {e}", dest_dir.display()))?;

    let name = src.to_string_lossy().to_ascii_lowercase();
    if name.ends_with(".zip") {
        extract_zip(src, &dest)
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let file = File::open(src).map_err(|e| format!("Couldn't open {}: {e}", src.display()))?;
        extract_tar(GzDecoder::new(file), &dest)
    } else if name.ends_with(".tar") {
        let file = File::open(src).map_err(|e| format!("Couldn't open {}: {e}", src.display()))?;
        extract_tar(file, &dest)
    } else {
        Err(format!(
            "Unsupported archive: {}. Supported: .zip, .tar.gz, .tgz, .tar",
            src.display()
        ))
    }
}

fn extract_zip(src: &Path, dest: &Path) -> Result<Extracted, String> {
    let file = File::open(src).map_err(|e| format!("Couldn't open {}: {e}", src.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("Bad zip: {e}"))?;
    let mut written = 0usize;
    let mut total: u64 = 0;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| format!("zip entry error: {e}"))?;

        // Use the crate's own zip-slip-safe name; None ⇒ absolute/`..` escape.
        let Some(rel) = entry.enclosed_name() else {
            return Err(format!("Refusing unsafe archive entry: {}", entry.name()));
        };
        // Re-verify against our root (defense in depth).
        let Some(target) = guard_under(dest, &rel) else {
            return Err(format!(
                "Refusing entry outside destination: {}",
                entry.name()
            ));
        };

        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|e| format!("mkdir error: {e}"))?;
            continue;
        }
        // Reject anything that isn't a regular file (e.g. a symlink entry).
        if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err(format!("Refusing symlink entry: {}", entry.name()));
        }

        let size = entry.size();
        if size > MAX_ENTRY_BYTES {
            return Err(format!("Entry too large: {} ({size} bytes)", entry.name()));
        }
        total = total.saturating_add(size);
        if total > MAX_TOTAL_BYTES {
            return Err("Archive exceeds the total size limit".to_string());
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir error: {e}"))?;
        }
        let mut out = File::create(&target).map_err(|e| format!("write error: {e}"))?;
        io::copy(&mut entry, &mut out).map_err(|e| format!("write error: {e}"))?;
        written += 1;
    }
    Ok(Extracted {
        dest: dest.to_path_buf(),
        files: written,
    })
}

fn extract_tar<R: Read>(reader: R, dest: &Path) -> Result<Extracted, String> {
    let mut archive = tar::Archive::new(reader);
    let mut written = 0usize;
    let mut total: u64 = 0;

    let entries = archive.entries().map_err(|e| format!("Bad tar: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("tar entry error: {e}"))?;
        // Reject symlinks/hardlinks and special entries — extract regular files/dirs only.
        let etype = entry.header().entry_type();
        let path = entry
            .path()
            .map_err(|e| format!("tar path error: {e}"))?
            .into_owned();

        let Some(target) = guard_under(dest, &path) else {
            return Err(format!(
                "Refusing entry outside destination: {}",
                path.display()
            ));
        };

        if etype.is_dir() {
            fs::create_dir_all(&target).map_err(|e| format!("mkdir error: {e}"))?;
            continue;
        }
        if !etype.is_file() {
            // symlink/hardlink/fifo/char/block — skip (a launcher never needs these,
            // and links are the tar zip-slip vector).
            return Err(format!(
                "Refusing non-regular tar entry: {}",
                path.display()
            ));
        }

        let size = entry.header().size().unwrap_or(0);
        if size > MAX_ENTRY_BYTES {
            return Err(format!(
                "Entry too large: {} ({size} bytes)",
                path.display()
            ));
        }
        total = total.saturating_add(size);
        if total > MAX_TOTAL_BYTES {
            return Err("Archive exceeds the total size limit".to_string());
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir error: {e}"))?;
        }
        let mut out = File::create(&target).map_err(|e| format!("write error: {e}"))?;
        io::copy(&mut entry, &mut out).map_err(|e| format!("write error: {e}"))?;
        written += 1;
    }
    Ok(Extracted {
        dest: dest.to_path_buf(),
        files: written,
    })
}

/// Create a `.tar.gz` from input paths (used when the requested output ends in
/// `.tar.gz`/`.tgz`). Kept minimal; zip is the default archive format.
pub fn targz_paths(inputs: &[PathBuf], out: &Path) -> Result<PathBuf, String> {
    let file = File::create(out).map_err(|e| format!("Couldn't create {}: {e}", out.display()))?;
    let enc = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(enc);
    for input in inputs {
        let base = input.parent().unwrap_or(Path::new(""));
        let name = input.strip_prefix(base).unwrap_or(input);
        if input.is_dir() {
            builder
                .append_dir_all(name, input)
                .map_err(|e| format!("tar error: {e}"))?;
        } else {
            let mut f = File::open(input).map_err(|e| format!("read error: {e}"))?;
            builder
                .append_file(name, &mut f)
                .map_err(|e| format!("tar error: {e}"))?;
        }
    }
    let enc = builder
        .into_inner()
        .map_err(|e| format!("tar finalize error: {e}"))?;
    enc.finish()
        .map_err(|e| format!("gzip finalize error: {e}"))?;
    Ok(out.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lychi_archtest_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn zip_then_extract_roundtrip() {
        let work = tmp("roundtrip");
        let a = work.join("a.txt");
        fs::write(&a, b"hello").unwrap();
        let b = work.join("b.txt");
        fs::write(&b, b"world").unwrap();

        let zpath = work.join("out.zip");
        zip_paths(&[a.clone(), b.clone()], &zpath).unwrap();
        assert!(zpath.exists());

        let dest = work.join("unpacked");
        let res = extract_archive(&zpath, &dest).unwrap();
        assert_eq!(res.files, 2);
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"hello");

        fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn extract_refuses_zip_slip() {
        let work = tmp("zipslip");
        // Hand-craft a zip whose entry name escapes the destination.
        let zpath = work.join("evil.zip");
        {
            let file = File::create(&zpath).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            // A raw name with `..` — enclosed_name() must reject it.
            zip.start_file("../../evil.txt", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"pwned").unwrap();
            zip.finish().unwrap();
        }
        let dest = work.join("out");
        let err = extract_archive(&zpath, &dest).unwrap_err();
        assert!(err.to_lowercase().contains("unsafe") || err.to_lowercase().contains("outside"));
        // Nothing escaped.
        assert!(!work.join("evil.txt").exists());

        fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn targz_roundtrip() {
        let work = tmp("targz");
        let a = work.join("a.txt");
        fs::write(&a, b"tar-hello").unwrap();
        let out = work.join("out.tar.gz");
        targz_paths(std::slice::from_ref(&a), &out).unwrap();

        let dest = work.join("un");
        let res = extract_archive(&out, &dest).unwrap();
        assert_eq!(res.files, 1);
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"tar-hello");

        fs::remove_dir_all(&work).ok();
    }
}
