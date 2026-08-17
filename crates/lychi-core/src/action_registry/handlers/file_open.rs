use async_trait::async_trait;
use std::path::PathBuf;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, ExecContext, OutputType,
};
use crate::db::frecency;
use crate::error::LychiError;

#[derive(Default)]
pub struct FileOpen;

impl FileOpen {
    pub fn new() -> Self {
        Self
    }

    /// Expand ~ to home directory.
    fn expand_path(path: &str) -> PathBuf {
        if let Some(rest) = path.strip_prefix("~/") {
            dirs::home_dir()
                .map(|h| h.join(rest))
                .unwrap_or_else(|| PathBuf::from(path))
        } else {
            PathBuf::from(path)
        }
    }
}

/// `file`'s argument surface: a single free-form action whose flat form IS
/// the path. The JSON Schema and the structured→flat adapter both derive
/// from this.
const FILE_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Open a file or folder in its default application (a PDF in the \
               viewer, a folder in the file manager, an image in the image \
               viewer). Use when the user wants to OPEN a known path, not edit \
               or inspect it. The path must already exist; nothing on disk \
               changes.",
        mutates: false,
        operands: &[Operand {
            name: "path",
            desc: "Path to the file or folder to open, absolute or ~-relative \
                   (e.g. \"~/Documents/report.pdf\", \"/etc/hosts\"). Must exist.",
            required: true,
            kind: ArgKind::Text,
            prefix: None,
        }],
    }],
};

/// Normalize the tool's `args` to the flat path string `execute` reads. A
/// constrained model sends the structured JSON (`{"path":"~/x.pdf"}`); a human
/// or legacy/flat caller sends the path directly and passes through unchanged.
/// Malformed JSON falls back to the raw string; the existence check rejects it
/// with the usual "Path not found" message.
fn file_args_to_flat(args: &str) -> String {
    FILE_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
}

#[async_trait]
impl ActionHandler for FileOpen {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["file"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "file"
    }

    fn description(&self) -> &str {
        "Open a file or folder in the default application"
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(FILE_GRAMMAR)
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
        let flat = file_args_to_flat(args);
        let path_str = flat.trim();
        if path_str.is_empty() {
            return Ok(ActionResult::err(
                "Usage: file <path> or type a path starting with / or ~/".to_string(),
            ));
        }

        let expanded = Self::expand_path(path_str);

        if !expanded.exists() {
            return Ok(ActionResult::err(format!(
                "Path not found: {}",
                expanded.display()
            )));
        }

        // Record frecency access
        let _ = frecency::record(&expanded.display().to_string());

        // Convert to file:// URI for GDK-based opening on the frontend
        let file_uri = format!("file://{}", expanded.display());

        Ok(
            ActionResult::ok(format!("Opened {}", expanded.display()), OutputType::Status)
                .with_link(file_uri),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_args_flatten_from_structured_json() {
        // Drift guard: the grammar's flat rendering is the bare path `execute`
        // reads — a constrained model's typed object flattens to it.
        assert_eq!(
            file_args_to_flat(r#"{"path":"~/Documents/report.pdf"}"#),
            "~/Documents/report.pdf"
        );
        // Absent/empty path → "" (execute's usage guard fires).
        assert_eq!(file_args_to_flat("{}"), "");
        assert_eq!(file_args_to_flat(r#"{"path":""}"#), "");
        // A plain-string caller passes straight through.
        assert_eq!(file_args_to_flat("~/x.pdf"), "~/x.pdf");
        assert_eq!(file_args_to_flat(""), "");
        // Malformed JSON falls back to the raw string.
        assert_eq!(file_args_to_flat("{not json"), "{not json");
    }

    #[test]
    fn file_schema_requires_the_path() {
        let schema = FILE_GRAMMAR.handler_schema();
        assert_eq!(schema["required"], serde_json::json!(["path"]));
        assert_eq!(schema["properties"]["path"]["type"], "string");
    }
}
