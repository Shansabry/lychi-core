//! Suggestion providers — small rule bricks that propose candidates with
//! relevance priors. Ranking, learning, gating live in `mod.rs`.

use std::sync::Arc;

use redb::Database;

use crate::action_registry::CompletionItem;
use crate::context::EnvironmentContext;
use crate::context::clipboard_detect::ClipboardContentType;
use crate::db::frecency;

use super::SuggestionReason;

/// A proposed suggestion: a concrete, honest command plus provenance.
pub struct Candidate {
    /// Exactly what executes on Enter.
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
            label: self.command.clone(),
            icon_path: Some("__context__".to_string()),
            score: self.relevance,
            description: Some(self.description),
            reason: Some(self.reason.user_reason()),
            thumb_b64: None,
            // `run` carries the REAL command. Without it the row falls back to
            // executing its label, which for a scoped git row would drop the
            // `-C` and run in whatever directory the shell resolves — the exact
            // wrong-repo failure the scoping exists to prevent.
            run: Some(self.command),
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

/// Minimum recorded uses before a workspace command may surface as a
/// suggestion. See the gate in [`MemoryProvider::suggest`].
pub const MIN_WORKSPACE_USES: u32 = 2;

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
        // Git, project and docker verb-guessing used to live here. They
        // proposed commands from a hardcoded verb list — `git pull`, `npm
        // install`, `docker ps` — whether or not the user wanted them, which
        // buried real results under speculation and still missed whatever was
        // actually being typed.
        //
        // What remains never invents a command:
        //   - clipboard  — acts on what IS on the clipboard right now
        //   - navigation — facts about the current workspace (open root, unpin)
        //   - memory     — commands the user actually ran here before
        //
        // Commands the user types are resolved to a target by
        // `Executor::multi_repo_rows`, which offers one row per repo. The
        // command is theirs; only the target needs picking.
        vec![
            Box::new(ClipboardProvider),
            Box::new(NavigationProvider),
            Box::new(MemoryProvider),
        ]
    })
}

/// Is this workspace-memory command just launching an app?
///
/// Matched against the app index rather than by parsing the verb: `open` is
/// also how files and URLs are opened, and only the index can tell
/// `open Spotify` (an app `recent_apps` will offer properly) from
/// `open ./notes.md` (a file, which it will not).
fn is_app_launch(command: &str) -> bool {
    command
        .strip_prefix("open ")
        .map(str::trim)
        .filter(|rest| !rest.is_empty())
        .is_some_and(|app| {
            crate::desktop_apps::app_index()
                .by_name_exact(&app.to_lowercase())
                .is_some()
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

/// The clipboard row for the zero state — [`ClipboardProvider`] run once,
/// outside the provider registry walk. One classifier, one row shape, whether
/// the row is reached from typed matching or the empty prompt.
pub(super) fn clipboard_candidate(
    env: &EnvironmentContext,
    db: Option<&Arc<Database>>,
) -> Option<Candidate> {
    let ctx = SuggestCtx {
        env,
        db,
        in_dev_window: env
            .active_window
            .as_ref()
            .is_some_and(|w| w.is_terminal || w.is_ide),
    };
    ClipboardProvider.suggest(&ctx).into_iter().next()
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

        // The ≥2-uses quality bar lives HERE, so every consumer of workspace
        // memory inherits it: a command run once is an event, not a habit, and
        // a one-off `kill 1234` haunting the empty prompt for a week was the
        // reported garbage. Counts, not scores — a single use minutes ago
        // outscores five uses last week.
        let scores: std::collections::HashMap<String, f64> =
            frecency::get_workspace_stats(db, root)
                .into_iter()
                .filter(|(_, (_, count))| *count >= MIN_WORKSPACE_USES)
                .map(|(cmd, (score, _))| (cmd, score))
                .collect();
        if scores.is_empty() {
            return Vec::new();
        }
        let project_name = root.rsplit('/').next().unwrap_or("project").to_string();

        let mut ranked: Vec<_> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        ranked
            .into_iter()
            // Drop app launches. `recent_apps` already offers these as real app
            // rows — display name, real icon, one keystroke — and a `Candidate`
            // cannot carry an icon (`into_completion_item` hardcodes
            // `__context__`), so anything emitted here is necessarily the raw
            // command text. Showing "open Xfce Terminal" as a lightning-bolt row
            // above the Xfce Terminal app row is the duplicate the app zero
            // state exists to remove.
            //
            // Workspace memory keeps everything else — `cargo test`, `npm run
            // dev` — which is what it is genuinely good at: commands, not
            // targets.
            .filter(|(command, _)| !is_app_launch(command))
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
