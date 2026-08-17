use std::path::PathBuf;

use async_trait::async_trait;

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

/// The JSON Schema for `browse`'s args: an optional `path` — the directory to
/// open in the interactive browser panel. `path` is optional because empty args
/// are a valid request (the home directory). Emitted as the tool's
/// `input_schema`.
fn browse_input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": { "type": "string",
                      "description": "The directory to browse, absolute or ~-relative (e.g. \"~/Downloads\", \"/etc\"). Must be an existing directory, not a file. Omit for the home directory." }
        },
        "additionalProperties": false
    })
}

/// Normalize the tool's `args` to the flat path string `execute` reads. A
/// constrained model sends the structured JSON (`{"path":"~/Downloads"}`); a
/// human or legacy/flat caller sends the path directly and passes through
/// unchanged (`""` — the home directory — included).
fn browse_args_to_flat(args: &str) -> String {
    let t = args.trim();
    if !t.starts_with('{') {
        return t.to_string();
    }
    match serde_json::from_str::<serde_json::Value>(t) {
        Ok(v) => v
            .get("path")
            .and_then(|a| a.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
        // Not the JSON we expected — fall back to the raw string; the existence
        // check will reject it with the usual "Directory not found" message.
        Err(_) => t.to_string(),
    }
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
    fn input_schema(&self) -> Option<serde_json::Value> {
        Some(browse_input_schema())
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
}
