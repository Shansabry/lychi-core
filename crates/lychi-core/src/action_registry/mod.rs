pub mod handlers;
pub mod registry;
pub mod trigger;

pub use trigger::{ArgTransform, Trigger};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::LychiError;

/// Risk level for any action. Used by the Rules Engine to decide
/// whether to auto-execute, require confirmation, or deny.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, specta::Type,
)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// How an action coordinates with concurrent executions (G4). Declared per
/// handler via `ActionHandler::execution_mode`; enforced by the app's execution
/// gate before `Executor::run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Runs immediately, any number in parallel. Fast, side-effect-light actions
    /// (calc, open, url, media control). The default.
    Immediate,
    /// A new invocation supersedes a still-running previous one of the same
    /// handler — the old work should be abandoned. For long, latest-wins actions
    /// (an AI query the user retyped).
    ReplacePrevious,
    /// Runs to completion with nothing else running concurrently. For destructive
    /// or global-state actions (index rebuild, bulk delete) where interleaving
    /// would corrupt state or confuse the user.
    Exclusive,
}

/// A handler's risk verdict for a specific invocation: the level, plus an
/// optional custom confirmation message. Returned by `ActionHandler::assess_risk`
/// so risk logic (and its user-facing wording) lives in the handler, not the
/// Rules Engine.
#[derive(Debug, Clone)]
pub struct RiskAssessment {
    pub level: RiskLevel,
    /// Custom confirmation message shown when this action needs confirming. When
    /// `None`, the Rules Engine uses a generic message for the level.
    pub reason: Option<String>,
}

/// Cheap, borrowed context passed to `assess_risk_ctx` (G2) so a handler can make
/// risk depend on *where* the action runs. Built by the executor from the live
/// `EnvironmentContext` — no I/O, just borrows of already-gathered fields.
#[derive(Debug, Clone, Copy, Default)]
pub struct RiskContext<'a> {
    /// Effective working directory the action will run in (detected workspace or
    /// terminal cwd), if known.
    pub cwd: Option<&'a str>,
    /// Root of the active code workspace, if a project is focused.
    pub workspace_root: Option<&'a str>,
}

impl RiskAssessment {
    /// A verdict with just a level and no custom message.
    pub fn level(level: RiskLevel) -> Self {
        Self {
            level,
            reason: None,
        }
    }

    /// A `Medium`-risk verdict (needs confirmation) with a custom message.
    pub fn confirm(reason: impl Into<String>) -> Self {
        Self {
            level: RiskLevel::Medium,
            reason: Some(reason.into()),
        }
    }
}

/// How the frontend should render the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum OutputType {
    /// Shell/terminal output — render with ANSI-to-HTML, monospace `<pre>`.
    Terminal,
    /// Natural language text (AI answers, notes) — clean readable sans-serif.
    Text,
    /// Short status message (e.g. "Launched Firefox") — compact, muted.
    Status,
    /// Structured weather card — JSON data rendered as a rich card.
    Weather,
    /// Inline SVG markup (e.g. a QR code) — embedded directly, crisp and
    /// scalable. Unicode-block "ASCII" QR isn't scannable in a GUI (anti-aliased
    /// text blurs module edges); SVG modules stay perfectly square (research
    /// 2026-07: vector is the correct format for on-screen QR).
    Svg,
}

/// The payload a handler produces — a sum type of the mutually-exclusive result
/// shapes. Replaces the old grab-bag of `Option` fields where a handler set one
/// or two and left a dozen `None`s. Internal to the core; the Tauri layer
/// flattens it into the wire DTO the frontend consumes (`CommandResultDto`).
#[derive(Debug, Clone, Default)]
pub enum Output {
    /// No payload — a bare success/failure carrying only `error`/`success`.
    #[default]
    None,
    /// Rendered text/terminal/status/weather/svg output.
    Text { body: String, kind: OutputType },
    /// The frontend should open this URL. `auto_open` = opening it IS the result
    /// (navigate + dismiss, no card); false = show a card with a link.
    Navigate { url: String, auto_open: bool },
    /// The Tauri side should launch this `.desktop` file via GIO DesktopAppInfo.
    LaunchDesktop { path: String },
    /// The app is already running — focus the window with this wm_class instead
    /// of launching a new instance (smart-open).
    FocusApp { wm_class: String },
}

