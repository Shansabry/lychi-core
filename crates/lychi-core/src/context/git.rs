//! Git repository detection.
//!
//! Walks upward from a directory looking for `.git/`. If found, reads
//! branch name from HEAD, dirty state from `git status`, and remote URL.

use std::fs;
use std::path::Path;
use std::process::Command;

use super::GitContext;

/// Detect git repository context from a directory.
pub fn detect(dir: &str) -> Option<GitContext> {
    let repo_root = find_git_root(dir)?;
    let branch = read_branch(&repo_root)?;
    let dirty = check_dirty(&repo_root);
    let remote = read_remote(&repo_root);

    Some(GitContext {
        repo_root,
        branch,
        dirty,
        remote,
    })
}

/// Walk upward looking for a `.git` entry (directory or file).
///
/// Returns the **working-tree root** (directory containing `.git`), not the gitdir itself.
/// Handles plain repos, worktrees (`.git` is a file), and submodules.
pub(crate) fn find_git_root(start: &str) -> Option<String> {
    let mut dir = Path::new(start);
    for _ in 0..50 {
        let git_entry = dir.join(".git");
        if git_entry.is_dir() || git_entry.is_file() {
            return Some(dir.to_string_lossy().into_owned());
        }
        dir = dir.parent()?;
    }
    None
}

/// Resolve the canonical gitdir path for a given working-tree root.
///
/// For plain repos: `<root>/.git`
/// For worktrees/submodules: reads the `gitdir:` pointer from the `.git` file.
///
/// Used for coherence checks — two paths with the same resolved gitdir belong
/// to the same repository (even across worktrees of the same repo).
pub fn resolve_gitdir(repo_root: &str) -> Option<String> {
    let git_path = Path::new(repo_root).join(".git");
    if git_path.is_dir() {
        // Plain repo — gitdir IS `.git`
        return Some(git_path.to_string_lossy().into_owned());
    }
    if git_path.is_file() {
        // Worktree or submodule — `.git` file contains "gitdir: <path>"
        let content = fs::read_to_string(&git_path).ok()?;
        let gitdir = content.trim().strip_prefix("gitdir: ")?.trim().to_string();
        // Resolve relative paths (gitdir may be relative to repo_root)
        let resolved = if Path::new(&gitdir).is_absolute() {
            gitdir
        } else {
            Path::new(repo_root)
                .join(&gitdir)
                .to_string_lossy()
                .into_owned()
        };
        // For worktrees, the gitdir points into `.git/worktrees/<name>`.
        // Strip that to get the common gitdir (the main repo's `.git`).
        let p = Path::new(&resolved);
        if let Some(parent) = p.parent()
            && parent.file_name().is_some_and(|n| n == "worktrees")
        {
            // e.g. /project/.git/worktrees/feature → /project/.git
            return parent.parent().map(|pp| pp.to_string_lossy().into_owned());
        }
        return Some(resolved);
    }
    None
}

/// Read current branch from `.git/HEAD`.
///
/// HEAD contains either `ref: refs/heads/<branch>` or a detached commit hash.
fn read_branch(repo_root: &str) -> Option<String> {
    let head = fs::read_to_string(Path::new(repo_root).join(".git/HEAD")).ok()?;
    let head = head.trim();

    if let Some(ref_path) = head.strip_prefix("ref: refs/heads/") {
        Some(ref_path.to_string())
    } else if head.len() >= 7 {
        // Detached HEAD — show short hash
        Some(head[..7].to_string())
    } else {
        None
    }
}

/// Check if the repo has uncommitted changes via `git status --porcelain`.
///
/// Uses a 500ms timeout to avoid blocking on large repos or NFS mounts.
fn check_dirty(repo_root: &str) -> bool {
    use std::time::{Duration, Instant};

    let mut child = match Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(repo_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return false;
                }
                return child
                    .stdout
                    .take()
                    .and_then(|mut out| {
                        use std::io::Read;
                        let mut buf = [0u8; 1];
                        out.read(&mut buf).ok().map(|n| n > 0)
                    })
                    .unwrap_or(false);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::warn!("git status timed out in {}", repo_root);
                    return false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return false,
        }
    }
}

/// Read the remote origin URL from `.git/config`.
fn read_remote(repo_root: &str) -> Option<String> {
    let config = fs::read_to_string(Path::new(repo_root).join(".git/config")).ok()?;

    let mut in_remote_origin = false;
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed == "[remote \"origin\"]" {
            in_remote_origin = true;
            continue;
        }
        if trimmed.starts_with('[') {
            in_remote_origin = false;
            continue;
        }
        if in_remote_origin && let Some(url) = trimmed.strip_prefix("url = ") {
            return Some(url.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a unique temp subdirectory to avoid cross-test collisions.
    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lychi-git-test-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create tmp dir");
        dir
    }

    #[test]
    fn test_resolve_gitdir_plain_repo() {
        let root = tmp_dir("plain");
        let git_dir = root.join(".git");
        fs::create_dir_all(&git_dir).expect("create .git dir");

        let resolved = resolve_gitdir(root.to_str().unwrap())
            .expect("resolve_gitdir must return Some for plain repo");

        assert_eq!(
            Path::new(&resolved).canonicalize().unwrap(),
            git_dir.canonicalize().unwrap(),
            "plain repo: resolved gitdir must be <root>/.git"
        );
    }

    #[test]
    fn test_resolve_gitdir_worktree_same_common_gitdir() {
        // Build a fake worktree layout:
        //   /tmp/lychi-git-test-wt/repo/.git/          ← common gitdir
        //   /tmp/lychi-git-test-wt/wt-a/.git           ← file: "gitdir: <repo>/.git/worktrees/a"
        //   /tmp/lychi-git-test-wt/wt-b/.git           ← file: "gitdir: <repo>/.git/worktrees/b"
        let base = tmp_dir("wt");
        let repo_git = base.join("repo").join(".git");
        fs::create_dir_all(&repo_git).expect("create repo/.git");

        for name in &["a", "b"] {
            let wt_root = base.join(format!("wt-{name}"));
            fs::create_dir_all(&wt_root).expect("create worktree dir");
            let gitdir_target = repo_git.join("worktrees").join(name);
            fs::create_dir_all(&gitdir_target).expect("create worktrees/<name> dir");
            fs::write(
                wt_root.join(".git"),
                format!("gitdir: {}\n", gitdir_target.to_str().unwrap()),
            )
            .expect("write .git file");
        }

        let resolved_a = resolve_gitdir(base.join("wt-a").to_str().unwrap())
            .expect("resolve_gitdir must return Some for worktree wt-a");
        let resolved_b = resolve_gitdir(base.join("wt-b").to_str().unwrap())
            .expect("resolve_gitdir must return Some for worktree wt-b");

        assert_eq!(
            resolved_a, resolved_b,
            "both worktrees must resolve to the same common gitdir"
        );
        assert_eq!(
            Path::new(&resolved_a).canonicalize().unwrap(),
            repo_git.canonicalize().unwrap(),
            "resolved gitdir must be the repo's .git directory"
        );
    }
}
