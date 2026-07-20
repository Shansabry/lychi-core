//! Suggestion providers — small rule bricks that propose candidates with
//! relevance priors. Ranking, learning, gating live in `mod.rs`.

use std::sync::Arc;

use redb::Database;

use crate::action_registry::CompletionItem;
use crate::context::clipboard_detect::ClipboardContentType;
use crate::context::{EnvironmentContext, ProjectKind};
use crate::db::frecency;

use super::SuggestionReason;

/// A proposed suggestion: a concrete, honest command plus provenance.
pub struct Candidate {
    /// Exactly what lands in the input / executes on Enter.
    pub command: String,
    /// Human explanation of what the command does.
    pub description: String,
    /// Provider-assigned prior (0–100); learned boost is added by the ranker.
    pub relevance: u16,
    pub reason: SuggestionReason,
}

impl Candidate {
    fn new(
        command: impl Into<String>,
        description: impl Into<String>,
        relevance: u16,
        reason: SuggestionReason,
    ) -> Self {
        Self {
            command: command.into(),
            description: description.into(),
            relevance,
            reason,
        }
    }

    pub fn into_completion_item(self) -> CompletionItem {
        CompletionItem {
            label: self.command,
            icon_path: Some("__context__".to_string()),
            score: self.relevance,
            description: Some(self.description),
            reason: Some(self.reason.user_reason()),
            thumb_b64: None,
            ..Default::default()
        }
    }
}

/// Everything a provider may look at.
pub struct SuggestCtx<'a> {
    pub env: &'a EnvironmentContext,
    pub db: Option<&'a Arc<Database>>,
    /// Focused window is a terminal or IDE.
    pub in_dev_window: bool,
}

/// When a provider's candidates are eligible to appear.
///
/// The zero-state (empty input) shows only what the user has actually used or
/// explicitly did — never speculative context-derived commands. Those surface
/// only once the user types a related keyword. This is the industry-standard
/// split (Raycast/Alfred/Chrome ZPS): recents cold, context after intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderTier {
    /// May appear on the empty prompt — usage-driven or explicit-action only
    /// (workspace memory, clipboard).
    ColdEligible,
    /// Appears only after the typed input matches this provider's domain
    /// (git/docker/project/navigation context actions).
    TypedOnly,
}

pub trait SuggestionProvider: Send + Sync {
    /// Stable id, used for the per-provider cap and debugging.
    fn id(&self) -> &'static str;
    /// Providers that apply outside dev windows too (default: dev-only).
    fn universal(&self) -> bool {
        false
    }
    /// When this provider's candidates may show. Default: typed-only, so a new
    /// provider never leaks speculative commands onto the empty prompt.
    fn tier(&self) -> ProviderTier {
        ProviderTier::TypedOnly
    }
    /// Domain vocabulary for typed matching beyond literal command substrings
    /// (e.g. docker → ["container", "image"]). Lets "cont" surface docker
    /// actions. Structural map on the trait — decides *candidacy*, never order
    /// (learned CTR decides order). Default: none.
    fn keywords(&self) -> &'static [&'static str] {
        &[]
    }
    fn suggest(&self, ctx: &SuggestCtx) -> Vec<Candidate>;
}

/// The provider registry. Order is irrelevant — the ranker sorts.
pub fn providers() -> &'static [Box<dyn SuggestionProvider>] {
    static PROVIDERS: std::sync::OnceLock<Vec<Box<dyn SuggestionProvider>>> =
        std::sync::OnceLock::new();
    PROVIDERS.get_or_init(|| {
        vec![
            Box::new(ClipboardProvider),
            Box::new(GitProvider),
            Box::new(ProjectProvider),
            Box::new(DockerProvider),
            Box::new(NavigationProvider),
            Box::new(MemoryProvider),
        ]
    })
}

// ── Clipboard (universal) ───────────────────────────────────────────────

struct ClipboardProvider;

