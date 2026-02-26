//! Context Awareness — detects the user's environment on summon.
//!
//! This is a data provider brick, not an action handler. It feeds into
//! the completions pipeline and AI routing to make Lychi context-aware.
//!
//! Detects: active window, terminal CWD, git state, project type, Docker.
//! Refreshed on each summon. Window stack scanning finds the nearest
//! terminal even when an IDE has focus.

pub mod active_window;
pub mod cache;
pub mod cwd;
pub mod docker;
pub mod git;
pub mod ide;
pub mod project;
pub mod suggestions;
pub mod window_stack;

use std::time::Instant;

use chrono::Timelike;
use serde::{Deserialize, Serialize};

/// The complete environmental context, refreshed on each summon.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvironmentContext {
    pub active_window: Option<WindowContext>,
    pub cwd: Option<String>,
    /// CWD from the most recently focused terminal (if different from `cwd`).
    /// Set when an IDE has focus but a terminal was recently used.
    /// Shell commands prefer this over `cwd` when available.
    #[serde(default)]
    pub terminal_cwd: Option<String>,
    /// WM class of the detected terminal emulator (from active window or stack).
    /// Used to launch `run` commands in the same terminal the user already uses.
    #[serde(default)]
    pub terminal_class: Option<String>,
    pub git: Option<GitContext>,
    pub project: Option<ProjectContext>,
    pub docker: Option<DockerContext>,
    /// Current hour (0-23) for time-aware suggestion ranking.
    #[serde(default)]
    pub hour: u8,
    /// Milliseconds taken to gather context.
    pub gather_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowContext {
    pub title: String,
    pub wm_class: String,
    pub pid: u32,
    pub is_terminal: bool,
    #[serde(default)]
    pub is_ide: bool,
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
    Flutter,
    Docker,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectScript {
    /// Command runner (e.g. "npm run", "make", "just").
    pub runner: String,
    /// Script/target name.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectContext {
    pub root: String,
    pub kind: ProjectKind,
    /// Whether a `docker-compose.yml` or `compose.yml` exists in the project root.
    #[serde(default)]
    pub has_compose: bool,
    /// Discovered project scripts/targets (npm scripts, Makefile targets, Justfile recipes).
    #[serde(default)]
    pub scripts: Vec<ProjectScript>,
    /// Detected package manager for Node projects (npm, pnpm, yarn, bun).
    #[serde(default)]
    pub package_manager: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerContext {
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
        if let Some(ref tcwd) = self.terminal_cwd {
            lines.push(format!("- Terminal CWD: {tcwd}"));
        }
        if let Some(ref git) = self.git {
            let dirty_flag = if git.dirty { " (dirty)" } else { "" };
            lines.push(format!("- Git branch: {}{dirty_flag}", git.branch));
        }
        if let Some(ref proj) = self.project {
            lines.push(format!("- Project type: {:?}", proj.kind));
            if let Some(ref pm) = proj.package_manager {
                lines.push(format!("- Package manager: {pm}"));
            }
        }
        if let Some(ref docker) = self.docker {
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

/// Detect session type from XDG_SESSION_TYPE.
#[cfg(target_os = "linux")]
pub fn is_wayland() -> bool {
    std::env::var("XDG_SESSION_TYPE")
        .map(|v| v == "wayland")
        .unwrap_or(false)
}

/// Snapshot the active window right now (before Lychi steals focus).
///
/// Call this **before** `show_window()`, then pass the result to
/// `gather()` inside `spawn_blocking`.
pub fn snapshot_active_window() -> Option<WindowContext> {
    active_window::detect()
}

/// Gather all context. Called on summon via `spawn_blocking`.
///
/// Each detector is fail-safe — returns `None` on any error.
/// Refreshed on every summon (no caching).
///
/// `pre_captured` should be the window snapshot taken **before** Lychi was shown.
/// If `None`, falls back to detecting the current active window (which may be Lychi itself).
///
/// When the focused window is NOT a terminal, a parallel window-stack scan
/// finds the most recently focused terminal and extracts its CWD into
/// `terminal_cwd`. Shell commands prefer this over the IDE-derived `cwd`.
pub fn gather(pre_captured: Option<WindowContext>) -> EnvironmentContext {
    let start = Instant::now();

    tracing::debug!(
        "gather: pre_captured={:?}",
        pre_captured
            .as_ref()
            .map(|w| format!("{}(pid={},term={})", w.wm_class, w.pid, w.is_terminal))
    );

    let window = pre_captured.or_else(active_window::detect);

    tracing::debug!(
        "gather: window={:?}",
        window
            .as_ref()
            .map(|w| format!("{}(pid={},term={})", w.wm_class, w.pid, w.is_terminal))
    );

    // Run window-stack scan in parallel with CWD/git/project/docker detection.
    // The stack scan involves D-Bus (KWin) or X11 calls that can take 50-200ms,
    // so we overlap it with the other detections.
    let (main_result, stack_terminal) = std::thread::scope(|s| {
        // Spawn the window-stack scan
        let window_ref = window.as_ref();
        let stack_handle = s.spawn(move || window_stack::find_recent_terminal(window_ref));

        // Main thread: CWD + git + project + docker (sequential chain)
        let cwd = window.as_ref().and_then(|w| {
            if w.is_terminal {
                cwd::detect(w.pid, &w.wm_class, &w.title)
            } else if w.is_ide {
                ide::detect_workspace(&w.title, &w.wm_class)
            } else {
                None
            }
        });

        let git_ctx = cwd.as_ref().and_then(|dir| {
            // Check cache first — avoids spawning `git status` subprocess
            if let Some(cached) = cache::get_git(dir) {
                tracing::debug!("gather: git cache hit for {dir}");
                return cached;
            }
            let result = git::detect(dir);
            cache::set_git(dir, &result);
            result
        });

        let project_ctx = cwd
            .as_ref()
            .and_then(|dir| {
                if let Some(cached) = cache::get_project(dir) {
                    tracing::debug!("gather: project cache hit for {dir}");
                    return cached;
                }
                let result = project::detect(dir)
                    .or_else(|| git_ctx.as_ref().and_then(|g| project::detect(&g.repo_root)));
                cache::set_project(&result);
                result
            })
            .or_else(|| git_ctx.as_ref().and_then(|g| project::detect(&g.repo_root)));

        let docker_ctx = if let Some(cached) = cache::get_docker() {
            tracing::debug!("gather: docker cache hit");
            cached
        } else {
            let result = docker::detect();
            cache::set_docker(&result);
            result
        };

        let main = (cwd, git_ctx, project_ctx, docker_ctx);
        let stack = stack_handle.join().ok().flatten();

        (main, stack)
    });

    let (cwd, git_ctx, project_ctx, docker_ctx) = main_result;

    tracing::debug!(
        "gather: stack_terminal={:?}, cwd={:?}",
        stack_terminal
            .as_ref()
            .map(|w| format!("{}(pid={})", w.wm_class, w.pid)),
        cwd.as_deref()
    );

    // Derive terminal_cwd from the stack-detected terminal
    let terminal_cwd = stack_terminal
        .as_ref()
        .and_then(|t| cwd::detect(t.pid, &t.wm_class, &t.title));

    tracing::debug!("gather: terminal_cwd={:?}", terminal_cwd.as_deref());

    // When terminal_cwd is available AND the focused window IS a terminal,
    // re-derive git/project from the terminal CWD (handles multi-terminal setups
    // where the stack terminal differs from the focused one).
    // When an IDE is focused, the IDE workspace context (already computed above)
    // is the primary context. When a non-dev window (browser, etc.) is focused,
    // skip context entirely — don't leak background terminal context.
    let focused_is_terminal = window.as_ref().is_some_and(|w| w.is_terminal);
    let (git_ctx, project_ctx) = if let Some(ref tcwd) = terminal_cwd
        && focused_is_terminal
        && cwd.as_deref() != Some(tcwd.as_str())
    {
        let git = if let Some(cached) = cache::get_git(tcwd) {
            tracing::debug!("gather: git cache hit for terminal_cwd {tcwd}");
            cached
        } else {
            let result = git::detect(tcwd);
            cache::set_git(tcwd, &result);
            result
        };
        let proj = if let Some(cached) = cache::get_project(tcwd) {
            tracing::debug!("gather: project cache hit for terminal_cwd {tcwd}");
            cached
        } else {
            let result = project::detect(tcwd)
                .or_else(|| git.as_ref().and_then(|g| project::detect(&g.repo_root)));
            cache::set_project(&result);
            result
        };
        tracing::debug!(
            "gather: re-derived from terminal_cwd: git={:?}, project={:?}",
            git.as_ref().map(|g| g.branch.as_str()),
            proj.as_ref().map(|p| format!("{:?}", p.kind))
        );
        (git, proj)
    } else {
        (git_ctx, project_ctx)
    };

    // Detect terminal emulator class: from focused window (if terminal) or stack
    let terminal_class = window
        .as_ref()
        .filter(|w| w.is_terminal)
        .map(|w| w.wm_class.clone())
        .or_else(|| stack_terminal.as_ref().map(|t| t.wm_class.clone()));

    EnvironmentContext {
        active_window: window,
        cwd,
        terminal_cwd,
        terminal_class,
        git: git_ctx,
        project: project_ctx,
        docker: docker_ctx,
        hour: chrono::Local::now().hour() as u8,
        gather_ms: start.elapsed().as_millis() as u64,
    }
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
            println!("  containers={}", d.containers.len());
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
            ..Default::default()
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
