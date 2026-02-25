//! Context Awareness — detects the user's environment on summon.
//!
//! This is a data provider brick, not an action handler. It feeds into
//! the completions pipeline and AI routing to make Lychi context-aware.
//!
//! Detects: active window, terminal CWD, git state, project type, Docker.
//! Refreshed on each summon, cached with 5s TTL.

pub mod active_window;
pub mod cwd;
pub mod docker;
pub mod git;
pub mod project;
pub mod suggestions;

use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// The complete environmental context, refreshed on each summon.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvironmentContext {
    pub active_window: Option<WindowContext>,
    pub cwd: Option<String>,
    pub git: Option<GitContext>,
    pub project: Option<ProjectContext>,
    pub docker: Option<DockerContext>,
    /// Milliseconds taken to gather context.
    pub gather_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowContext {
    pub title: String,
    pub wm_class: String,
    pub pid: u32,
    pub is_terminal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitContext {
    pub repo_root: String,
    pub branch: String,
    pub dirty: bool,
    pub remote: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProjectKind {
    Rust,
    Node,
    Python,
    Go,
    Docker,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectContext {
    pub root: String,
    pub kind: ProjectKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerContext {
    pub daemon_running: bool,
    pub containers: Vec<ContainerInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub image: String,
    pub status: String,
}

impl EnvironmentContext {
    /// Build a concise hint string for AI routing prompts.
    pub fn ai_hint(&self) -> Option<String> {
        let mut lines = Vec::new();

        if let Some(ref cwd) = self.cwd {
            lines.push(format!("- Working directory: {cwd}"));
        }
        if let Some(ref git) = self.git {
            let dirty_flag = if git.dirty { " (dirty)" } else { "" };
            lines.push(format!("- Git branch: {}{dirty_flag}", git.branch));
        }
        if let Some(ref proj) = self.project {
            lines.push(format!("- Project type: {:?}", proj.kind));
        }
        if let Some(ref docker) = self.docker
            && docker.daemon_running
        {
            let n = docker.containers.len();
            lines.push(format!("- Docker: {n} running container(s)"));
        }
        if let Some(ref win) = self.active_window
            && !win.is_terminal
        {
            lines.push(format!("- Active window: {} ({})", win.title, win.wm_class));
        }

        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }
}

static CONTEXT_CACHE: Mutex<Option<(EnvironmentContext, Instant)>> = Mutex::new(None);
const CACHE_TTL_SECS: u64 = 5;

/// Snapshot the active window right now (before Lychi steals focus).
///
/// Call this **before** `show_window()`, then pass the result to
/// `gather_with_window()` inside `spawn_blocking`.
pub fn snapshot_active_window() -> Option<WindowContext> {
    active_window::detect()
}

/// Gather all context. Called on summon via `spawn_blocking`.
///
/// Each detector is fail-safe — returns `None` on any error.
/// Results are cached for 5 seconds to avoid redundant work on rapid summons.
///
/// `pre_captured` should be the window snapshot taken **before** Lychi was shown.
/// If `None`, falls back to detecting the current active window (which may be Lychi itself).
pub fn gather(pre_captured: Option<WindowContext>) -> EnvironmentContext {
    // Check cache first
    if let Ok(guard) = CONTEXT_CACHE.lock()
        && let Some((ref ctx, ref ts)) = *guard
        && ts.elapsed().as_secs() < CACHE_TTL_SECS
    {
        return ctx.clone();
    }

    let start = Instant::now();

    let window = pre_captured.or_else(active_window::detect);

    let cwd = window
        .as_ref()
        .filter(|w| w.is_terminal)
        .and_then(|w| cwd::detect(w.pid, &w.wm_class, &w.title));

    let git_ctx = cwd.as_ref().and_then(|dir| git::detect(dir));

    let project_ctx = cwd
        .as_ref()
        .and_then(|dir| project::detect(dir))
        .or_else(|| git_ctx.as_ref().and_then(|g| project::detect(&g.repo_root)));

    let docker_ctx = docker::detect();

    let ctx = EnvironmentContext {
        active_window: window,
        cwd,
        git: git_ctx,
        project: project_ctx,
        docker: docker_ctx,
        gather_ms: start.elapsed().as_millis() as u64,
    };

    // Update cache
    if let Ok(mut guard) = CONTEXT_CACHE.lock() {
        *guard = Some((ctx.clone(), Instant::now()));
    }

    ctx
}

/// Get cached context without refreshing.
pub fn cached() -> Option<EnvironmentContext> {
    CONTEXT_CACHE
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|(ctx, _)| ctx.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gather_live() {
        // Test active window (may be None in CI / headless)
        let win = active_window::detect();
        println!("=== Active Window ===");
        if let Some(ref w) = win {
            println!(
                "  class={}, pid={}, terminal={}, title={}",
                w.wm_class, w.pid, w.is_terminal, w.title
            );
        } else {
            println!("  None (expected in headless/test env)");
        }

        // Test git detection from THIS repo's directory
        let this_dir = env!("CARGO_MANIFEST_DIR");
        println!("\n=== Git (from {this_dir}) ===");
        if let Some(ref g) = git::detect(this_dir) {
            println!(
                "  branch={}, dirty={}, root={}, remote={:?}",
                g.branch, g.dirty, g.repo_root, g.remote
            );
        } else {
            println!("  None");
        }

        // Test project detection
        println!("\n=== Project ===");
        if let Some(ref p) = project::detect(this_dir) {
            println!("  kind={:?}, root={}", p.kind, p.root);
        } else {
            println!("  None");
        }

        // Test Docker
        println!("\n=== Docker ===");
        if let Some(ref d) = docker::detect() {
            println!(
                "  daemon={}, containers={}",
                d.daemon_running,
                d.containers.len()
            );
            for c in &d.containers {
                println!("    {} ({}) — {}", c.name, c.image, c.status);
            }
        } else {
            println!("  None (no Docker socket)");
        }

        // Test full gather
        let ctx = gather(None);
        println!("\n=== Full Gather ({}ms) ===", ctx.gather_ms);

        if let Some(hint) = ctx.ai_hint() {
            println!("AI hint:\n{hint}");
        } else {
            println!("AI hint: None");
        }

        println!("\n=== Suggestions ===");
        // Build a fake context using real git/project data for suggestions
        let test_ctx = EnvironmentContext {
            active_window: None,
            cwd: Some(this_dir.to_string()),
            git: git::detect(this_dir),
            project: project::detect(this_dir),
            docker: docker::detect(),
            gather_ms: 0,
        };
        for item in suggestions::suggest(&test_ctx) {
            println!(
                "  {} — {}",
                item.label,
                item.description.unwrap_or_default()
            );
        }
    }
}
