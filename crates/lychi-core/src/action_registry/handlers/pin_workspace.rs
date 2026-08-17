//! Pin workspace action handler — lets users override IDE workspace detection.
//!
//! `pin workspace /path/to/project` — pins the workspace (session-only).
//! `pin workspace clear` or `pin workspace unpin` — clears the pin.
//! Empty args — shows current pin status.

use std::path::Path;

use async_trait::async_trait;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
    RiskLevel,
};
use crate::context::pin;
use crate::error::LychiError;

/// `pin_workspace`'s argument surface: one free-form action whose flat form is
/// the directory path, the literal `clear`, or empty for the status query. The
/// JSON Schema and the structured→flat adapter both derive from this.
const PIN_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Pin a workspace directory so context detection (active project, \
               run cwd) uses it instead of auto-detecting from the focused \
               window — session-only, cleared on restart. Pass `clear` to \
               unpin and resume auto-detection, or call with no arguments to \
               see the current pin.",
        mutates: true,
        operands: &[
            // Named `unpin` (not `clear`) so it can never merge with `clip`'s
            // clear flag in the group schema — the flat rendering is still the
            // literal `clear` the parser reads.
            Operand {
                name: "unpin",
                desc: "Unpin the workspace and resume auto-detection. When \
                       true, `path` does not apply.",
                required: false,
                kind: ArgKind::Bool { flag: "clear" },
                prefix: None,
            },
            Operand {
                name: "path",
                desc: "The directory to pin (must exist), e.g. \
                       \"/home/user/projects/lychi\". Omit (with `unpin` \
                       false) to report the current pin status.",
                required: false,
                kind: ArgKind::Text,
                prefix: None,
            },
        ],
    }],
};

/// Normalize the tool's `args` to the flat string the checks in `execute`
/// read. A constrained model sends the structured JSON (`{"path":"/x"}` /
/// `{"unpin":true}`); a human or legacy/flat caller sends the string directly,
/// and malformed JSON falls back to the raw string.
fn pin_args_to_flat(args: &str) -> String {
    PIN_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
}

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
    fn grammar(&self) -> Option<Grammar> {
        Some(PIN_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Personal
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // A constrained model sends `{"path":..}` / `{"unpin":true}`; flatten
        // it (and a plain-string caller passes through) to the form the checks
        // below read.
        let flat = pin_args_to_flat(args);
        let args = flat.trim();

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Drift guard: the grammar's flat renderings are exactly the strings
    /// `execute`'s checks read — `clear` verbatim for the unpin branch, the
    /// bare path for the pin branch, empty for the status query.
    #[test]
    fn pin_args_flatten_from_structured_json() {
        assert_eq!(pin_args_to_flat(r#"{"unpin":true}"#), "clear");
        assert_eq!(pin_args_to_flat(r#"{"unpin":false}"#), "");
        assert_eq!(
            pin_args_to_flat(r#"{"path":"/home/user/projects/lychi"}"#),
            "/home/user/projects/lychi"
        );
        // Nothing set → empty → the status branch.
        assert_eq!(pin_args_to_flat("{}"), "");
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(pin_args_to_flat("clear"), "clear");
        assert_eq!(pin_args_to_flat("/tmp"), "/tmp");
        // Malformed JSON → raw fallback.
        assert_eq!(pin_args_to_flat("{not json"), "{not json");
    }

    #[test]
    fn pin_grammar_is_free_form() {
        assert!(PIN_GRAMMAR.is_free_form());
        let schema = PIN_GRAMMAR.handler_schema();
        assert_eq!(schema["properties"]["unpin"]["type"], "boolean");
        assert_eq!(schema["properties"]["path"]["type"], "string");
        // Nothing is required — a bare call is the status query.
        assert_eq!(schema["required"], serde_json::json!([]));
    }
}
