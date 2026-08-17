use std::path::PathBuf;

use async_trait::async_trait;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, ExecContext, OutputType,
};
use crate::error::LychiError;

#[derive(Default)]
pub struct BrowseHandler;

impl BrowseHandler {
    pub fn new() -> Self {
        Self
    }

    fn expand_path(path: &str) -> PathBuf {
        if let Some(rest) = path.strip_prefix("~/") {
            dirs::home_dir()
                .map(|h| h.join(rest))
                .unwrap_or_else(|| PathBuf::from(path))
        } else if path == "~" {
            dirs::home_dir().unwrap_or_else(|| PathBuf::from(path))
        } else {
            PathBuf::from(path)
        }
    }
}

/// `browse`'s argument surface: a single free-form action whose flat form IS
/// the path. `path` is optional because empty args are a valid request (the
/// home directory). The JSON Schema and the structured→flat adapter both
/// derive from this.
const BROWSE_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Open a directory in the launcher's interactive file-browser panel, where \
               the user can navigate, preview, and act on entries. Use ONLY to open a \
               whole folder without filtering (e.g. the Downloads folder); when the user \
               names specific files or search terms, list or find them with a shell \
               command instead. Read-only: nothing on disk changes.",
        mutates: false,
        operands: &[Operand {
            name: "path",
            desc: "The directory to browse, absolute or ~-relative (e.g. \"~/Downloads\", \
                   \"/etc\"). Must be an existing directory, not a file. Omit for the \
                   home directory.",
            required: false,
            kind: ArgKind::Text,
            prefix: None,
        }],
    }],
};

/// Normalize the tool's `args` to the flat path string `execute` reads. A
/// constrained model sends the structured JSON (`{"path":"~/Downloads"}`); a
/// human or legacy/flat caller sends the path directly and passes through
/// unchanged (`""` — the home directory — included). Malformed JSON falls back
/// to the raw string; the existence check rejects it with the usual
/// "Directory not found" message.
fn browse_args_to_flat(args: &str) -> String {
    BROWSE_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
}

#[async_trait]
impl ActionHandler for BrowseHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["browse"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "browse"
    }

    fn description(&self) -> &str {
        "Browse files in a directory interactively"
    }
    fn usage(&self) -> &str {
        "ONLY use to open/browse a whole folder without filtering (e.g. 'browse downloads'). If the user mentions specific filenames or search terms, use 'run' with ls/find instead"
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(BROWSE_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Files
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Files
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // A constrained model sends `{"path":..}`; flatten it (a plain-string
        // caller passes through) to the bare path the checks below read.
        let flat = browse_args_to_flat(args);
        let path_str = flat.trim();
        let dir = if path_str.is_empty() { "~/" } else { path_str };

        let expanded = Self::expand_path(dir);

        if !expanded.exists() || !expanded.is_dir() {
            return Ok(ActionResult::err(format!("Directory not found: {dir}")));
        }

        // Ensure the path ends with / for the frontend @ mode
        let browse_path = if dir.ends_with('/') {
            dir.to_string()
        } else {
            format!("{dir}/")
        };

        Ok(ActionResult::ok(
            format!("__browse_panel__:{browse_path}"),
            OutputType::Status,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browse_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; it flattens to the bare
        // path the existence checks read.
        assert_eq!(
            browse_args_to_flat(r#"{"path":"~/Downloads"}"#),
            "~/Downloads"
        );
        // Absent/empty path → "" (the home-directory default).
        assert_eq!(browse_args_to_flat("{}"), "");
        assert_eq!(browse_args_to_flat(r#"{"path":""}"#), "");
        // A plain-string caller passes straight through.
        assert_eq!(browse_args_to_flat("~/Downloads"), "~/Downloads");
        assert_eq!(browse_args_to_flat(""), "");
        // Malformed JSON falls back to the raw string.
        assert_eq!(browse_args_to_flat("{not json"), "{not json");
    }

    #[test]
    fn browse_schema_keeps_path_optional() {
        // The grammar-derived schema must keep `path` optional — empty args
        // are a valid request (the home directory).
        let schema = BROWSE_GRAMMAR.handler_schema();
        assert!(schema["properties"]["path"].is_object());
        assert_eq!(schema["required"], serde_json::json!([]));
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
    }
}
