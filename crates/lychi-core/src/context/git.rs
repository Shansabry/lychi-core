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

/// Walk upward looking for a `.git` directory.
fn find_git_root(start: &str) -> Option<String> {
    let mut dir = Path::new(start);
    for _ in 0..50 {
        if dir.join(".git").is_dir() {
            return Some(dir.to_string_lossy().into_owned());
        }
        dir = dir.parent()?;
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
/// Uses a 500ms timeout to avoid blocking on large repos.
fn check_dirty(repo_root: &str) -> bool {
    // Use a child thread with timeout since Command doesn't support timeout natively
    let root = repo_root.to_string();
    let handle = std::thread::spawn(move || {
        Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=no"])
            .current_dir(&root)
            .output()
            .ok()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false)
    });

    handle.join().unwrap_or_default()
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
