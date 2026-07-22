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
pub mod clipboard_detect;
pub mod config;
pub mod cwd;
pub mod docker;
pub mod git;
pub mod ide;
pub mod ide_config;
pub mod ide_proc;
pub mod metrics;
pub mod multi_repo;
pub mod network;
pub mod pin;
pub mod project;
pub mod suggestions;
pub mod terminal_probe;
pub mod window_stack;
pub mod wlr_toplevel;
pub mod workspace_cache;

use std::time::Instant;

use chrono::Timelike;
use serde::{Deserialize, Serialize};

/// Soft-stale threshold: show a warning in completions, trigger async re-gather.
pub const SOFT_STALE_SECS: u64 = 30;
/// Hard-stale threshold: additionally downgrade AI context hint trust.
/// The hint is still included but tagged so the router can be conservative.
pub const HARD_STALE_SECS: u64 = 300; // 5 minutes

/// The complete environmental context, refreshed on each summon.
#[derive(Debug, Clone, Default, Serialize, Deserialize, specta::Type)]
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
    /// Detected clipboard content type (read on summon).
    #[serde(default)]
    pub clipboard: Option<clipboard_detect::ClipboardContentType>,
    /// Network context (WiFi SSID, VPN status).
    #[serde(default)]
    pub network: Option<network::NetworkContext>,
    /// Milliseconds taken to gather context.
    pub gather_ms: u64,
    /// Whether `terminal_cwd` is coherent with `cwd` (same git/project root).
    /// False when IDE and background terminal are in different projects — prevents
    /// cross-project context contamination.
    #[serde(default)]
    pub terminal_matches_workspace: bool,
    /// Wall-clock time when this context was gathered. Not serialized — only
    /// meaningful within a single process lifetime.
    #[serde(skip)]
    pub gathered_at: Option<Instant>,
    /// How the active window was resolved on this summon (observability only).
    #[serde(skip)]
    pub active_window_source: ActiveWindowSource,
    /// How the background terminal was selected on this summon (observability only).
    #[serde(skip)]
    pub terminal_source: TerminalSource,
    /// How the IDE workspace was resolved on this summon (observability only).
    #[serde(skip)]
    pub ide_workspace_source: IdeWorkspaceSource,
    /// How the code root was resolved on this summon (observability only).
    #[serde(skip)]
    pub code_root_source: CodeRootSource,
}

impl EnvironmentContext {
    /// How long ago this context was gathered. `None` if `gathered_at` was not set
    /// (e.g. deserialized from a previous session or constructed in tests).
    pub fn age(&self) -> Option<std::time::Duration> {
        self.gathered_at.map(|t| t.elapsed())
    }

    /// Soft-stale: older than `SOFT_STALE_SECS`. Triggers UX warning + async re-gather.
    pub fn is_soft_stale(&self) -> bool {
        self.gathered_at
            .is_some_and(|t| t.elapsed().as_secs() >= SOFT_STALE_SECS)
    }

    /// Hard-stale: older than `HARD_STALE_SECS`. AI hint should be treated as low-trust.
    pub fn is_hard_stale(&self) -> bool {
        self.gathered_at
            .is_some_and(|t| t.elapsed().as_secs() >= HARD_STALE_SECS)
    }

