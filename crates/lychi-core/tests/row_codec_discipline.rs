//! Source-scan invariant: raw `postcard::` row codecs stay inside `db/`.
//!
//! Every redb row value is `[SCHEMA_VERSION][body]`. That contract only holds
//! if every writer goes through `db::encode_row`/`wrap_body` and every reader
//! through `db::decode_row`/`decode_value`/`body_of` — one site calling
//! `postcard::to_allocvec` against a table produces an untagged row that every
//! reader then skips as garbage, which is precisely the silent-data-loss class
//! the envelope exists to prevent. A convention would rot the first time a new
//! store is written from an old example; a scan cannot.
//!
//! Allowed:
//! - `src/db/mod.rs` — the codec itself.
//! - `src/backup/mod.rs` — its `postcard::` is the ARCHIVE container format
//!   (`TableDump`), one layer below rows; row values pass through it as raw
//!   bytes, tag included.

use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn raw_postcard_row_codecs_stay_in_db() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();

    const ALLOWED: [&str; 2] = ["src/db/mod.rs", "src/backup/mod.rs"];
    let mut offenders: Vec<String> = Vec::new();

    for file in files {
        let rel = file
            .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")))
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED.iter().any(|a| rel.ends_with(a)) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (i, line) in src.lines().enumerate() {
            // Codec calls only — doc comments may discuss postcard freely.
            let code = line.split("//").next().unwrap_or("");
            if code.contains("postcard::from_bytes") || code.contains("postcard::to_allocvec") {
                offenders.push(format!("{rel}:{}", i + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "raw postcard row codec outside db/ — rows written there are untagged \
         and every reader will skip them as garbage. Use db::encode_row / \
         db::decode_row / db::decode_value instead.\n{}",
        offenders.join("\n")
    );
}
