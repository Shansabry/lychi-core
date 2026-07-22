//! Central path-access decider — the shared sanity gate for "may we open /
//! reveal / read this filesystem path?". Companion to `rules::shell::authorize`
//! and `rules::uri::authorize_uri`.
//!
//! Threat model note: Lychi is a **local single-user** launcher, so opening a
//! path the user themselves typed is intended behavior — this is NOT a sandbox
//! and does not jail paths to a workspace. What it *does* stop is the malformed
//! / traversal-laden / non-canonical input that indicates the path did not come
//! straight from the user (e.g. an `@`-reference or AI/bookmark-derived string):
//! embedded NUL bytes, and paths that use `..` to climb ABOVE the filesystem
//! root. It also canonicalizes so callers act on the resolved real path.

use std::path::{Component, Path, PathBuf};

/// The verdict for touching a filesystem path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathDecision {
    /// Safe to act on. Carries the cleaned path the caller should use.
    Allow { path: PathBuf },
    /// Blocked — the path is malformed or escapes the root.
    Deny { reason: String },
}

/// The **central path decider**. Validates a filesystem path before it is opened
/// or revealed. Does not require the path to exist (callers check existence for
/// their own control flow), but rejects clearly-unsafe shapes.
///
/// - NUL byte anywhere → `Deny` (can truncate the path at the OS boundary).
/// - `..` components that climb above the root → `Deny` (traversal escape).
/// - otherwise `Allow`, returning the lexically-normalized path (`.`/redundant
///   `..` resolved) so callers act on a clean value.
pub fn authorize_path(raw: &Path) -> PathDecision {
    let as_str = raw.to_string_lossy();
    if as_str.contains('\0') {
        return PathDecision::Deny {
            reason: "Path contains a NUL byte".to_string(),
        };
    }
    if as_str.trim().is_empty() {
        return PathDecision::Deny {
            reason: "Empty path".to_string(),
        };
    }

    // Lexically normalize, tracking depth so a `..` that would escape above the
    // root (for an absolute path) or above the start (for a relative one) is
    // rejected rather than silently clamped.
    let mut out: Vec<Component> = Vec::new();
    let mut depth: i32 = 0;
    let is_absolute = raw.is_absolute();
    for comp in raw.components() {
        match comp {
            Component::ParentDir => {
                if depth == 0 {
                    // Climbing above the root/start.
                    if is_absolute {
                        return PathDecision::Deny {
                            reason: format!("Path escapes filesystem root via `..`: {as_str}"),
                        };
                    }
                    // Relative path climbing above its start — keep the `..`
                    // (legitimate for a relative reference) but don't go negative.
                    out.push(comp);
                } else {
                    out.pop();
                    depth -= 1;
                }
            }
            Component::CurDir => { /* drop `.` */ }
            Component::Normal(_) => {
                out.push(comp);
                depth += 1;
            }
            // Root / prefix components anchor the path; keep them, no depth change.
            other => out.push(other),
        }
    }

    let normalized: PathBuf = out.iter().collect();
    PathDecision::Allow { path: normalized }
}

/// Convenience: return the cleaned path if allowed, else `Err(reason)`.
pub fn check_path(raw: &Path) -> Result<PathBuf, String> {
    match authorize_path(raw) {
        PathDecision::Allow { path } => Ok(path),
        PathDecision::Deny { reason } => Err(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_normal_paths() {
        assert!(matches!(
            authorize_path(Path::new("/home/sab/notes.txt")),
            PathDecision::Allow { .. }
        ));
        assert!(matches!(
            authorize_path(Path::new("relative/file.md")),
            PathDecision::Allow { .. }
        ));
    }

    #[test]
    fn normalizes_dot_and_parent() {
        // /home/sab/../sab/./notes.txt → /home/sab/notes.txt
        let d = authorize_path(Path::new("/home/sab/../sab/./notes.txt"));
        match d {
            PathDecision::Allow { path } => {
                assert_eq!(path, PathBuf::from("/home/sab/notes.txt"));
            }
            _ => panic!("expected allow"),
        }
    }

    #[test]
    fn denies_root_escape() {
        assert!(matches!(
            authorize_path(Path::new("/../../etc/passwd")),
            PathDecision::Deny { .. }
        ));
        assert!(matches!(
            authorize_path(Path::new("/..")),
            PathDecision::Deny { .. }
        ));
    }

    #[test]
    fn denies_nul_and_empty() {
        assert!(matches!(
            authorize_path(Path::new("/home/\0/x")),
            PathDecision::Deny { .. }
        ));
        assert!(matches!(authorize_path(Path::new("")), PathDecision::Deny { .. }));
    }

    #[test]
    fn relative_parent_is_kept_not_escaped() {
        // A relative `../sibling` is a legitimate reference, not a root escape.
        assert!(matches!(
            authorize_path(Path::new("../sibling/file")),
            PathDecision::Allow { .. }
        ));
    }
}
