//! Contextual completion suggestions based on detected environment.
//!
//! Maps `EnvironmentContext` → `Vec<CompletionItem>` to show relevant
//! commands when the user opens Lychi with an empty input.
//!
//! Each suggestion carries a structured `SuggestionReason` explaining *why*
//! it was suggested. The reason is serialized as a user-facing string on the
//! `CompletionItem::reason` field for frontend display.

use crate::action_registry::CompletionItem;

use super::{EnvironmentContext, ProjectKind};

// ── Suggestion Reason ───────────────────────────────────────────────────

/// Why a particular suggestion was generated. Structured so the engine can
/// produce user-facing strings, debug strings, and (later) scoring features
/// from the same data.
#[derive(Debug, Clone)]
pub enum SuggestionReason {
    /// Git working tree has uncommitted changes.
    GitDirty,
    /// Git working tree is clean — push/pull are the natural next actions.
    GitClean,
    /// On a non-default branch (not main/master).
    GitFeatureBranch { branch: String },
    /// A project script was discovered (npm run dev, cargo build, etc.).
    ProjectScript { runner: String },
    /// Install command for the detected package manager.
    ProjectInstall { pm: String },
    /// Project has a docker-compose.yml / compose.yml.
    DockerCompose,
    /// Docker daemon has running containers.
    DockerRunning { count: usize },
    /// User is deep in a project subdirectory.
    DirectoryDepth { project_name: String },
}

impl SuggestionReason {
    /// Short user-facing string for display in the UI.
    pub fn user_reason(&self) -> String {
        match self {
            Self::GitDirty => "Uncommitted changes".into(),
            Self::GitClean => "Up to date".into(),
            Self::GitFeatureBranch { branch } => format!("On {branch}"),
            Self::ProjectScript { runner } => format!("{runner} script"),
            Self::ProjectInstall { pm } => format!("{pm} project"),
            Self::DockerCompose => "Compose project".into(),
            Self::DockerRunning { count } => {
                format!("{count} container{}", if *count == 1 { "" } else { "s" })
            }
            Self::DirectoryDepth { project_name } => format!("In {project_name}/"),
        }
    }

    /// Verbose string for `ctx` debug output.
    pub fn debug_reason(&self) -> String {
        match self {
            Self::GitDirty => "git.dirty=true".into(),
            Self::GitClean => "git.dirty=false".into(),
            Self::GitFeatureBranch { branch } => format!("branch={branch} (not main/master)"),
            Self::ProjectScript { runner } => format!("script.runner={runner}"),
            Self::ProjectInstall { pm } => format!("project.pm={pm}"),
            Self::DockerCompose => "project.has_compose=true".into(),
            Self::DockerRunning { count } => format!("docker.containers={count}"),
            Self::DirectoryDepth { project_name } => {
                format!("cwd depth>=1 in {project_name}")
            }
        }
    }
}