/// The result of running a handler. Handler-facing and internal: it is NOT
/// serialized directly — the Tauri layer converts it (plus the executor's
/// envelope) into the flat `CommandResultDto` that crosses IPC. This keeps the
/// handler API clean (a sum type, no `None`-columns) without disturbing the wire
/// format the frontend depends on.
#[derive(Debug, Clone, Default)]
pub struct ActionResult {
    pub success: bool,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub output: Output,
    /// A fallback/related URL shown ALONGSIDE text output (e.g. an AI answer with
    /// a "browse the web" link). Distinct from `Output::Navigate`, which IS the
    /// result. Only a couple of handlers (ask, weather-ask) use this.
    pub link: Option<String>,
    /// Result-level risk tag for display (e.g. app-control kill). Distinct from
    /// the pre-execution `assess_risk` gate; the frontend shows it on the card.
    pub risk_level: Option<RiskLevel>,
}

impl ActionResult {
    /// Successful result with rendered text output.
    pub fn ok(output: impl Into<String>, output_type: OutputType) -> Self {
        Self {
            success: true,
            output: Output::Text {
                body: output.into(),
                kind: output_type,
            },
            ..Default::default()
        }
    }

    /// Failed result with an error message.
    pub fn err(error: impl Into<String>) -> Self {
        Self {
            success: false,
            error: Some(error.into()),
            ..Default::default()
        }
    }

    /// A bare success with no payload.
    pub fn empty_ok() -> Self {
        Self {
            success: true,
            ..Default::default()
        }
    }

    /// A successful "open this URL" result. `auto_open` = navigate + dismiss.
    pub fn navigate(url: impl Into<String>, auto_open: bool) -> Self {
        Self {
            success: true,
            output: Output::Navigate {
                url: url.into(),
                auto_open,
            },
            ..Default::default()
        }
    }

    /// A successful "launch this .desktop file" result (app launcher).
    pub fn launch_desktop(path: impl Into<String>) -> Self {
        Self {
            success: true,
            output: Output::LaunchDesktop { path: path.into() },
            ..Default::default()
        }
    }

    /// A successful "focus the already-running app" result (smart-open).
    pub fn focus_app(wm_class: impl Into<String>) -> Self {
        Self {
            success: true,
            output: Output::FocusApp {
                wm_class: wm_class.into(),
            },
            ..Default::default()
        }
    }

    /// Set the result-level risk tag (builder-style).
    pub fn with_risk(mut self, risk: RiskLevel) -> Self {
        self.risk_level = Some(risk);
        self
    }

    /// Attach a fallback/related URL shown alongside the output (builder-style).
    pub fn with_link(mut self, url: impl Into<String>) -> Self {
        self.link = Some(url.into());
        self
    }

    /// Set the elapsed duration in milliseconds (builder-style).
    pub fn with_duration(mut self, ms: u64) -> Self {
        self.duration_ms = ms;
        self
    }
}

/// Flat wire DTO sent to the frontend. Deliberately mirrors the historical
/// field layout so the generated TS bindings and the ~19 frontend consumer sites
/// stay unchanged — the sum-type cleanup is internal to the core. The Tauri layer
/// builds this from an `ActionResult` plus the executor's envelope
/// (risk/confirmation/executed_args/routed_by).
#[derive(Debug, Clone, Default, Serialize, Deserialize, specta::Type)]
pub struct CommandResultDto {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_confirmation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_level: Option<RiskLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_type: Option<OutputType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executed_args: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_desktop: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_app: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_open: bool,
}

/// The executor-owned envelope: fields the executor/rules populate around a
/// handler's `ActionResult`, which handlers never set. Combined with the result
/// to build the wire DTO.
#[derive(Debug, Clone, Default)]
pub struct ResultEnvelope {
    pub routed_by: Option<String>,
    pub needs_confirmation: Option<String>,
    pub risk_level: Option<RiskLevel>,
    pub executed_args: Option<String>,
}

