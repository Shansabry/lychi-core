//! Contextual completion suggestions based on detected environment.
//!
//! Maps `EnvironmentContext` → `Vec<CompletionItem>` to show relevant
//! commands when the user opens Lychi with an empty input.
//!
//! Each suggestion carries a structured `SuggestionReason` explaining *why*
//! it was suggested. The reason is serialized as a user-facing string on the
//! `CompletionItem::reason` field for frontend display.

use std::sync::Arc;

use redb::Database;

use crate::action_registry::CompletionItem;
use crate::db::frecency;

use super::app_class::{self, AppClass};
use super::browser_context::{self, BrowserContext};
use super::clipboard_detect::ClipboardContentType;
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
    /// Clipboard contains actionable content.
    ClipboardContent { content_type: String },
    /// Command previously run in this workspace.
    WorkspaceMemory { project: String },
    /// Suggestion based on focused browser window.
    BrowserContext { detail: String },
    /// Generic app-class suggestion (media, file manager, etc.).
    AppClassSuggestion { app: String },
    /// Pinned workspace is active or detection failed (pin hint).
    PinnedWorkspace,
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
            Self::ClipboardContent { content_type } => format!("Clipboard: {content_type}"),
            Self::WorkspaceMemory { project } => format!("Recent in {project}"),
            Self::BrowserContext { detail } => detail.clone(),
            Self::AppClassSuggestion { app } => app.clone(),
            Self::PinnedWorkspace => "Pinned workspace".into(),
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
            Self::ClipboardContent { content_type } => {
                format!("clipboard.type={content_type}")
            }
            Self::WorkspaceMemory { project } => format!("workspace={project}"),
            Self::BrowserContext { detail } => format!("browser={detail}"),
            Self::AppClassSuggestion { app } => format!("app_class={app}"),
            Self::PinnedWorkspace => "workspace.pinned=true".into(),
        }
    }
}

/// Generate contextual completions based on current environment.
///
/// Returns up to 20 suggestions based on detected context.
/// Pass `db` to enable workspace-local command memory (None in tests).
pub fn suggest(ctx: &EnvironmentContext, db: Option<&Arc<Database>>) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    // Determine if we're in a dev window (terminal/IDE)
    let in_dev_window = ctx
        .active_window
        .as_ref()
        .is_some_and(|w| w.is_terminal || w.is_ide);

    // ── 1. Clipboard suggestions (universal — all window types) ─────────
    suggest_clipboard(ctx, &mut items);

    // ── 2. Dev-window suggestions (git, project, docker, workspace memory) ──
    if in_dev_window {
        suggest_dev(ctx, db, &mut items);
    } else {
        // ── 3. App-class suggestions (browser, media, file manager) ─────
        suggest_for_app_class(ctx, &mut items);
    }

    // Sort by score descending, then truncate to 20 most relevant
    items.sort_by(|a, b| b.score.cmp(&a.score));
    items.truncate(20);
    items
}

// ── Clipboard Suggestions ───────────────────────────────────────────────

fn suggest_clipboard(ctx: &EnvironmentContext, items: &mut Vec<CompletionItem>) {
    let Some(ref clip_type) = ctx.clipboard else {
        return;
    };

    match clip_type {
        ClipboardContentType::Url(url) => {
            // Truncate display URL for readability
            let display = if url.len() > 60 {
                format!("{}…", &url[..57])
            } else {
                url.clone()
            };
            let reason = SuggestionReason::ClipboardContent {
                content_type: "URL".into(),
            };
            items.push(completion(
                &format!("open {url}"),
                &format!("Open {display}"),
                70,
                &reason,
            ));
        }
        ClipboardContentType::FilePath(path) => {
            let name = path.rsplit('/').next().unwrap_or(path);
            let reason = SuggestionReason::ClipboardContent {
                content_type: "file path".into(),
            };
            items.push(completion(
                &format!("open {path}"),
                &format!("Open {name}"),
                70,
                &reason,
            ));
        }
        ClipboardContentType::IpAddress(ip) => {
            let reason = SuggestionReason::ClipboardContent {
                content_type: "IP address".into(),
            };
            items.push(completion(
                &format!("run ping -c 4 {ip}"),
                &format!("Ping {ip}"),
                65,
                &reason,
            ));
        }
        ClipboardContentType::GitHash(hash) => {
            // Only suggest git show if we have git context
            if ctx.git.is_some() {
                let short = &hash[..7.min(hash.len())];
                let reason = SuggestionReason::ClipboardContent {
                    content_type: "git hash".into(),
                };
                items.push(completion(
                    &format!("run git show {hash}"),
                    &format!("Show commit {short}"),
                    65,
                    &reason,
                ));
            }
        }
        ClipboardContentType::ErrorTrace => {
            let reason = SuggestionReason::ClipboardContent {
                content_type: "error".into(),
            };
            items.push(completion(
                "web clipboard error",
                "Search this error",
                65,
                &reason,
            ));
        }
        ClipboardContentType::Json => {
            let reason = SuggestionReason::ClipboardContent {
                content_type: "JSON".into(),
            };
            items.push(completion("clip", "Clipboard contains JSON", 60, &reason));
        }
        // UUID and Plain don't have useful contextual actions
        ClipboardContentType::Uuid(_) | ClipboardContentType::Plain => {}
    }
}

