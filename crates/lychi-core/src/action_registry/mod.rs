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

/// A privacy-relevant network disclosure an invocation performs (C6), declared
/// by the handler on its [`RiskAssessment`] and enforced by the Rules Engine
/// against what the user has already granted in `PrivacyConfig`.
///
/// Declared HERE — by the handler, next to the dispatch that decides what the
/// invocation actually does — because the Rules Engine keeping its own list of
/// which args are sensitive is a second parser of the same question, and the
/// two drifted three separate ways: `sysinfo speed` ran the speedtest
/// unconsented (gate knew only "speedtest"), `sysinfo network` fetched the
/// public IP unconsented (gate knew only "net"), and `sysinfo ip` prompted
/// about a public-IP lookup it never performs (it prints local addresses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentKind {
    /// Sends the user's IP to a geolocation service to locate them.
    IpGeolocation,
    /// Looks up the user's public IP via an external service.
    PublicIp,
    /// Bulk data transfer to a third party (e.g. speedtest). Has no
    /// remember-me flag: consented per run, every run.
    LargeTransfer,
}

impl ConsentKind {
    /// The feature key the frontend passes to `grant_privacy_consent` when the
    /// user confirms with "Allow and remember". `None` = nothing to remember
    /// (LargeTransfer asks every run). These strings are the wire contract
    /// with `commands/config.rs`'s grant endpoint — the frontend used to
    /// recover this fact by substring-matching the confirmation PROSE
    /// ("freeipapi.com" / "ifconfig.me"), so rewording a sentence silently
    /// broke consent persistence.
    pub fn feature_key(self) -> Option<&'static str> {
        match self {
            ConsentKind::IpGeolocation => Some("ip_geolocation"),
            ConsentKind::PublicIp => Some("public_ip"),
            ConsentKind::LargeTransfer => None,
        }
    }
}

/// A consent requirement: what kind, and the exact prompt to show. The prompt
/// lives with the declaration for the same reason `reason` lives on
/// [`RiskAssessment`] — the handler owns its user-facing wording.
#[derive(Debug, Clone)]
pub struct ConsentNeed {
    pub kind: ConsentKind,
    pub prompt: String,
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
    /// A privacy consent this invocation needs before it may run. Independent
    /// of `level`: an action can be operationally Low risk and still disclose
    /// data (sysinfo net is harmless to the machine, not to privacy).
    pub consent: Option<ConsentNeed>,
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
            consent: None,
        }
    }

    /// A `Medium`-risk verdict (needs confirmation) with a custom message.
    pub fn confirm(reason: impl Into<String>) -> Self {
        Self {
            level: RiskLevel::Medium,
            reason: Some(reason.into()),
            consent: None,
        }
    }

    /// Attach a privacy-consent requirement (C6) to this verdict.
    pub fn with_consent(mut self, kind: ConsentKind, prompt: impl Into<String>) -> Self {
        self.consent = Some(ConsentNeed {
            kind,
            prompt: prompt.into(),
        });
        self
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
    /// CommonMark, rendered as formatted rich text.
    ///
    /// Distinct from `Text`, which is prose with no structure: a model that
    /// emits a numbered list, a table, `**bold**`, or a fenced code block was
    /// already producing markup, and `Text` threw the structure away and
    /// displayed the source characters. That is most visible on the AI surface,
    /// where the agent's answers are markdown by nature.
    ///
    /// Rendered with a strict sanitiser rather than raw HTML — the content is
    /// model output or third-party API text, so it is untrusted by default.
    Markdown,
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

/// Semantic state for a badge. Fixed set on purpose: a failed systemd unit, a
/// failed conversion and an expired timer must look the same, which they cannot
/// if each handler picks its own colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum BadgeTone {
    Ok,
    Warn,
    Error,
    Muted,
}

/// A short state chip on a row.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct Badge {
    pub text: String,
    pub tone: BadgeTone,
}

/// Right-aligned metadata on a row.
///
/// A typed vocabulary rather than a pre-formatted `String`, because formatting
/// is the frontend's job: a handler computing "3 days ago" is the same mistake
/// as a handler padding columns to align them. `Relative` carries a unix
/// timestamp and the renderer phrases it, so ages read identically across
/// timers, notes, clipboard and history.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Accessory {
    /// Literal text (a version, a size already in human units).
    Text { value: String },
    /// Unix seconds, rendered relative to now ("now", "3d").
    Relative { at: i64 },
}