impl SuggestionProvider for ClipboardProvider {
    fn id(&self) -> &'static str {
        "clipboard"
    }
    fn universal(&self) -> bool {
        true
    }
    fn tier(&self) -> ProviderTier {
        // Copying is an explicit recent user action (high signal), not ambient
        // state — so clipboard actions may show on the empty prompt.
        ProviderTier::ColdEligible
    }

    fn suggest(&self, ctx: &SuggestCtx) -> Vec<Candidate> {
        let Some(ref clip) = ctx.env.clipboard else {
            return Vec::new();
        };
        let reason = |t: &str| SuggestionReason::ClipboardContent {
            content_type: t.into(),
        };
        match clip {
            ClipboardContentType::Url(url) => {
                let display = if url.chars().count() > 60 {
                    format!("{}…", url.chars().take(57).collect::<String>())
                } else {
                    url.clone()
                };
                vec![Candidate::new(
                    format!("open {url}"),
                    format!("Open {display}"),
                    70,
                    reason("URL"),
                )]
            }
            ClipboardContentType::FilePath(path) => {
                let name = path.rsplit('/').next().unwrap_or(path);
                vec![Candidate::new(
                    format!("open {path}"),
                    format!("Open {name}"),
                    70,
                    reason("file path"),
                )]
            }
            ClipboardContentType::IpAddress(ip) => vec![Candidate::new(
                format!("run ping -c 4 {ip}"),
                format!("Ping {ip}"),
                65,
                reason("IP address"),
            )],
            ClipboardContentType::GitHash(hash) if ctx.env.git.is_some() => {
                let short = &hash[..7.min(hash.len())];
                vec![Candidate::new(
                    format!("run git show {hash}"),
                    format!("Show commit {short}"),
                    65,
                    reason("git hash"),
                )]
            }
            // Honest payload: search the actual error line, not the words
            // "clipboard error".
            ClipboardContentType::ErrorTrace(msg) => vec![Candidate::new(
                format!("web {msg}"),
                "Search this error",
                65,
                reason("error"),
            )],
            ClipboardContentType::Json => vec![Candidate::new(
                "clip",
                "Clipboard contains JSON",
                60,
                reason("JSON"),
            )],
            // UUID / Plain / non-git hash: no useful action
            _ => Vec::new(),
        }
    }
}

// ── Git ─────────────────────────────────────────────────────────────────

struct GitProvider;

impl SuggestionProvider for GitProvider {
    fn id(&self) -> &'static str {
        "git"
    }
    fn keywords(&self) -> &'static [&'static str] {
        &[
            "commit", "branch", "stash", "diff", "status", "push", "pull",
        ]
    }

    fn suggest(&self, ctx: &SuggestCtx) -> Vec<Candidate> {
        let Some(ref git) = ctx.env.git else {
            return Vec::new();
        };
        let feature_branch = match git.branch.as_str() {
            "main" | "master" | "develop" => None,
            b => Some(b.to_string()),
        };
        let reason = |fallback: SuggestionReason| match &feature_branch {
            Some(branch) => SuggestionReason::GitFeatureBranch {
                branch: branch.clone(),
            },
            None => fallback,
        };

        if git.dirty {
            [
                ("git commit", "Commit staged changes", 100u16),
                ("git diff", "View uncommitted changes", 95),
                ("git status", "Show working tree status", 88),
                ("git stash", "Stash current changes", 85),
            ]
            .into_iter()
            .map(|(cmd, desc, score)| {
                Candidate::new(cmd, desc, score, reason(SuggestionReason::GitDirty))
            })
            .collect()
        } else {
            [
                ("git pull", "Pull latest changes", 100u16),
                ("git push", "Push commits to remote", 95),
            ]
            .into_iter()
            .map(|(cmd, desc, score)| {
                Candidate::new(cmd, desc, score, reason(SuggestionReason::GitClean))
            })
            .collect()
        }
    }
}

// ── Project (install + scripts + workspace scripts) ────────────────────

struct ProjectProvider;

impl SuggestionProvider for ProjectProvider {
    fn id(&self) -> &'static str {
        "project"
    }
    fn keywords(&self) -> &'static [&'static str] {
        &[
            "deps",
            "dependencies",
            "packages",
            "install",
            "build",
            "dev",
            "test",
            "script",
        ]
    }

    fn suggest(&self, ctx: &SuggestCtx) -> Vec<Candidate> {
        let Some(ref project) = ctx.env.project else {
            return Vec::new();
        };
        let mut out = Vec::new();

        if project.kind == ProjectKind::Node {
            let pm = project.package_manager.as_deref().unwrap_or("npm");
            out.push(Candidate::new(
                format!("run {pm} install"),
                "Install dependencies",
                86,
                SuggestionReason::ProjectInstall { pm: pm.into() },
            ));
        }

        for (i, script) in project.scripts.iter().enumerate() {
            let command = if script.name.is_empty() {
                format!("run {}", script.runner)
            } else {
                format!("run {} {}", script.runner, script.name)
            };
            out.push(Candidate::new(
                command,
                format!("Run {} {}", script.runner, script.name),
                90u16.saturating_sub(i as u16).max(84),
                SuggestionReason::ProjectScript {
                    runner: script.runner.clone(),
                },
            ));
        }

        for (i, script) in project.workspace_scripts.iter().enumerate() {
            let base = if script.name.is_empty() {
                format!("run {}", script.runner)
            } else {
                format!("run {} {}", script.runner, script.name)
            };
            out.push(Candidate::new(
                format!("{base} (workspace)"),
                format!("Run {} {} at workspace root", script.runner, script.name),
                82u16.saturating_sub(i as u16).max(70),
                SuggestionReason::ProjectScript {
                    runner: format!("{} (workspace)", script.runner),
                },
            ));
        }

        out
    }
}

