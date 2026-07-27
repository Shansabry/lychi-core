//! Path helpers shared by every file action — home expansion, safe output
//! naming, and the extraction path-traversal guard. No I/O beyond `canonicalize`.

use std::path::{Path, PathBuf};

/// Expand a leading `~` to the home directory. `~/foo` → `$HOME/foo`, `~` → `$HOME`.
pub fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(path)
}

/// Build a non-colliding output path next to `src`: insert `suffix` before the
/// extension (`img.jpg` + `_800x600` → `img_800x600.jpg`), overriding the
/// extension when `new_ext` is `Some` (`img.png` + `""`/`Some("webp")` →
/// `img.webp`). Never returns the source path itself.
pub fn sibling_output(src: &Path, suffix: &str, new_ext: Option<&str>) -> PathBuf {
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = new_ext
        .map(str::to_string)
        .or_else(|| src.extension().and_then(|s| s.to_str()).map(String::from))
        .unwrap_or_else(|| "out".to_string());
    let file = if ext.is_empty() {
        format!("{stem}{suffix}")
    } else {
        format!("{stem}{suffix}.{ext}")
    };
    match src.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(file),
        _ => PathBuf::from(file),
    }
}

/// Zip-slip / path-traversal guard for archive extraction. Given the extraction
/// root `dest` (must already exist / be creatable) and a relative `entry` path
/// from inside the archive, return the absolute target ONLY if it stays under
/// `dest`. Rejects absolute paths, `..` escapes, and anything that resolves
/// outside the root.
///
/// `dest` is canonicalized once by the caller; here we join and verify the
/// lexical result stays under it. We intentionally do NOT canonicalize the
/// candidate (it doesn't exist yet), so we normalize `.`/`..` ourselves and
/// reject any `..` that would climb above the root.
pub fn guard_under(dest: &Path, entry: &Path) -> Option<PathBuf> {
    // An absolute entry can never be "under dest" safely — reject outright.
    if entry.is_absolute() {
        return None;
    }
    let mut out = dest.to_path_buf();
    for comp in entry.components() {
        use std::path::Component;
        match comp {
            Component::Normal(seg) => out.push(seg),
            Component::CurDir => {}
            // Any parent-dir hop is a traversal attempt — refuse the whole entry.
            Component::ParentDir => return None,
            // Absolute-root / prefix components inside an entry are illegal.
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    // Defense in depth: the assembled path must still start with dest.
    if out.starts_with(dest) {
        Some(out)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_keeps_extension() {
        assert_eq!(
            sibling_output(Path::new("/a/b/img.jpg"), "_800x600", None),
            PathBuf::from("/a/b/img_800x600.jpg")
        );
    }

    #[test]
    fn sibling_overrides_extension() {
        assert_eq!(
            sibling_output(Path::new("/a/b/img.png"), "", Some("webp")),
            PathBuf::from("/a/b/img.webp")
        );
    }

    #[test]
    fn guard_allows_nested_entry() {
        let dest = Path::new("/tmp/out");
        assert_eq!(
            guard_under(dest, Path::new("sub/dir/file.txt")),
            Some(PathBuf::from("/tmp/out/sub/dir/file.txt"))
        );
    }

    #[test]
    fn guard_rejects_parent_escape() {
        let dest = Path::new("/tmp/out");
        assert_eq!(guard_under(dest, Path::new("../evil")), None);
        assert_eq!(guard_under(dest, Path::new("a/../../evil")), None);
    }

    #[test]
    fn guard_rejects_absolute() {
        let dest = Path::new("/tmp/out");
        assert_eq!(guard_under(dest, Path::new("/etc/passwd")), None);
    }
}