// ── Dev-Window Suggestions ──────────────────────────────────────────────

fn suggest_dev(
    ctx: &EnvironmentContext,
    db: Option<&Arc<Database>>,
    items: &mut Vec<CompletionItem>,
) {
    // Track labels we add so workspace memory can dedup
    let mut added_labels: Vec<String> = Vec::new();

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
    if let Some(ref git) = ctx.git {
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
            for (label, desc, score) in [
                ("git commit", "Commit staged changes", 100u16),
                ("git diff", "View uncommitted changes", 95),
                ("git status", "Show working tree status", 88),
                ("git stash", "Stash current changes", 85),
            ] {
                added_labels.push(label.to_string());
                items.push(completion(label, desc, score, &dirty_reason));
            }
        } else {
            for (label, desc, score) in [
                ("git pull", "Pull latest changes", 100u16),
                ("git push", "Push commits to remote", 95),
            ] {
                added_labels.push(label.to_string());
                items.push(completion(label, desc, score, &clean_reason));
            }
        }
    }

    // Project-specific install commands
    if let Some(ref project) = ctx.project {
        let pm = project.package_manager.as_deref().unwrap_or("npm");
        if project.kind == ProjectKind::Node {
            let label = format!("run {pm} install");
            let reason = SuggestionReason::ProjectInstall { pm: pm.into() };
            added_labels.push(label.clone());
            items.push(completion(&label, "Install dependencies", 86, &reason));
        }
    }

    // Discovered project scripts
    if let Some(ref project) = ctx.project {
        for (i, script) in project.scripts.iter().enumerate() {
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
            added_labels.push(label.clone());
            items.push(completion(&label, &desc, score.max(84), &reason));
        }
    }

    // Workspace-root scripts (monorepo) — lower score, always labelled (workspace)
    if let Some(ref project) = ctx.project {
        for (i, script) in project.workspace_scripts.iter().enumerate() {
            let score = 82u16.saturating_sub(i as u16);
            let base_label = if script.name.is_empty() {
                format!("run {}", script.runner)
            } else {
                format!("run {} {}", script.runner, script.name)
            };
            let label = format!("{base_label} (workspace)");
            let desc = if script.name.is_empty() {
                format!("{} (workspace)", script.runner)
            } else {
                format!("{} {} (workspace)", script.runner, script.name)
            };
            let reason = SuggestionReason::ProjectScript {
                runner: format!("{} (workspace)", script.runner),
            };
            added_labels.push(label.clone());
            items.push(completion(&label, &desc, score.max(70), &reason));
        }
    }

    // Pinned workspace — show unpin option when active
    if super::pin::get().is_some() {
        let reason = SuggestionReason::PinnedWorkspace;
        let label = "pin workspace clear";
        added_labels.push(label.to_string());
        items.push(completion(label, "Unpin workspace", 75, &reason));
    }

    // Docker Compose suggestions (project-level)
    if let Some(ref project) = ctx.project
        && project.has_compose
    {
        let reason = SuggestionReason::DockerCompose;
        for (label, desc, score) in [
            ("run docker compose up -d", "Start all services", 82u16),
            ("run docker compose down", "Stop all services", 80),
            ("run docker compose logs", "View service logs", 78),
        ] {
            added_labels.push(label.to_string());
            items.push(completion(label, desc, score, &reason));
        }
    }

    // Docker container suggestions
    if let Some(ref docker) = ctx.docker
        && !docker.containers.is_empty()
    {
        let reason = SuggestionReason::DockerRunning {
            count: docker.containers.len(),
        };
        added_labels.push("run docker ps".to_string());
        items.push(completion(
            "run docker ps",
            "List running containers",
            77,
            &reason,
        ));
        if let Some(first) = docker.containers.first() {
            let label = format!("run docker logs {}", first.name);
            added_labels.push(label.clone());
            items.push(completion(
                &label,
                &format!("Logs for {}", first.name),
                76,
                &reason,
            ));
        }
    }

    // Directory depth awareness
    if let (Some(cwd), Some(project)) = (&ctx.cwd, &ctx.project)
        && depth_below_root(cwd, &project.root) >= 1
    {
        let name = project.root.rsplit('/').next().unwrap_or("project");
        let reason = SuggestionReason::DirectoryDepth {
            project_name: name.into(),
        };
        let label = format!("open {}", project.root);
        added_labels.push(label.clone());
        items.push(completion(
            &label,
            &format!("Open project root ({name})"),
            91,
            &reason,
        ));
    }

    // Workspace-local command memory (frecency-based)
    if let Some(db) = db {
        suggest_workspace_memory(ctx, db, &added_labels, items);
    }
}