impl CommandResultDto {
    /// Flatten a handler result + executor envelope into the wire shape.
    pub fn build(result: ActionResult, envelope: ResultEnvelope) -> Self {
        let mut dto = CommandResultDto {
            success: result.success,
            error: result.error,
            duration_ms: result.duration_ms,
            routed_by: envelope.routed_by,
            needs_confirmation: envelope.needs_confirmation,
            // Result-level risk (handler-set, e.g. app-control) takes precedence;
            // otherwise the executor's assessed risk (for confirm prompts).
            risk_level: result.risk_level.or(envelope.risk_level),
            executed_args: envelope.executed_args,
            ..Default::default()
        };
        match result.output {
            Output::None => {}
            Output::Text { body, kind } => {
                dto.output = Some(body);
                dto.output_type = Some(kind);
            }
            Output::Navigate { url, auto_open } => {
                dto.open_url = Some(url);
                dto.auto_open = auto_open;
            }
            Output::LaunchDesktop { path } => dto.launch_desktop = Some(path),
            Output::FocusApp { wm_class } => dto.focus_app = Some(wm_class),
        }
        // A fallback link shown alongside text output (ask / weather-ask). Never
        // auto-opens — it's a "browse" affordance next to the answer.
        if let Some(link) = result.link {
            dto.open_url = Some(link);
        }
        dto
    }
}

/// One row in the dynamic command catalog (Guide/help). Generated from the live
/// registry so it never goes stale. See `ActionRegistry::command_catalog`.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct CommandInfo {
    /// Handler id (e.g. "open", "translate").
    pub id: String,
    /// Primary keyword the user types (e.g. "open", "qr", "translate").
    pub keyword: String,
    /// One-line human description from the handler.
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, specta::Type)]
pub struct CompletionItem {
    pub label: String,
    pub icon_path: Option<String>,
    pub score: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Provenance — why this was suggested. Set by context suggestions,
    /// `None` for non-context completions (app search, emoji, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Base64-encoded PNG thumbnail for image clipboard entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb_b64: Option<String>,
    /// The exact command to execute when this completion is chosen, when it
    /// differs from `label`. `label` is the human-facing display text (e.g.
    /// "Search YouTube: cats"); `run` is what the executor receives (e.g.
    /// "yt cats"). `None` means "label is the command" or "let the frontend
    /// decide" (app launch → `open {label}`). The frontend always prefers
    /// `run` when present — no label reverse-parsing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Text to insert into the input when this completion is chosen, instead of
    /// executing. Used for argument-needing hints (e.g. "volume <n>" fills
    /// "system volume " so the user types the value, then Enter runs it) — the
    /// tab-to-complete pattern in Raycast/Alfred. Takes precedence over `run`.
    /// `None` means the completion executes (via `run` or the fallback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
}

impl CompletionItem {
    /// A completion whose `label` is also displayed; icon/score set, the rest
    /// defaulted. Use the `with_*` builders to add optional fields.
    pub fn new(label: impl Into<String>, icon_path: Option<String>, score: u16) -> Self {
        Self {
            label: label.into(),
            icon_path,
            score,
            ..Default::default()
        }
    }

    /// Set the exact command to run (when it differs from `label`).
    pub fn with_run(mut self, run: impl Into<String>) -> Self {
        self.run = Some(run.into());
        self
    }

    /// Set text to insert into the input instead of executing (tab-to-complete
    /// for argument-needing hints, e.g. "system volume ").
    pub fn with_fill(mut self, fill: impl Into<String>) -> Self {
        self.fill = Some(fill.into());
        self
    }

    /// Set the secondary description line.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the provenance reason chip (context suggestions).
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// Set the base64 PNG thumbnail (clipboard images).
    pub fn with_thumb(mut self, thumb_b64: impl Into<String>) -> Self {
        self.thumb_b64 = Some(thumb_b64.into());
        self
    }
}

/// A terminal Lychi can route a `run` command into (the focus-ring target).
#[derive(Clone, Debug)]
pub struct TerminalTarget {
    pub wm_class: String,
    pub pid: u32,
    pub window_id: Option<String>,
}

/// Where a `run` command's output goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputMode {
    /// Open the command in a terminal emulator (the default).
    #[default]
    Terminal,
    /// Capture output inline in Lychi's result panel (Shift+Enter).
    Inline,
}