/// One action a row supports, declared by the handler that produced the row.
///
/// **Declarative, never executable.** The handler names an `id` and the `target`
/// it applies to; it does NOT ship a command string for the frontend to run.
/// That distinction is the safety property: if actions carried commands, then
/// anything able to populate a row could propose arbitrary execution, and the
/// rules engine would see something indistinguishable from user input. Instead
/// the producing handler maps `(id, target)` back to a command — and because it
/// enumerated the rows in the first place, it can reject a `target` it never
/// emitted, which is what stops injection through the argument.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct RowAction {
    /// Verb, resolved by the producing handler (e.g. `"restart"`).
    pub id: String,
    /// Human label for the ⌘K menu.
    pub label: String,
    /// Which row this acts on (e.g. the unit name). Echoed back on invocation
    /// and validated by the handler against what it produced.
    pub target: String,
    /// Per-action risk, so `stop` can confirm while `logs` does not. Finer than
    /// the handler-wide `default_risk`; the rules engine remains the gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<RiskLevel>,
}

/// A single row in a `Rows` result.
///
/// Deliberately the same vocabulary as `CompletionItem` (title/subtitle/icon),
/// because a list of things is a list of things whether it came from a
/// suggestion pass or an execution. Two divergent row shapes would be two
/// renderers that drift.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct Row {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badge: Option<Badge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accessories: Vec<Accessory>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<RowAction>,
}

impl Row {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: None,
            badge: None,
            accessories: Vec::new(),
            actions: Vec::new(),
        }
    }
    pub fn subtitle(mut self, s: impl Into<String>) -> Self {
        let s = s.into();
        if !s.is_empty() {
            self.subtitle = Some(s);
        }
        self
    }
    pub fn badge(mut self, text: impl Into<String>, tone: BadgeTone) -> Self {
        self.badge = Some(Badge {
            text: text.into(),
            tone,
        });
        self
    }
    /// Right-aligned literal text (a version, an app-id, a size).
    pub fn accessory_text(mut self, value: impl Into<String>) -> Self {
        let value = value.into();
        if !value.is_empty() {
            self.accessories.push(Accessory::Text { value });
        }
        self
    }
    /// Right-aligned age, rendered relative to now by the frontend.
    pub fn accessory_at(mut self, at: i64) -> Self {
        self.accessories.push(Accessory::Relative { at });
        self
    }
    pub fn action(mut self, id: &str, label: &str, target: &str, risk: Option<RiskLevel>) -> Self {
        self.actions.push(RowAction {
            id: id.into(),
            label: label.into(),
            target: target.into(),
            risk,
        });
        self
    }
}

/// A titled group of rows. Grouping is explicit rather than implied by sort
/// order, so "failed" units can lead without a handler encoding that in a
/// string.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct Section {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub rows: Vec<Row>,
    /// Which handler produced these rows.
    ///
    /// Row actions are resolved by their producer, so the frontend has to say
    /// who that was when invoking one. It cannot be inferred from the envelope:
    /// `routed_by` records HOW the command was routed ("explicit"/"pattern"/
    /// "ai"), not WHO handled it. Carrying it on the section keeps the action
    /// resolvable without the frontend guessing.
    pub handler: String,
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
    /// A list of things, optionally grouped, each optionally actionable.
    ///
    /// Crosses IPC **typed**, unlike the weather card, which smuggles JSON
    /// through the string `output` field and is parsed back out with
    /// `JSON.parse` frontend-side. That works exactly once; a second handler
    /// wanting structure has to copy the hack. Here specta generates the
    /// TypeScript from this type, so producer and renderer cannot drift.
    Rows { sections: Vec<Section> },
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
    /// When the pending confirmation is a PRIVACY CONSENT prompt: the feature
    /// key to persist on "Allow and remember" (`grant_privacy_consent`).
    /// Typed here because the frontend used to substring-match the prompt
    /// prose to recover it — rewording a sentence broke consent persistence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consent_feature: Option<String>,
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
    /// Structured rows, when the handler produced them. Typed all the way to
    /// TypeScript rather than serialised into `output` — see `Output::Rows`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<Section>>,
}

