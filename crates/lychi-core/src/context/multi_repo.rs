//! Single source of truth for "where does a `run` command execute?"
//!
//! `resolve_run_targets` takes the environment as plain data (`RunContext`) and
//! returns a ranked list of candidate directories plus a mode:
//!   - `AutoRun`  (exactly one candidate) — terminal cwd, or a single-repo IDE
//!     workspace: nothing to choose, just run there.
//!   - `Pick`     (several candidates) — a multi-repo container workspace:
//!     the caller shows one row per repo and the user picks explicitly.
//!
//! Single-repo and multi-repo travel the SAME path — the only difference is the
//! candidate count. This unifies the previously scattered logic (executor cwd
//! precedence, `ide::resolve_code_root`, ad-hoc multi-repo picking) behind one
//! pure, testable-without-Tauri function.
//!
//! Ranking is usage-driven (frecency per workspace) then recency (mtime) then
//! name — never a hardcoded repo list. A "run in all repos" fan-out is offered
//! only for read-only/safe commands (`git status`, `pnpm install`), never for
//! side-effectful ones (`pnpm dev`).

use std::path::Path;
use std::sync::Arc;

use redb::Database;

/// The focused window, as far as run-target resolution cares.
#[derive(Debug, Clone)]
pub enum FocusedWindow {
    /// A terminal is focused; `cwd` is its working directory (if known).
    Terminal { cwd: Option<String> },
    /// An IDE is focused with this workspace root open.
    Ide { workspace_root: Option<String> },
    /// Anything else / nothing.
    Other,
}

/// Everything the resolver needs — plain data, so it's unit-testable without
/// Tauri or a live desktop.
pub struct RunContext<'a> {
    pub focused: FocusedWindow,
    /// Coherent terminal cwd (same repo/project as the workspace), if any.
    pub coherent_terminal_cwd: Option<String>,
    pub db: &'a Arc<Database>,
}

/// One place a command could run.
#[derive(Debug, Clone, PartialEq)]
pub struct RunTarget {
    /// Absolute directory to run in.
    pub dir: String,
    /// Display name (directory basename).
    pub name: String,
}

/// How the caller should treat the candidate list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetMode {
    /// Exactly one candidate — run there, no picker.
    AutoRun,
    /// Several candidates — show one row per target; the user picks.
    Pick,
}

/// The resolved run target(s).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTargets {
    /// Ranked candidates (frecency → mtime → name).
    pub candidates: Vec<RunTarget>,
    pub mode: TargetMode,
    /// When `Some`, a "run in all repos" fan-out is offered (read-only/safe
    /// commands only). Holds the container whose child repos are the targets.
    pub all_repos_container: Option<String>,
}

/// Resolve where `command` should run, given the environment.
/// Returns `None` when there's no usable context (caller falls back to its
/// process cwd / current behaviour).
pub fn resolve_run_targets(command: &str, ctx: &RunContext) -> Option<ResolvedTargets> {
    // Produce the raw candidate directories per focus.
    let (mut candidates, container) = match &ctx.focused {
        // Terminal focused → its cwd is the single target.
        FocusedWindow::Terminal { cwd } => {
            let dir = cwd.clone()?;
            (vec![dir], None)
        }
        FocusedWindow::Ide { workspace_root } => {
            let root = workspace_root.as_deref()?;
            let root_path = Path::new(root);
            if crate::context::ide::is_project_dir(root_path) {
                // Single-repo workspace → the root itself.
                (vec![root.to_string()], None)
            } else {
                // Container of repos → each child repo is a target.
                let repos = crate::context::ide::enumerate_child_repos(root_path);
                if repos.is_empty() {
                    // No repos found under a non-project container — fall back
                    // to a coherent terminal cwd if we have one, else give up.
                    let dir = ctx.coherent_terminal_cwd.clone()?;
                    (vec![dir], None)
                } else {
                    (repos, Some(root.to_string()))
                }
            }
        }
        FocusedWindow::Other => return None,
    };

    // Rank: frecency (per container) → mtime → name. Container scoping keys the
    // frecency; for single-target lists this is a no-op.
    if candidates.len() > 1 {
        rank_candidates(&mut candidates, container.as_deref(), ctx.db);
    }

    let targets: Vec<RunTarget> = candidates
        .into_iter()
        .map(|dir| {
            let name = Path::new(&dir)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&dir)
                .to_string();
            RunTarget { dir, name }
        })
        .collect();

    let mode = if targets.len() == 1 {
        TargetMode::AutoRun
    } else {
        TargetMode::Pick
    };

    // Offer a fan-out "all repos" row only for a multi-repo Pick AND a
    // read-only / safe command.
    let all_repos_container = if mode == TargetMode::Pick && is_fanout_safe(command) {
        container
    } else {
        None
    };

    Some(ResolvedTargets {
        candidates: targets,
        mode,
        all_repos_container,
    })
}

/// Record that `command` ran in `repo` under `container`, so this workspace's
/// repos rank by usage next time. `None` container (single-target) is a no-op.
pub fn record_choice(db: &Arc<Database>, container: Option<&str>, repo: &str) {
    if let Some(container) = container {
        let _ = crate::db::frecency::record_repo_choice(db, container, repo);
    }
}