/// The immutable, per-invocation execution context passed to every handler's
/// `execute()`. The industry-standard command-execution model: the orchestrator
/// builds one context per run — the runtime environment (cwd, terminal, routing)
/// AND how output should be delivered (`output_mode`) — and threads it to the
/// handler by reference. Handlers that need it read it; the rest ignore it.
///
/// Replaces the previous cluster of module-level `static`s set through
/// fire-and-forget free functions (the hidden global side-channel the
/// architecture review flagged). Being per-call and immutable, tests construct a
/// fresh context instead of serializing on a global lock, and no cross-invocation
/// state can leak.
#[derive(Clone, Debug, Default)]
pub struct ExecContext {
    /// Working directory for a shell command (from context/multi-repo resolution).
    pub cwd: Option<String>,
    /// Configured terminal emulator (binary name).
    pub terminal: Option<String>,
    /// Terminal routing mode — "auto" | "manual" | "off".
    pub terminal_routing: String,
    /// Resolved routing target terminal (from the focus ring).
    pub terminal_target: Option<TerminalTarget>,
    /// How the command's output should be delivered.
    pub output_mode: OutputMode,
}

impl ExecContext {
    /// The routing mode, defaulting to "off" when unset.
    pub fn routing_mode(&self) -> &str {
        if self.terminal_routing.is_empty() {
            "off"
        } else {
            &self.terminal_routing
        }
    }
}

/// Trait for action handlers. Each handler has a unique ID (e.g. "open", "web", "run")
/// and knows how to execute its action and provide completions.
#[async_trait]
pub trait ActionHandler: Send + Sync {
    /// Unique identifier for this handler (e.g., "open", "web", "run").
    fn id(&self) -> &str;

    /// Human-readable description for help/discovery.
    fn description(&self) -> &str;

    /// Default risk level for this handler. Override for risky handlers.
    ///
    /// This is the *static* risk (same for every invocation). For per-invocation
    /// risk (e.g. `service nginx status` is safe but `service nginx stop` is not),
    /// override `assess_risk` instead — it sees the args and can also supply a
    /// custom confirmation message.
    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }

    /// Assess the risk of a *specific* invocation, given its args and a cheap
    /// `RiskContext` (cwd, active workspace). This is where a handler owns the
    /// "which of my invocations is dangerous?" decision — keeping that knowledge
    /// in the handler instead of the Rules Engine — and it can make risk depend on
    /// *where* the action runs (G2): deleting inside `/tmp` vs `~/Documents`,
    /// running a checked-in script vs a downloaded one.
    ///
    /// The default defers to `default_risk()` with no custom message, so handlers
    /// with uniform risk don't need to implement this. Handlers with mixed risk
    /// override it to inspect `args` (and, when relevant, `ctx`).
    fn assess_risk(&self, _args: &str, _ctx: &RiskContext<'_>) -> RiskAssessment {
        RiskAssessment::level(self.default_risk())
    }

    /// How this handler's executions coordinate with concurrent ones (G4). The
    /// Executor uses this to serialize/cancel appropriately. Default `Immediate`
    /// (unbounded parallelism) suits fast, side-effect-light actions (calc, open,
    /// completions). Override for long/cancellable or destructive/exclusive work.
    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Immediate
    }

    /// Keyword triggers that route to this handler, with their arg transforms.
    /// The registry builds the routing index from these at startup, so a handler
    /// declares its own prefixes here instead of editing the central
    /// `intent/patterns.rs` table. Default: none (structural-only handlers, or
    /// handlers reached solely via AI/fallback).
    fn triggers(&self) -> &'static [Trigger] {
        &[]
    }

    /// Execute the action with the given arguments, within the per-invocation
    /// `ExecContext` (runtime env + output routing). Most handlers ignore `ctx`;
    /// the shell/ssh handlers read it for cwd/terminal/output-mode.
    async fn execute(&self, ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError>;

    /// Provide completions for partial input. Default: empty.
    async fn completions(&self, _partial: &str) -> Vec<CompletionItem> {
        Vec::new()
    }
}