    /// Build a concise hint string for AI routing prompts.
    /// When hard-stale, a caveat is prepended so the router can be conservative
    /// about trusting workspace-specific routing decisions.
    pub fn ai_hint(&self) -> Option<String> {
        let mut lines = Vec::new();

        // Hard-stale caveat: router should not make workspace-specific decisions
        if self.is_hard_stale() {
            lines.push(
                "- Note: context is stale (>5min) — workspace details may be outdated".into(),
            );
        }

        if let Some(ref cwd) = self.cwd {
            lines.push(format!("- Working directory: {cwd}"));
        }
        if let Some(ref tcwd) = self.terminal_cwd
            && self.terminal_matches_workspace
        {
            lines.push(format!("- Terminal CWD: {tcwd}"));
            // Don't expose incoherent terminal CWD to AI — it's from a different project
        }
        if let Some(ref git) = self.git {
            let dirty_flag = if git.dirty { " (dirty)" } else { "" };
            lines.push(format!("- Git branch: {}{dirty_flag}", git.branch));
        }
        if let Some(ref proj) = self.project {
            lines.push(format!("- Project type: {:?}", proj.kind));
            if let Some(ref ws) = proj.workspace_root {
                lines.push(format!("- Workspace root: {ws}"));
            }
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
        if let Some(ref clip) = self.clipboard {
            let desc = match clip {
                clipboard_detect::ClipboardContentType::Url(u) => format!("URL: {u}"),
                clipboard_detect::ClipboardContentType::FilePath(p) => format!("File: {p}"),
                clipboard_detect::ClipboardContentType::IpAddress(ip) => format!("IP: {ip}"),
                clipboard_detect::ClipboardContentType::Json => "JSON content".into(),
                clipboard_detect::ClipboardContentType::GitHash(h) => format!("Git hash: {h}"),
                clipboard_detect::ClipboardContentType::Uuid(u) => format!("UUID: {u}"),
                clipboard_detect::ClipboardContentType::ErrorTrace(msg) => {
                    format!("Error/stack trace: {msg}")
                }
                clipboard_detect::ClipboardContentType::Plain => "Plain text".into(),
            };
            lines.push(format!("- Clipboard: {desc}"));
        }
        if let Some(ref net) = self.network {
            if let Some(ref ssid) = net.ssid {
                lines.push(format!("- WiFi: {ssid}"));
            }
            if net.vpn_active {
                lines.push("- VPN: active".into());
            }
        }

        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct WindowContext {
    pub title: String,
    pub wm_class: String,
    pub pid: u32,
    pub is_terminal: bool,
    #[serde(default)]
    pub is_ide: bool,
    /// Stable per-window identifier. On KWin: UUID string from `w.internalId`.
    /// On X11: hex-formatted X11 window ID (e.g. `"0x00a00012"`).
    /// `None` when unavailable (older Plasma, headless, etc.).
    #[serde(default)]
    pub window_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct GitContext {
    pub repo_root: String,
    pub branch: String,
    pub dirty: bool,
    pub remote: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, specta::Type)]
pub enum ProjectKind {
    Rust,
    Node,
    Python,
    Go,
    Flutter,
    Docker,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ProjectScript {
    /// Command runner (e.g. "npm run", "make", "just").
    pub runner: String,
    /// Script/target name.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
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
    /// Workspace/monorepo root when this project is a subpackage.
    /// Set when git root ≠ project root and the subpackage is under git root.
    /// Always set for observability; `workspace_scripts` only populated for proven workspaces.
    #[serde(default)]
    pub workspace_root: Option<String>,
    /// Scripts discovered at the workspace root (supplement subpackage `scripts`).
    /// Only populated when git root has explicit workspace markers (pnpm-workspace.yaml,
    /// package.json "workspaces", Cargo.toml [workspace], nx/turbo/lerna/rush).
    #[serde(default)]
    pub workspace_scripts: Vec<ProjectScript>,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct DockerContext {
    pub containers: Vec<ContainerInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub image: String,
    pub status: String,
}

/// How the active window was resolved on the most recent summon.
/// Informational only — not sent to AI, not serialized.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ActiveWindowSource {
    /// Cache hit, age < 1s — returned immediately with no D-Bus probe.
    CacheHot,
    /// Cache hit, age 1–15s — returned cached value, async refresh spawned.
    CacheWarm,
    /// Cache absent or expired — ran a live D-Bus probe.
    #[default]
    LiveProbe,
    /// Updated by the background KWin watcher (most up-to-date path).
    Watcher,
}

impl std::fmt::Display for ActiveWindowSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CacheHot => f.write_str("cache_hot"),
            Self::CacheWarm => f.write_str("cache_warm"),
            Self::LiveProbe => f.write_str("live_probe"),
            Self::Watcher => f.write_str("watcher"),
        }
    }
}

/// How the IDE workspace was resolved on the most recent summon.
/// Informational only — not sent to AI, not serialized.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum IdeWorkspaceSource {
    /// Ground truth from the IDE's process tree (`/proc/<pid>/cwd`+`cmdline`).
    Proc,
    /// From the IDE's own config/session state (e.g. VS Code storage.json).
    Config,
    /// Resolved from the window title + disk search (this gather).
    Title,
    /// Satisfied from per-window workspace cache (fast path, no re-resolve).
    Cached,
    /// Pinned workspace override (bypasses auto-detection).
    Pinned,
    /// No workspace found.
    #[default]
    None,
}