/// The executor-owned envelope: fields the executor/rules populate around a
/// handler's `ActionResult`, which handlers never set. Combined with the result
/// to build the wire DTO.
#[derive(Debug, Clone, Default)]
pub struct ResultEnvelope {
    pub routed_by: Option<String>,
    pub needs_confirmation: Option<String>,
    /// See [`CommandResultDto::consent_feature`].
    pub consent_feature: Option<String>,
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
            consent_feature: envelope.consent_feature,
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
            Output::Rows { sections } => dto.sections = Some(sections),
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

/// The family a command belongs to, for grouping in the Guide. Each handler
/// declares its own via `ActionHandler::category` (default `General`), so the
/// grouping is generated from the live registry and never hardcoded frontend-side.
/// Serialized as a lowercase string (`"files"`, `"ai"`, …) for the TS bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum CommandCategory {
    /// Files, folders, search, browse, reveal.
    Files,
    /// AI chat, presets, text transforms.
    Ai,
    /// Web search, YouTube, definitions, bangs, bookmarks.
    Web,
    /// Processes, windows, services, power, system info.
    System,
    /// Shell, ssh, terminal, dev encoders/utilities.
    Developer,
    /// Media / MPRIS control.
    Media,
    /// Calculators, converters, generators, notes, timers, and other tools.
    Utilities,
    /// Uncategorised (the default).
    General,
}

impl CommandCategory {
    /// Human-readable section title shown in the Guide.
    pub fn title(self) -> &'static str {
        match self {
            CommandCategory::Files => "Files & Folders",
            CommandCategory::Ai => "AI",
            CommandCategory::Web => "Web & Search",
            CommandCategory::System => "System & Windows",
            CommandCategory::Developer => "Developer",
            CommandCategory::Media => "Media",
            CommandCategory::Utilities => "Utilities",
            CommandCategory::General => "General",
        }
    }

    /// Display order in the Guide (lower = earlier).
    pub fn order(self) -> u8 {
        match self {
            CommandCategory::Files => 0,
            CommandCategory::Ai => 1,
            CommandCategory::Web => 2,
            CommandCategory::Developer => 3,
            CommandCategory::System => 4,
            CommandCategory::Media => 5,
            CommandCategory::Utilities => 6,
            CommandCategory::General => 7,
        }
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
    /// The family this command belongs to (for Guide grouping).
    pub category: CommandCategory,
    /// The category's human-readable section title (so the frontend needn't map).
    pub category_title: String,
    /// The category's display order (lower sorts earlier).
    pub category_order: u8,
    /// Whether this command mutates external state (see
    /// [`ActionHandler::mutates_state`]). Surfaced so the AI coordinator can
    /// refuse to run two mutating tools in one turn.
    pub mutates: bool,
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
    /// May Enter select this row without the user arrowing to it?
    ///
    /// The VERDICT of `suggestions::Suggestion::can_be_default`, carried across
    /// IPC rather than recomputed. That rule is three conditions — the row's
    /// `Source` (a fallback or a guard may never be the default), its `Tier`
    /// (only an identity or prefix match may), and its `CompletionKind` — and
    /// the ranker is the only place that knows the first two.
    ///
    /// It used to be dropped at the boundary (`.map(|s| s.item)`), so the
    /// frontend re-derived it from `label`/`run` with a `startsWith` check. That
    /// reimplemented one of the three conditions and lost `Source` entirely,
    /// which is how a guard row could become Enter's target. Same shape as the
    /// bug the suggestions module documents having fixed: "the rule existed
    /// twice… they disagreed, which is precisely how `dnf search firefox`
    /// launched Firefox."
    ///
    /// Defaults to `false` for hand-built items that never went through the
    /// ranker: refusing to auto-select is the safe direction — the user's own
    /// text runs instead.
    #[serde(default)]
    pub can_be_default: bool,
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
    /// What KIND of row this is, when that changes how selecting it behaves.
    ///
    /// The frontend switches on this instead of matching display text. Label
    /// strings are for humans: they get reworded, translated, and truncated, and
    /// routing that depends on them breaks silently when they do. A row that
    /// needs special handling declares it here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<CompletionKind>,
    /// This row is user-pinned to the zero state. The ⌘K panel switches its
    /// action label on this (Pin ↔ Unpin); like `can_be_default` it is a
    /// backend verdict carried across IPC, never re-derived from display text.
    #[serde(default)]
    pub pinned: bool,
}

/// Rows whose selection behaviour differs from "run the command".
///
/// Deliberately small: most completions need no kind at all (`None` = an
/// ordinary row that runs via `run`/`fill`/`label`). Add a variant only when the
/// frontend must genuinely branch, never as a decorative tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "kebab-case")]
pub enum CompletionKind {
    /// A "Did you mean: X?" offer. Selecting it fills the corrected command
    /// (carried in `description`) rather than running the row's text, and — the
    /// part that needs a flag — it must WIN over the natural-language guard: the
    /// user explicitly picked a command, so Enter must not fall through to AI.
    Correction,
    /// A computed result (`= 42`). It's an ANSWER, not a command: selecting it
    /// displays the value rather than executing anything, and it must never be
    /// written back into the input as if it were runnable text.
    Calc,
    /// "Ask AI: …" — send the query to the agent.
    ///
    /// This is a KIND rather than a `run` string because there is no `ask`
    /// handler in the registry: a `run: "ask …"` row would be re-parsed by the
    /// executor's pattern router, find no such trigger, and fall through to a
    /// web search. Encoding the intent in the type means nothing has to recover
    /// it from text. The query itself travels in `description`.
    AskAi,
    /// "Search web: …" — the other universal escape hatch, alongside `AskAi`.
    /// Same reasoning: the query lives in `description`, not in a command
    /// string something downstream has to parse.
    SearchWeb,
}

