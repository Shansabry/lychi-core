//! The agent coordinator — the tool-calling loop.
//!
//! This is a self-contained brick (no Tauri, no UI, no DB). It drives a
//! streaming `AiProvider`, accumulates the tool calls the model requests,
//! executes them through a [`ToolExecutor`] trait, feeds results back, and loops
//! until the model returns a final answer — emitting a unified [`AgentEvent`]
//! stream the UI forwards, and pausing for human approval on destructive tools.
//!
//! Design (grounded in Pydantic-AI / Vercel / OpenAI Agents SDK / rig):
//! - **`ToolExecutor` trait** is the reuse + test seam: the real `Executor`
//!   implements it; a mock drives the whole loop in unit tests with no side
//!   effects. The coordinator is generic over it.
//! - **`AgentEvent` stream** is the coordinator's OWN output (not the provider's
//!   raw stream) — the UI subscribes to this and never sees wire details.
//! - **`Session`** is the append-only message array (serializable → free history
//!   + persistence). Executed tool results are appended BEFORE suspending, so
//!     resume never re-runs a completed tool (the key HITL invariant).
//! - **`Outcome::AwaitingApproval`** is a caller-driven suspend (not a callback):
//!   the coordinator is policy-agnostic and merely reacts to a tool result that
//!   `needs_confirmation` (the Rules Engine decides which tools do).

mod loop_;
mod session;
mod tool_executor;
mod tool_filter;

pub use loop_::{
    AgentEvent, AgentEventStream, Coordinator, MaxSteps, Outcome, OutcomeHandle, StopCondition,
};
pub use session::{ApprovalDecision, ApprovalRequest, PendingApproval, Session};
pub use tool_executor::{ResumeToken, ToolArtifact, ToolExecutor, ToolOutcome, ToolOutputChannel};
pub use tool_filter::select_tools;