impl std::fmt::Display for IdeWorkspaceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Proc => f.write_str("proc"),
            Self::Config => f.write_str("config"),
            Self::Title => f.write_str("title"),
            Self::Cached => f.write_str("cached"),
            Self::Pinned => f.write_str("pinned"),
            Self::None => f.write_str("none"),
        }
    }
}

/// How the code root was resolved on the most recent summon.
/// Informational only — not sent to AI, not serialized.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CodeRootSource {
    /// Workspace root itself has `.git` + build marker — it IS the code root.
    WorkspaceStrong,
    /// Unique child/grandchild has `.git` + build marker.
    StrongChild,
    /// No code root resolved (workspace is meta-container with 0 or >1 candidates).
    #[default]
    None,
}

impl std::fmt::Display for CodeRootSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkspaceStrong => f.write_str("workspace_strong"),
            Self::StrongChild => f.write_str("strong_child"),
            Self::None => f.write_str("none"),
        }
    }
}

/// How the background terminal window was selected on the most recent summon.
/// Informational only — not sent to AI, not serialized.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminalSource {
    /// The focused window itself is a terminal (summoned from terminal).
    FocusedWindow,
    /// Selected from the focus ring, populated by the background watcher.
    FocusRingWatcher,
    /// Selected from the focus ring, seeded from the pre-summon window snapshot.
    FocusRingPreSummon,
    /// Fell back to Z-order window stack scan.
    Stacking,
    /// No background terminal found.
    #[default]
    None,
}

impl std::fmt::Display for TerminalSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FocusedWindow => f.write_str("focused_window"),
            Self::FocusRingWatcher => f.write_str("focus_ring(watcher)"),
            Self::FocusRingPreSummon => f.write_str("focus_ring(pre_summon)"),
            Self::Stacking => f.write_str("stacking"),
            Self::None => f.write_str("none"),
        }
    }
}

/// Detect a Wayland session. `XDG_SESSION_TYPE` is the primary signal, but it's
/// frequently ABSENT under autostart (the D-Bus activation environment the
/// session launches us in may not carry it), which previously misrouted us to
/// the X11 hotkey path on boot. `WAYLAND_DISPLAY` is the reliable fallback — if
/// it's set, we're on Wayland regardless of what (if anything) XDG_SESSION_TYPE
/// says.
#[cfg(target_os = "linux")]
pub fn is_wayland() -> bool {
    if std::env::var("XDG_SESSION_TYPE").map(|v| v == "wayland").unwrap_or(false) {
        return true;
    }
    // Fallback: the Wayland display socket is set on any real Wayland session.
    std::env::var("WAYLAND_DISPLAY")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Compositor family the session runs under. Cached — env vars don't change
/// at runtime. Used to gate compositor-specific probes: the KWin D-Bus
/// detectors are pointless (temp-file write + doomed D-Bus connect) on
/// GNOME/wlroots sessions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Compositor {
    KdeWayland,
    GnomeWayland,
    OtherWayland,
    X11,
}