impl CompletionKind {
    /// Whether this row is a FALLBACK — an escape hatch offered when nothing
    /// else fits, rather than a result in its own right.
    ///
    /// Fallbacks pin to the bottom of the list and are never auto-selected
    /// (the Alfred model). That distinction is load-bearing: these rows were
    /// once removed entirely because they WERE auto-selectable, so Enter on a
    /// question ran whichever fallback frecency floated up — competing with the
    /// single input classifier. Present but never preselected keeps the escape
    /// hatch without re-creating that bug.
    pub fn is_fallback(self) -> bool {
        matches!(self, Self::AskAi | Self::SearchWeb)
    }
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
/// A live sink for progressive command output. A handler that produces output
/// over time (a shell command, a build, a deploy) pushes each chunk here as it
/// arrives; whoever built the context (the AI coordinator) forwards those chunks
/// to the UI so the user watches the work happen instead of waiting for one blob
/// at the end.
///
/// A thin wrapper over an unbounded mpsc sender: `Clone` (so a handler can move a
/// copy into a spawned reader task), cheap, and non-blocking (`push` never awaits
/// — a full or closed channel silently drops, because streaming is best-effort
/// UI sugar and must never stall or fail the actual command). The FINAL,
/// complete output is still returned normally by the handler; the sink is purely
/// additive, for the human watching.
#[derive(Clone)]
pub struct OutputSink(tokio::sync::mpsc::UnboundedSender<String>);

impl OutputSink {
    /// Wrap a sender. The paired receiver is drained by the context's builder
    /// (e.g. the agent adapter) and turned into UI events.
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<String>) -> Self {
        Self(tx)
    }

    /// Push one chunk of output. Non-blocking and infallible from the caller's
    /// view: if the receiver is gone (the user navigated away, the run was
    /// cancelled) the chunk is dropped rather than propagating an error into the
    /// command's own execution.
    pub fn push(&self, chunk: impl Into<String>) {
        let _ = self.0.send(chunk.into());
    }
}

impl std::fmt::Debug for OutputSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The channel handle has no useful Debug; hide it so ExecContext stays
        // `Debug` (used in traces) without leaking an opaque sender.
        f.write_str("OutputSink(..)")
    }
}

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
    /// Optional live-output sink (see [`OutputSink`]). `None` — the default and
    /// every non-streaming path — means "no live streaming"; the handler runs
    /// exactly as before and returns its full output at the end. `Some` is wired
    /// by the AI coordinator so a captured `run` streams its output into the chat
    /// as it happens. Any handler can honour it; those that don't simply ignore
    /// it, so it is backward-compatible by construction.
    pub sink: Option<OutputSink>,
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

    /// The family this handler belongs to, for grouping in the Guide. Override to
    /// place a handler in a category; the default `General` keeps unclassified
    /// handlers visible. Purely presentational — never affects routing or risk.
    fn category(&self) -> CommandCategory {
        CommandCategory::General
    }

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

    /// Whether an invocation of this handler MUTATES external state — writes or
    /// deletes files, changes system/service state, installs packages, etc. —
    /// as opposed to being read-only or idempotent (search, calc, open a URL,
    /// define a word).
    ///
    /// The AI coordinator uses this to refuse to run two mutating calls in the
    /// SAME turn: a tool-calling model sometimes hedges, emitting several
    /// variants of one destructive operation at once (three ways to resize the
    /// same photos). Running all of them wastes tokens (→ rate limits) and can
    /// corrupt the result — the "don't parallelize non-idempotent tools"
    /// principle. Read-only handlers stay `false` and remain freely parallel
    /// (two file searches, two definitions in one turn are fine).
    ///
    /// Declared HERE, by the handler, for the same reason as `default_risk` and
    /// the consent kinds: the code that decides what an invocation *does* is the
    /// one place that knows whether it mutates. A central list in the coordinator
    /// would be a second, drift-prone parser of that question. Default `false`
    /// (read-only); the mutating handlers override to `true`.
    fn mutates_state(&self) -> bool {
        false
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