// ── Docker (compose + running containers) ──────────────────────────────

struct DockerProvider;

impl SuggestionProvider for DockerProvider {
    fn id(&self) -> &'static str {
        "docker"
    }
    fn keywords(&self) -> &'static [&'static str] {
        &[
            "container",
            "containers",
            "image",
            "images",
            "compose",
            "service",
            "logs",
        ]
    }

    fn suggest(&self, ctx: &SuggestCtx) -> Vec<Candidate> {
        let mut out = Vec::new();

        if ctx.env.project.as_ref().is_some_and(|p| p.has_compose) {
            for (cmd, desc, score) in [
                ("run docker compose up -d", "Start all services", 82u16),
                ("run docker compose down", "Stop all services", 80),
                ("run docker compose logs", "View service logs", 78),
            ] {
                out.push(Candidate::new(
                    cmd,
                    desc,
                    score,
                    SuggestionReason::DockerCompose,
                ));
            }
        }

        if let Some(ref docker) = ctx.env.docker
            && !docker.containers.is_empty()
        {
            let reason = SuggestionReason::DockerRunning {
                count: docker.containers.len(),
            };
            out.push(Candidate::new(
                "run docker ps",
                "List running containers",
                77,
                reason.clone(),
            ));
            if let Some(first) = docker.containers.first() {
                out.push(Candidate::new(
                    format!("run docker logs {}", first.name),
                    format!("Logs for {}", first.name),
                    76,
                    reason,
                ));
            }
        }

        out
    }
}

// ── Navigation (project root, pinned workspace) ─────────────────────────

struct NavigationProvider;

impl SuggestionProvider for NavigationProvider {
    fn id(&self) -> &'static str {
        "navigation"
    }
    fn keywords(&self) -> &'static [&'static str] {
        &["root", "project", "workspace", "unpin", "pin"]
    }

    fn suggest(&self, ctx: &SuggestCtx) -> Vec<Candidate> {
        let mut out = Vec::new();

        if let (Some(cwd), Some(project)) = (&ctx.env.cwd, &ctx.env.project)
            && depth_below_root(cwd, &project.root) >= 1
        {
            let name = project.root.rsplit('/').next().unwrap_or("project");
            out.push(Candidate::new(
                format!("open {}", project.root),
                format!("Open project root ({name})"),
                91,
                SuggestionReason::DirectoryDepth {
                    project_name: name.into(),
                },
            ));
        }

        if crate::context::pin::get().is_some() {
            out.push(Candidate::new(
                "pin workspace clear",
                "Unpin workspace",
                75,
                SuggestionReason::PinnedWorkspace,
            ));
        }

        out
    }
}

/// How many directory levels `cwd` is below `project_root`.
fn depth_below_root(cwd: &str, project_root: &str) -> usize {
    use std::path::Path;
    Path::new(cwd)
        .strip_prefix(Path::new(project_root))
        .map(|rel| rel.components().count())
        .unwrap_or(0)
}

// ── Workspace memory (commands previously run in this project) ─────────

struct MemoryProvider;

impl SuggestionProvider for MemoryProvider {
    fn id(&self) -> &'static str {
        "memory"
    }
    fn tier(&self) -> ProviderTier {
        // Purely usage-driven: commands the user actually ran in this
        // workspace. The canonical cold-eligible source.
        ProviderTier::ColdEligible
    }

    fn suggest(&self, ctx: &SuggestCtx) -> Vec<Candidate> {
        let Some(db) = ctx.db else {
            return Vec::new();
        };
        let root = ctx
            .env
            .project
            .as_ref()
            .map(|p| p.root.as_str())
            .or(ctx.env.cwd.as_deref());
        let Some(root) = root else {
            return Vec::new();
        };

        let scores = frecency::get_workspace_scores(db, root);
        if scores.is_empty() {
            return Vec::new();
        }
        let project_name = root.rsplit('/').next().unwrap_or("project").to_string();

        let mut ranked: Vec<_> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        ranked
            .into_iter()
            .take(5)
            .enumerate()
            .map(|(i, (command, _score))| {
                Candidate::new(
                    command.clone(),
                    command,
                    72u16.saturating_sub(i as u16),
                    SuggestionReason::WorkspaceMemory {
                        project: project_name.clone(),
                    },
                )
            })
            .collect()
    }
}