/// Generate contextual completions based on current environment.
///
/// Returns up to 20 suggestions based on detected context.
pub fn suggest(ctx: &EnvironmentContext) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    // Only show dev-context suggestions (git, project, docker) when a
    // terminal or IDE is focused — don't leak background terminal context
    // into browser/random-app sessions.
    let in_dev_window = ctx
        .active_window
        .as_ref()
        .is_some_and(|w| w.is_terminal || w.is_ide);

    // Detect feature branch once (used for git reason enrichment)
    let on_feature_branch = ctx.git.as_ref().and_then(|g| {
        let b = g.branch.as_str();
        if b != "main" && b != "master" && b != "develop" {
            Some(b.to_string())
        } else {
            None
        }
    });

    // Git context suggestions
    if in_dev_window && let Some(ref git) = ctx.git {
        // Pick the most specific reason: feature branch > dirty/clean
        let dirty_reason = match &on_feature_branch {
            Some(branch) => SuggestionReason::GitFeatureBranch {
                branch: branch.clone(),
            },
            None => SuggestionReason::GitDirty,
        };
        let clean_reason = match &on_feature_branch {
            Some(branch) => SuggestionReason::GitFeatureBranch {
                branch: branch.clone(),
            },
            None => SuggestionReason::GitClean,
        };

        if git.dirty {
            items.push(completion(
                "git commit",
                "Commit staged changes",
                100,
                &dirty_reason,
            ));
            items.push(completion(
                "git diff",
                "View uncommitted changes",
                95,
                &dirty_reason,
            ));
            items.push(completion(
                "git status",
                "Show working tree status",
                88,
                &dirty_reason,
            ));
            items.push(completion(
                "git stash",
                "Stash current changes",
                85,
                &dirty_reason,
            ));
        } else {
            items.push(completion(
                "git pull",
                "Pull latest changes",
                100,
                &clean_reason,
            ));
            items.push(completion(
                "git push",
                "Push commits to remote",
                95,
                &clean_reason,
            ));
        }
    }

    // Project-specific install commands (not discoverable from config files)
    if in_dev_window && let Some(ref project) = ctx.project {
        let pm = project.package_manager.as_deref().unwrap_or("npm");
        if project.kind == ProjectKind::Node {
            let install_cmd = pm;
            let reason = SuggestionReason::ProjectInstall { pm: pm.into() };
            items.push(completion(
                &format!("run {install_cmd} install"),
                "Install dependencies",
                86,
                &reason,
            ));
        }
    }

    // Discovered project scripts (npm scripts, cargo commands, python scripts,
    // flutter/dart commands, go commands, Makefile targets, Justfile recipes, Taskfile tasks)
    if in_dev_window && let Some(ref project) = ctx.project {
        for (i, script) in project.scripts.iter().enumerate() {
            // Score descending so first scripts rank higher, all above Docker suggestions
            let score = 90u16.saturating_sub(i as u16);
            let label = if script.name.is_empty() {
                format!("run {}", script.runner)
            } else {
                format!("run {} {}", script.runner, script.name)
            };
            let desc = if script.name.is_empty() {
                script.runner.clone()
            } else {
                format!("{} {}", script.runner, script.name)
            };
            let reason = SuggestionReason::ProjectScript {
                runner: script.runner.clone(),
            };
            items.push(completion(&label, &desc, score.max(84), &reason));
        }
    }

    // Docker Compose suggestions (project-level)
    if in_dev_window
        && let Some(ref project) = ctx.project
        && project.has_compose
    {
        let reason = SuggestionReason::DockerCompose;
        items.push(completion(
            "run docker compose up -d",
            "Start all services",
            82,
            &reason,
        ));
        items.push(completion(
            "run docker compose down",
            "Stop all services",
            80,
            &reason,
        ));
        items.push(completion(
            "run docker compose logs",
            "View service logs",
            78,
            &reason,
        ));
    }

    // Docker container suggestions — only when in a dev context (terminal or IDE)
    if in_dev_window
        && let Some(ref docker) = ctx.docker
        && !docker.containers.is_empty()
    {
        let reason = SuggestionReason::DockerRunning {
            count: docker.containers.len(),
        };
        items.push(completion(
            "run docker ps",
            "List running containers",
            77,
            &reason,
        ));
        // Suggest logs for first container
        if let Some(first) = docker.containers.first() {
            items.push(completion(
                &format!("run docker logs {}", first.name),
                &format!("Logs for {}", first.name),
                76,
                &reason,
            ));
        }
    }

    // Directory depth awareness: suggest opening project root when deep in subdirectories
    if let (Some(cwd), Some(project)) = (&ctx.cwd, &ctx.project)
        && depth_below_root(cwd, &project.root) >= 1
    {
        let name = project.root.rsplit('/').next().unwrap_or("project");
        let reason = SuggestionReason::DirectoryDepth {
            project_name: name.into(),
        };
        items.push(completion(
            &format!("open {}", project.root),
            &format!("Open project root ({name})"),
            91,
            &reason,
        ));
    }

    // Sort by score descending, then truncate to 20 most relevant
    items.sort_by(|a, b| b.score.cmp(&a.score));
    items.truncate(20);
    items
}

/// How many directory levels `cwd` is below `project_root`.
fn depth_below_root(cwd: &str, project_root: &str) -> usize {
    use std::path::Path;
    let cwd_path = Path::new(cwd);
    let root_path = Path::new(project_root);
    cwd_path
        .strip_prefix(root_path)
        .map(|rel| rel.components().count())
        .unwrap_or(0)
}

fn completion(
    label: &str,
    _description: &str,
    score: u16,
    reason: &SuggestionReason,
) -> CompletionItem {
    let reason_str = reason.user_reason();
    CompletionItem {
        label: label.to_string(),
        icon_path: Some("__context__".to_string()),
        score,
        description: Some(reason_str.clone()),
        reason: Some(reason_str),
    }
}
