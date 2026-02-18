pub mod handlers;
pub mod registry;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::LychiError;

/// Risk level for any action. Used by the Rules Engine to decide
/// whether to auto-execute, require confirmation, or deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// How the frontend should render the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputType {
    /// Shell/terminal output — render with ANSI-to-HTML, monospace `<pre>`.
    Terminal,
    /// Natural language text (AI answers, notes) — clean readable sans-serif.
    Text,
    /// Short status message (e.g. "Launched Firefox") — compact, muted.
    Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    pub success: bool,
    pub output: Option<String>,
    pub error: Option<String>,
    pub duration_ms: u64,
    /// Set to Some("ai") when AI routed this action (for ✦ indicator).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routed_by: Option<String>,
    /// If set, the frontend should open this URL (using GDK for proper Wayland focus).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_url: Option<String>,
    /// If set, the action requires user confirmation before executing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_confirmation: Option<String>,
    /// Risk level of this action (populated by rules engine).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_level: Option<RiskLevel>,
    /// How the frontend should render the output. None defaults to Status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_type: Option<OutputType>,
    /// The actual args that were executed (set by executor, not by handlers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executed_args: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionItem {
    pub label: String,
    pub icon_path: Option<String>,
    pub score: u16,
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
    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }

    /// Execute the action with the given arguments.
    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError>;

    /// Provide completions for partial input. Default: empty.
    async fn completions(&self, _partial: &str) -> Vec<CompletionItem> {
        Vec::new()
    }
}
