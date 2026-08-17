//! The tool-execution seam. The coordinator depends only on this trait, so the
//! real `Executor` (with the Rules Engine, side effects, Tauri context) and a
//! test mock both satisfy it. This is the highest-value abstraction in the
//! brick — it makes the entire agent loop unit-testable.

use async_trait::async_trait;

use crate::error::LychiError;

/// A rich, non-text result from a tool (a QR SVG, a weather card, an image) that
/// the UI renders inline in the chat. The MODEL only ever sees the text `output`
/// (a short summary), never this — so raw SVG/JSON doesn't pollute its context.
#[derive(Clone, Debug)]
pub struct ToolArtifact {
    /// What kind of content this is, for the UI to render: "svg" | "weather" | …
    pub kind: String,
    /// The raw content (SVG markup, weather JSON, a data: URI, …).
    pub content: String,
}

/// What running one tool produced. Either it completed (with output), or the
/// Rules Engine flagged it destructive and it PAUSED awaiting user approval.
pub enum ToolOutcome {
    /// The tool ran. `output` is the text to feed back to the model (stdout, an
    /// answer, or an error message). `is_error` marks a failure so the model can
    /// react — a tool error is NOT fatal; it's fed back like any result.
    /// `artifact` carries a rich visual result (QR/weather/image) for the UI to
    /// render inline — the model never sees it (it gets only `output`).
    Ran {
        output: String,
        is_error: bool,
        artifact: Option<ToolArtifact>,
        /// A captured image the MODEL should see (a screenshot the agent just
        /// took, already vision-encoded). The loop appends it to the session as
        /// an image message after the tool result — tool-result content is
        /// text-only on the OpenAI dialect, so it can't ride the result itself.
        image: Option<crate::providers::ImageSource>,
    },
    /// The tool is destructive and needs the user's OK before running. `reason`
    /// explains why (shown in the approval prompt). `resume` is an opaque token
    /// the executor uses to run this exact assessed action on approval (so the
    /// approved run can't be re-resolved into something else — closes the TOCTOU
    /// gap). The coordinator treats it as a black box.
    NeedsApproval { reason: String, resume: ResumeToken },
}

/// An opaque, resumable handle to a paused (assessed-but-not-run) tool call. The
/// coordinator serializes it into the `Session` and hands it back to
/// `run_approved` on user approval. Its contents are the executor's concern.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ResumeToken(pub serde_json::Value);

/// A live-output channel the coordinator hands to `execute` so a streaming tool
/// (a shell command) can push its output line-by-line while it runs. A plain
/// tokio sender of `String` chunks — deliberately NOT `action_registry`'s
/// `OutputSink`, so the coordinator brick keeps its clean dependency surface
/// (`error` + `providers` only). The adapter that bridges to the real executor
/// wraps this into whatever the handler layer expects. `None` = the tool runs
/// without streaming (tests, non-streaming tools), exactly as before.
pub type ToolOutputChannel = tokio::sync::mpsc::UnboundedSender<String>;

/// Runs individual tools for the coordinator. Implemented by the real `Executor`
/// adapter and by test mocks.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Execute the tool `name` with a single `args` string (Lychi's uniform tool
    /// shape). Returns `Ran` on completion (or tool error) or `NeedsApproval` if
    /// the Rules Engine gated it. Infrastructure failures (not tool-logic errors)
    /// surface as `Err`.
    ///
    /// `output` is an optional live channel: a tool that produces progressive
    /// output pushes each chunk into it as it runs, so the coordinator can stream
    /// it to the UI. A tool that doesn't stream ignores it; the FINAL output is
    /// always still returned in `ToolOutcome::Ran` regardless.
    async fn execute(
        &self,
        name: &str,
        args: &str,
        output: Option<ToolOutputChannel>,
    ) -> Result<ToolOutcome, LychiError>;

    /// Run a previously-paused tool after the user approved it, using the
    /// `resume` token captured in the `NeedsApproval` outcome. Returns the tool
    /// output (or a tool error) — never `NeedsApproval` again.
    async fn run_approved(&self, resume: ResumeToken) -> Result<String, LychiError>;
}