/// Add workspace memory suggestions — commands frequently run in this project.
fn suggest_workspace_memory(
    ctx: &EnvironmentContext,
    db: &Arc<Database>,
    already_added: &[String],
    items: &mut Vec<CompletionItem>,
) {
    let project_root = ctx
        .project
        .as_ref()
        .map(|p| p.root.as_str())
        .or(ctx.cwd.as_deref());

    let Some(root) = project_root else {
        return;
    };

    let scores = frecency::get_workspace_scores(db, root);
    if scores.is_empty() {
        return;
    }

    let project_name = root.rsplit('/').next().unwrap_or("project");

    // Sort by score descending, take top 5, skip already-suggested commands
    let mut ranked: Vec<_> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut count = 0;
    for (command, _score) in ranked {
        if count >= 5 {
            break;
        }
        // Dedup against commands already in the suggestion list
        if already_added.contains(&command) {
            continue;
        }
        let reason = SuggestionReason::WorkspaceMemory {
            project: project_name.into(),
        };
        items.push(completion(
            &command,
            &command,
            72u16.saturating_sub(count as u16),
            &reason,
        ));
        count += 1;
    }
}

// ── App-Class Suggestions ───────────────────────────────────────────────

fn suggest_for_app_class(ctx: &EnvironmentContext, items: &mut Vec<CompletionItem>) {
    let Some(ref window) = ctx.active_window else {
        return;
    };

    let app_class = app_class::classify(&window.wm_class);

    match app_class {
        AppClass::Browser => suggest_browser(ctx, &window.title, items),
        AppClass::MediaPlayer => suggest_media(items),
        AppClass::FileManager => suggest_file_manager(items),
        // Terminal and IDE are handled by suggest_dev; Communication and Unknown
        // have no specific suggestions yet.
        _ => {}
    }
}

fn suggest_browser(ctx: &EnvironmentContext, title: &str, items: &mut Vec<CompletionItem>) {
    let browser_ctx = browser_context::parse_title(title);

    match browser_ctx {
        BrowserContext::GitHub { owner, repo } => {
            let reason = SuggestionReason::BrowserContext {
                detail: format!("{owner}/{repo}"),
            };
            items.push(completion(
                &format!("run git clone https://github.com/{owner}/{repo}.git"),
                &format!("Clone {owner}/{repo}"),
                75,
                &reason,
            ));
            items.push(completion(
                &format!("web https://github.com/{owner}/{repo}/issues"),
                &format!("{owner}/{repo} issues"),
                72,
                &reason,
            ));
            items.push(completion(
                &format!("web https://github.com/{owner}/{repo}/pulls"),
                &format!("{owner}/{repo} pull requests"),
                70,
                &reason,
            ));
        }
        BrowserContext::Localhost { port } => {
            let reason = SuggestionReason::BrowserContext {
                detail: format!("localhost:{port}"),
            };
            // If docker context is available, suggest container management
            if let Some(ref docker) = ctx.docker
                && !docker.containers.is_empty()
            {
                items.push(completion(
                    "run docker ps",
                    "List running containers",
                    73,
                    &reason,
                ));
                if let Some(first) = docker.containers.first() {
                    items.push(completion(
                        &format!("run docker logs {}", first.name),
                        &format!("Logs for {}", first.name),
                        71,
                        &reason,
                    ));
                }
            }
            items.push(completion(
                &format!("web http://localhost:{port}"),
                &format!("Open localhost:{port}"),
                70,
                &reason,
            ));
        }
        BrowserContext::StackOverflow => {
            let reason = SuggestionReason::BrowserContext {
                detail: "Stack Overflow".into(),
            };
            items.push(completion("web", "Search the web", 68, &reason));
        }
        BrowserContext::Documentation => {
            let reason = SuggestionReason::BrowserContext {
                detail: "Documentation".into(),
            };
            items.push(completion("web", "Search documentation", 68, &reason));
        }
        BrowserContext::Unknown => {
            // Generic browser suggestions
            let reason = SuggestionReason::AppClassSuggestion {
                app: "Browser".into(),
            };
            items.push(completion("web", "Search the web", 65, &reason));
        }
    }
}

fn suggest_media(items: &mut Vec<CompletionItem>) {
    let reason = SuggestionReason::AppClassSuggestion {
        app: "Media".into(),
    };
    items.push(completion("media toggle", "Play / Pause", 68, &reason));
    items.push(completion("media next", "Next track", 66, &reason));
    items.push(completion("media prev", "Previous track", 64, &reason));
}

fn suggest_file_manager(items: &mut Vec<CompletionItem>) {
    let reason = SuggestionReason::AppClassSuggestion {
        app: "File manager".into(),
    };
    items.push(completion("open ~", "Open home directory", 68, &reason));
    items.push(completion(
        "open ~/Downloads",
        "Open Downloads",
        66,
        &reason,
    ));
    items.push(completion(
        "open ~/Documents",
        "Open Documents",
        64,
        &reason,
    ));
}

// ── Helpers ─────────────────────────────────────────────────────────────

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