/// Rank repo paths by frecency (usage in this container) → mtime → name.
fn rank_candidates(candidates: &mut [String], container: Option<&str>, db: &Arc<Database>) {
    let scores = container
        .map(|c| crate::db::frecency::get_repo_choice_scores(db, c))
        .unwrap_or_default();
    candidates.sort_by(|a, b| {
        let sa = scores.get(a).copied().unwrap_or(0.0);
        let sb = scores.get(b).copied().unwrap_or(0.0);
        // Higher frecency first.
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Then more-recently-modified first.
            .then_with(|| mtime(b).cmp(&mtime(a)))
            // Then stable alphabetical.
            .then_with(|| a.cmp(b))
    });
}

/// Directory mtime as a sort key (0 if unavailable).
fn mtime(path: &str) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether a command is safe to fan out across all repos: read-only / idempotent
/// (status/log/fetch/pull, dependency install). Side-effectful or long-running
/// commands (dev servers, builds, deletes) are NOT — running them in every repo
/// at once is dangerous/nonsensical. Name-agnostic: a small verb allowlist.
fn is_fanout_safe(command: &str) -> bool {
    let cmd = command.trim();
    const SAFE_TWO: &[&str] = &[
        "git status",
        "git fetch",
        "git pull",
        "git log",
        "git diff",
        "git branch",
        "git remote",
        "npm install",
        "pnpm install",
        "yarn install",
        "bun install",
    ];
    let words: Vec<&str> = cmd.split_whitespace().collect();
    if words.len() >= 2 {
        let two = format!("{} {}", words[0], words[1]);
        if SAFE_TWO.contains(&two.as_str()) {
            return true;
        }
    }
    // Bare `pnpm install` / `npm i` short forms.
    matches!(cmd, "npm i" | "pnpm i" | "yarn")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkrepo(base: &Path, name: &str) -> String {
        let d = base.join(name);
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::write(d.join("package.json"), "{}").unwrap();
        d.to_string_lossy().into_owned()
    }

    #[test]
    fn single_repo_workspace_autoruns() {
        let db = crate::db::open_test_database();
        let base = std::env::temp_dir().join(format!("lychi-single-{}", std::process::id()));
        let repo = mkrepo(&base, "solo");
        let ctx = RunContext {
            focused: FocusedWindow::Ide {
                workspace_root: Some(repo.clone()),
            },
            coherent_terminal_cwd: None,
            db: &db,
        };
        let r = resolve_run_targets("pnpm dev", &ctx).unwrap();
        assert_eq!(r.mode, TargetMode::AutoRun);
        assert_eq!(r.candidates.len(), 1);
        assert_eq!(r.candidates[0].dir, repo);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn container_workspace_gives_pick_list() {
        let db = crate::db::open_test_database();
        let base = std::env::temp_dir().join(format!("lychi-cont-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        // Container has a marker but no build marker → not a single repo.
        std::fs::create_dir_all(base.join(".claude")).unwrap();
        let a = mkrepo(&base, "amt-course-registration");
        let b = mkrepo(&base, "amt-api");
        let c = mkrepo(&base, "AMT-admin");

        let ctx = RunContext {
            focused: FocusedWindow::Ide {
                workspace_root: Some(base.to_string_lossy().into_owned()),
            },
            coherent_terminal_cwd: None,
            db: &db,
        };
        // pnpm dev → Pick over all repos, no fan-out (not safe), none auto.
        let r = resolve_run_targets("pnpm dev", &ctx).unwrap();
        assert_eq!(r.mode, TargetMode::Pick);
        assert_eq!(r.candidates.len(), 3);
        let dirs: Vec<&str> = r.candidates.iter().map(|t| t.dir.as_str()).collect();
        assert!(
            dirs.contains(&a.as_str()) && dirs.contains(&b.as_str()) && dirs.contains(&c.as_str())
        );
        assert!(r.all_repos_container.is_none());

        // git status → Pick + fan-out row offered.
        let r2 = resolve_run_targets("git status", &ctx).unwrap();
        assert_eq!(r2.mode, TargetMode::Pick);
        assert!(r2.all_repos_container.is_some());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn terminal_focus_autoruns_cwd() {
        let db = crate::db::open_test_database();
        let ctx = RunContext {
            focused: FocusedWindow::Terminal {
                cwd: Some("/tmp/somewhere".into()),
            },
            coherent_terminal_cwd: None,
            db: &db,
        };
        let r = resolve_run_targets("ls", &ctx).unwrap();
        assert_eq!(r.mode, TargetMode::AutoRun);
        assert_eq!(r.candidates[0].dir, "/tmp/somewhere");
    }

    #[test]
    fn fanout_safety() {
        assert!(is_fanout_safe("git status"));
        assert!(is_fanout_safe("git fetch --all"));
        assert!(is_fanout_safe("pnpm install"));
        // Side-effectful / long-running → not safe to fan out.
        assert!(!is_fanout_safe("pnpm dev"));
        assert!(!is_fanout_safe("pnpm build"));
        assert!(!is_fanout_safe("rm -rf node_modules"));
        assert!(!is_fanout_safe("git push"));
    }
}
