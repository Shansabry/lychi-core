//! Pin workspace action handler — lets users override IDE workspace detection.
//!
//! `pin workspace /path/to/project` — pins the workspace (session-only).
//! `pin workspace clear` or `pin workspace unpin` — clears the pin.
//! Empty args — shows current pin status.

use std::path::Path;

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CompletionItem, ExecContext, OutputType, RiskLevel,
};
use crate::context::pin;
use crate::error::LychiError;

pub struct PinWorkspaceHandler;

#[async_trait]
impl ActionHandler for PinWorkspaceHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::{ArgTransform, Trigger};
        static TRIGGERS: &[Trigger] = &[
            Trigger::new(&["pin"], ArgTransform::StripLeading("workspace ")),
            Trigger::new(&["unpin"], ArgTransform::Fixed("clear")),
        ];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "pin_workspace"
    }

    fn description(&self) -> &str {
        "Pin a workspace directory for context detection"
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let args = args.trim();

        // Show current pin status
        if args.is_empty() {
            return match pin::get() {
                Some(path) => Ok(ActionResult::ok(
                    format!("Pinned workspace: {path}"),
                    OutputType::Status,
                )),
                None => Ok(ActionResult::ok(
                    "No workspace pinned (using auto-detection)",
                    OutputType::Status,
                )),
            };
        }

        // Clear the pin
        if args == "clear" || args == "unpin" {
            pin::set(None);
            return Ok(ActionResult::ok(
                "Workspace unpinned — auto-detection resumed",
                OutputType::Status,
            ));
        }

        // Pin a new path
        let path = Path::new(args);
        if !path.is_dir() {
            return Ok(ActionResult::err(format!("Not a directory: {args}")));
        }
        let abs = path
            .canonicalize()
            .map_err(LychiError::Io)?
            .to_string_lossy()
            .into_owned();
        pin::set(Some(abs.clone()));
        Ok(ActionResult::ok(
            format!("Workspace pinned: {abs}"),
            OutputType::Status,
        ))
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let mut items = Vec::new();

        // Always offer "clear" to unpin
        if pin::get().is_some() && "clear".starts_with(partial) {
            items.push(CompletionItem {
                label: "pin workspace clear".to_string(),
                icon_path: None,
                score: 100,
                description: Some("Unpin workspace".to_string()),
                reason: None,
                thumb_b64: None,
                run: Some("pin_workspace clear".to_string()),
                ..Default::default()
            });
        }

        items
    }
}