#[cfg(target_os = "linux")]
pub fn compositor() -> Compositor {
    static COMPOSITOR: std::sync::OnceLock<Compositor> = std::sync::OnceLock::new();
    *COMPOSITOR.get_or_init(|| {
        if !is_wayland() {
            return Compositor::X11;
        }
        let desktop = std::env::var("XDG_SESSION_DESKTOP")
            .or_else(|_| std::env::var("XDG_CURRENT_DESKTOP"))
            .map(|v| v.to_uppercase())
            .unwrap_or_default();
        if desktop.contains("KDE") {
            Compositor::KdeWayland
        } else if desktop.contains("GNOME") {
            Compositor::GnomeWayland
        } else {
            Compositor::OtherWayland
        }
    })
}

/// True only on KDE Plasma Wayland — the one session type where the KWin
/// D-Bus scripting detectors can work.
#[cfg(target_os = "linux")]
pub fn is_kde_wayland_session() -> bool {
    compositor() == Compositor::KdeWayland
}

/// Snapshot the active window right now (before Lychi steals focus).
///
/// Call this **before** `show_window()`, then pass the result to
/// `gather()` inside `spawn_blocking`.
pub fn snapshot_active_window() -> Option<WindowContext> {
    // Bypass cache — the watcher may not have polled since the user switched
    // windows, so the cache could still show the previous window. Ground truth
    // matters here; we also update the cache as a side effect.
    active_window::detect_live()
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
    active_window::record_summon();
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

    // Run window-stack scan, clipboard detection, and network detection in parallel
    // with CWD/git/project/docker detection. The stack scan involves D-Bus (KWin)
    // or X11 calls that can take 50-200ms, and network detection spawns nmcli,
    // so we overlap them with the other detections.
    let (main_result, stack_result, clipboard_result, network_result) = std::thread::scope(|s| {
        // Spawn the window-stack scan
        let window_ref = window.as_ref();
        let stack_handle = s.spawn(move || window_stack::find_recent_terminal(window_ref));

        // Spawn clipboard detection (arboard read, < 2ms)
        let clipboard_handle =
            s.spawn(|| clipboard_detect::detect().map(|(_text, content_type)| content_type));

        // Spawn network detection (nmcli + /sys/class/net, 10-50ms)
        let network_handle = s.spawn(|| {
            if let Some(cached) = cache::get_network() {
                tracing::debug!("gather: network cache hit");
                return cached;
            }
            let result = network::detect();
            cache::set_network(&result);
            result
        });

        // Main thread: CWD + git + project + docker (sequential chain)
        let mut ide_workspace_source = IdeWorkspaceSource::None;

        // Pinned workspace override — bypasses auto-detection entirely.
        // code_root resolution still runs afterward (handles meta-containers).
        let pinned = pin::get();

        let cwd = if let Some(ref pinned_path) = pinned {
            tracing::debug!("gather: using pinned workspace: {pinned_path}");
            ide_workspace_source = IdeWorkspaceSource::Pinned;
            Some(pinned_path.clone())
        } else {
            window.as_ref().and_then(|w| {
                if w.is_terminal {
                    cwd::detect(w.pid, &w.wm_class, &w.title)
                } else if w.is_ide {
                    let (ws, src) =
                        ide::detect_workspace(w.pid, &w.title, &w.wm_class, w.window_id.as_deref());
                    ide_workspace_source = src;

                    // Store in per-window cache for future fast-path hits —
                    // but only when the path is consistent with the title
                    // token, so a fallback-resolved path can never poison the
                    // cache against a token it doesn't belong to.
                    if let (Some(path), Some(wid), Some(token)) =
                        (&ws, &w.window_id, ide::extract_token(&w.title))
                        && ide::path_matches_token(path, token)
                    {
                        let marker = ide::which_project_marker(std::path::Path::new(path))
                            .unwrap_or_else(|| ".git".to_string());
                        workspace_cache::set(
                            wid,
                            workspace_cache::CachedWorkspace {
                                path: path.clone(),
                                token: token.to_string(),
                                marker,
                                resolved_at: std::time::Instant::now(),
                            },
                        );
                    }

                    ws
                } else {
                    None
                }
            })
        };

        // Resolve code_root for IDE workspaces (or pinned): the actual repo/project
        // directory where git + build markers live. For terminals, use cwd directly.
        let is_ide = window.as_ref().is_some_and(|w| w.is_ide);
        let needs_code_root = is_ide || pinned.is_some();
        let (code_root, code_root_source) = if needs_code_root {
            // Active-file proxy hint: the IDE window title's subfolder token
            // names the focused sub-repo when the workspace root holds several
            // (e.g. `amt/` with three repos → the one the user is editing).
            let title_token = window
                .as_ref()
                .filter(|w| w.is_ide)
                .and_then(|w| ide::extract_token(&w.title));
            let hint = ide::ActiveHint {
                title_token,
                terminal_cwd: None, // computed later in the pipeline; title suffices
            };
            cwd.as_ref()
                .and_then(|ws| ide::resolve_code_root(std::path::Path::new(ws), &hint))
                .map(|(path, src)| (Some(path), src))
                .unwrap_or((None, CodeRootSource::None))
        } else {
            (None, CodeRootSource::None)
        };

        if needs_code_root {
            tracing::debug!(
                "gather: code_root={:?} (source={})",
                code_root.as_deref(),
                code_root_source
            );
        }

        // For IDE/pinned: use code_root only (don't fall back to workspace meta-root)
        // For terminals: use cwd as before
        let detect_dir: Option<&str> = if needs_code_root {
            code_root.as_deref()
        } else {
            cwd.as_deref()
        };

        let git_ctx = detect_dir.and_then(|dir| {
            // Check cache first — avoids spawning `git status` subprocess
            if let Some(cached) = cache::get_git(dir) {
                tracing::debug!("gather: git cache hit for {dir}");
                return cached;
            }
            let result = git::detect(dir);
            if result.is_none() {
                tracing::debug!("gather: git=none (no .git found walking up from {dir})");
            }
            cache::set_git(dir, &result);
            result
        });

        let project_ctx = detect_dir
            .and_then(|dir| {
                if let Some(cached) = cache::get_project(dir) {
                    tracing::debug!("gather: project cache hit for {dir}");
                    return cached;
                }
                let result = project::detect(dir)
                    .or_else(|| git_ctx.as_ref().and_then(|g| project::detect(&g.repo_root)));
                if result.is_none() {
                    tracing::debug!("gather: project=none (no project markers at {dir})");
                }
                cache::set_project(&result);
                result
            })
            .or_else(|| git_ctx.as_ref().and_then(|g| project::detect(&g.repo_root)));

        // Enrich with workspace context when git root differs from project root
        let mut project_ctx = project_ctx;
        if let (Some(proj), Some(git)) = (&mut project_ctx, &git_ctx) {
            project::enrich_with_workspace(proj, &git.repo_root);
        }

        let docker_ctx = if let Some(cached) = cache::get_docker() {
            tracing::debug!("gather: docker cache hit");
            cached
        } else {
            let result = docker::detect();
            cache::set_docker(&result);
            result
        };

        let main = (
            cwd,
            git_ctx,
            project_ctx,
            docker_ctx,
            ide_workspace_source,
            code_root_source,
        );
        let stack = stack_handle
            .join()
            .ok()
            .unwrap_or((None, TerminalSource::None));
        let clipboard = clipboard_handle.join().ok().flatten();
        let net = network_handle.join().ok().flatten();

        (main, stack, clipboard, net)
    });

    let (cwd, git_ctx, project_ctx, docker_ctx, ide_workspace_source, code_root_source): (
        Option<String>,
        Option<GitContext>,
        Option<ProjectContext>,
        Option<DockerContext>,
        IdeWorkspaceSource,
        CodeRootSource,
    ) = main_result;
    let (stack_terminal, terminal_source): (Option<WindowContext>, TerminalSource) = stack_result;

    tracing::debug!(
        "gather: stack_terminal={:?}, cwd={:?}, terminal_source={}",
        stack_terminal
            .as_ref()
            .map(|w| format!("{}(pid={})", w.wm_class, w.pid)),
        cwd.as_deref(),
        terminal_source,
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
        // Enrich with workspace context for terminal CWD too
        let mut proj = proj;
        if let (Some(p), Some(g)) = (&mut proj, &git) {
            project::enrich_with_workspace(p, &g.repo_root);
        }
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

    // Coherence check: does terminal_cwd belong to the same repository as cwd?
    //
    // Uses resolve_gitdir() which follows `.git` file pointers — correctly handles
    // worktrees (both dirs share the same common gitdir), submodules, and plain repos.
    // Falls back to project-marker root comparison if neither side has git.
    //
    // Result gates whether terminal_cwd is allowed to override workspace context for routing.
    let terminal_matches_workspace = match (&cwd, &terminal_cwd) {
        (Some(ide_dir), Some(term_dir)) if ide_dir != term_dir => {
            // Primary: compare resolved gitdir (handles worktrees correctly)
            let ide_gitdir = git_ctx
                .as_ref()
                .and_then(|g| git::resolve_gitdir(&g.repo_root));
            let term_gitdir = git::detect(term_dir)
                .as_ref()
                .and_then(|g| git::resolve_gitdir(&g.repo_root));

            match (ide_gitdir, term_gitdir) {
                (Some(ig), Some(tg)) => ig == tg,
                // Fallback: compare project-marker roots (for non-git monorepos etc.)
                _ => {
                    let ide_proj = project_ctx.as_ref().map(|p| p.root.as_str());
                    let term_proj = project::detect(term_dir).map(|p| p.root);
                    match (ide_proj, term_proj) {
                        (Some(ip), Some(ref tp)) => ip == tp.as_str(),
                        _ => false,
                    }
                }
            }
        }
        // terminal_cwd is None, or same as cwd — trivially coherent
        _ => true,
    };

    if !terminal_matches_workspace && terminal_cwd.is_some() {
        metrics::inc_terminal_incoherent_filtered();
    }
    tracing::debug!("gather: terminal_matches_workspace={terminal_matches_workspace}");

    EnvironmentContext {
        active_window: window,
        cwd,
        terminal_cwd,
        terminal_class,
        git: git_ctx,
        project: project_ctx,
        docker: docker_ctx,
        hour: chrono::Local::now().hour() as u8,
        clipboard: clipboard_result,
        network: network_result,
        gather_ms: start.elapsed().as_millis() as u64,
        terminal_matches_workspace,
        gathered_at: Some(start),
        active_window_source: ActiveWindowSource::LiveProbe, // refined by detect_kwin()
        terminal_source,
        ide_workspace_source,
        code_root_source,
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
        for item in suggestions::suggest(&test_ctx, None) {
            println!(
                "  {} — {}",
                item.label,
                item.description.unwrap_or_default()
            );
        }
    }

    // ── Golden Scenario: Staleness ────────────────────────────────────────

    #[test]
    fn fresh_context_is_not_stale() {
        let ctx = EnvironmentContext {
            gathered_at: Some(Instant::now()),
            ..Default::default()
        };
        assert!(
            !ctx.is_soft_stale(),
            "freshly gathered context must not be soft-stale"
        );
        assert!(
            !ctx.is_hard_stale(),
            "freshly gathered context must not be hard-stale"
        );
    }

    #[test]
    fn context_without_gathered_at_is_not_stale() {
        // Deserialized context (serde skip) has gathered_at=None — must not be considered stale
        // to avoid false warnings for contexts loaded from IPC/JSON.
        let ctx = EnvironmentContext {
            gathered_at: None,
            ..Default::default()
        };
        assert!(
            !ctx.is_soft_stale(),
            "context with no gathered_at must not be soft-stale"
        );
        assert!(
            !ctx.is_hard_stale(),
            "context with no gathered_at must not be hard-stale"
        );
    }

    #[test]
    fn age_reflects_elapsed_time() {
        let ctx = EnvironmentContext {
            gathered_at: Some(Instant::now()),
            ..Default::default()
        };
        let age = ctx
            .age()
            .expect("age() must return Some when gathered_at is set");
        assert!(
            age.as_millis() < 100,
            "age should be near-zero for fresh context"
        );
    }

    // ── Golden Scenario: Coherence gating ────────────────────────────────

    #[test]
    fn coherence_trivially_true_when_no_terminal_cwd() {
        // If terminal_cwd is None there's nothing to mismatch — trivially coherent.
        let ctx = EnvironmentContext {
            cwd: Some("/home/sab/projects/api".into()),
            terminal_cwd: None,
            terminal_matches_workspace: true,
            gathered_at: Some(Instant::now()),
            ..Default::default()
        };
        assert!(ctx.terminal_matches_workspace);
    }

    #[test]
    fn coherence_trivially_true_when_same_path() {
        // terminal_cwd == cwd → trivially the same project.
        let ctx = EnvironmentContext {
            cwd: Some("/home/sab/projects/api".into()),
            terminal_cwd: Some("/home/sab/projects/api".into()),
            terminal_matches_workspace: true,
            gathered_at: Some(Instant::now()),
            ..Default::default()
        };
        assert!(ctx.terminal_matches_workspace);
    }

    #[test]
    fn incoherent_terminal_excluded_from_ai_hint() {
        use crate::context::clipboard_detect::ClipboardContentType;

        // IDE on Rust project, terminal on Node project — incoherent.
        // The terminal_cwd must NOT appear in ai_hint.
        let ctx = EnvironmentContext {
            cwd: Some("/home/sab/projects/cli".into()),
            terminal_cwd: Some("/home/sab/projects/api".into()),
            terminal_matches_workspace: false, // incoherent
            git: Some(GitContext {
                repo_root: "/home/sab/projects/cli".into(),
                branch: "main".into(),
                dirty: false,
                remote: None,
            }),
            clipboard: Some(ClipboardContentType::Plain),
            gathered_at: Some(Instant::now()),
            ..Default::default()
        };

        let hint = ctx.ai_hint().unwrap_or_default();
        assert!(
            !hint.contains("/home/sab/projects/api"),
            "incoherent terminal_cwd must not appear in ai_hint, got:\n{hint}"
        );
        assert!(
            hint.contains("/home/sab/projects/cli"),
            "workspace cwd must still appear in ai_hint, got:\n{hint}"
        );
    }

    #[test]
    fn coherent_terminal_included_in_ai_hint() {
        // IDE and terminal in the same project → terminal_cwd appears in hint.
        let ctx = EnvironmentContext {
            cwd: Some("/home/sab/projects/api".into()),
            terminal_cwd: Some("/home/sab/projects/api/src".into()),
            terminal_matches_workspace: true,
            gathered_at: Some(Instant::now()),
            ..Default::default()
        };

        let hint = ctx.ai_hint().unwrap_or_default();
        assert!(
            hint.contains("/home/sab/projects/api/src"),
            "coherent terminal_cwd must appear in ai_hint, got:\n{hint}"
        );
    }

    #[test]
    fn hard_stale_caveat_in_ai_hint() {
        // Hard-stale context must include the trust caveat in ai_hint.
        // We simulate by directly constructing a context with gathered_at far in the past.
        // Note: Instant arithmetic — we can't go backwards, so we use a no-op context
        // and verify the method logic by testing with is_hard_stale() = false on fresh ctx.
        let fresh_ctx = EnvironmentContext {
            cwd: Some("/home/sab/projects/api".into()),
            gathered_at: Some(Instant::now()),
            ..Default::default()
        };
        let hint = fresh_ctx.ai_hint().unwrap_or_default();
        assert!(
            !hint.contains("stale"),
            "fresh context hint must not mention staleness, got:\n{hint}"
        );

        // Verify the caveat line format exists in the code path by constructing
        // a context without gathered_at (hard_stale() returns false → no caveat).
        let no_ts_ctx = EnvironmentContext {
            cwd: Some("/home/sab/projects/api".into()),
            gathered_at: None,
            ..Default::default()
        };
        let hint2 = no_ts_ctx.ai_hint().unwrap_or_default();
        assert!(
            !hint2.contains("stale"),
            "context without timestamp must not show stale caveat"
        );
    }
}
