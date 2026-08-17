//! Zip archive creation — a deterministic (no-AI) launcher action.
//!
//! `zip <path> [<path>...] [to <out.zip>]` — bundle files/folders into a zip.
//! When no `to <out>` is given, the archive is named after the first input
//! (`~/Docs/report.pdf` → `~/Docs/report.zip`). `.tar.gz`/`.tgz` outputs produce
//! a gzip-compressed tarball instead.
//!
//! Writes a NEW archive; never overwrites an input. Creating an archive is
//! non-destructive → `Low` risk (auto-run).

use std::path::PathBuf;

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType, Trigger,
};
use crate::error::LychiError;
use crate::files::archive::{targz_paths, zip_paths};
use crate::files::paths::{expand_home, sibling_output};

pub struct ZipHandler;

impl ZipHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ZipHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse `zip a b c to out.zip` → (inputs, Some(out)); without `to`, out=None.
/// Inputs are whitespace-separated; the ` to <out>` tail is optional. (Paths
/// with spaces aren't supported here without quoting — the common case is
/// `@`-referenced or dragged paths that arrive without spaces, or a single path.)
fn parse_args(args: &str) -> (Vec<String>, Option<String>) {
    let args = args.trim();
    if let Some(idx) = args.rfind(" to ") {
        let (head, tail) = (args[..idx].trim(), args[idx + 4..].trim());
        let inputs = head.split_whitespace().map(String::from).collect();
        let out = (!tail.is_empty()).then(|| tail.to_string());
        return (inputs, out);
    }
    (args.split_whitespace().map(String::from).collect(), None)
}

/// The JSON Schema for `zip`'s args: a required `paths` array plus an optional
/// `output` archive path. Emitted as the tool's `input_schema` so a constrained
/// model sends inputs and output as separate fields instead of guessing the
/// ` to ` syntax.
fn zip_input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "paths": { "type": "array", "items": { "type": "string" }, "minItems": 1,
                       "description": "Files and/or folders to bundle, e.g. [\"~/Docs/report.pdf\", \"~/Docs/img\"]. ~ expands to the home directory. Paths must not contain spaces (the launcher's flat syntax is whitespace-separated)." },
            "output": { "type": "string",
                        "description": "Output archive path. A .zip extension (the default) writes a zip; .tar.gz or .tgz writes a gzip-compressed tarball. Omit to create <first-input-stem>.zip next to the first input (report.pdf → report.zip). Never overwrites an input." }
        },
        "required": ["paths"],
        "additionalProperties": false
    })
}

/// Normalize the tool's `args` to the flat `"<path...> [to <out>]"` string the
/// parser already understands. A constrained model sends the structured JSON
/// (`{"paths":["a","b"],"output":"out.zip"}`); a human or legacy/flat caller
/// sends the string directly. Keeps `execute` on `&str`.
fn zip_args_to_flat(args: &str) -> String {
    let t = args.trim();
    if !t.starts_with('{') {
        return t.to_string();
    }
    match serde_json::from_str::<serde_json::Value>(t) {
        Ok(v) => {
            let paths: Vec<&str> = v
                .get("paths")
                .and_then(|p| p.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            // No inputs → empty string; execute reports the usage error.
            if paths.is_empty() {
                return String::new();
            }
            let joined = paths.join(" ");
            let out = v
                .get("output")
                .and_then(|o| o.as_str())
                .unwrap_or("")
                .trim();
            if out.is_empty() {
                joined
            } else {
                format!("{joined} to {out}")
            }
        }
        // Not the JSON we expected — fall back to the raw string; the parser
        // will handle it (or reject it) as usual.
        Err(_) => t.to_string(),
    }
}

#[async_trait]
impl ActionHandler for ZipHandler {
    fn triggers(&self) -> &'static [Trigger] {
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["zip", "compress"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "zip"
    }

    fn mutates_state(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Zip files/folders: zip <path...> [to <out.zip>]"
    }
    fn usage(&self) -> &str {
        "zip <path...> [to <out.zip>]"
    }
    fn input_schema(&self) -> Option<serde_json::Value> {
        Some(zip_input_schema())
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Files
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // A constrained model sends `{"paths":..,"output":..}`; flatten it (and
        // a plain-string caller passes through) before parsing.
        let flat = zip_args_to_flat(args);
        let (input_strs, out_str) = parse_args(&flat);
        if input_strs.is_empty() {
            return Ok(ActionResult::err("Usage: zip <path...> [to <out.zip>]"));
        }
        let inputs: Vec<PathBuf> = input_strs.iter().map(|s| expand_home(s)).collect();
        for p in &inputs {
            if !p.exists() {
                return Ok(ActionResult::err(format!("No such path: {}", p.display())));
            }
        }

        // Default output: <first-input-stem>.zip next to the first input.
        let out = match out_str {
            Some(s) => expand_home(&s),
            None => sibling_output(&inputs[0], "", Some("zip")),
        };
        let out_lower = out.to_string_lossy().to_ascii_lowercase();
        let is_targz = out_lower.ends_with(".tar.gz") || out_lower.ends_with(".tgz");

        let result = tokio::task::spawn_blocking(move || {
            if is_targz {
                targz_paths(&inputs, &out)
            } else {
                zip_paths(&inputs, &out)
            }
        })
        .await
        .map_err(|e| LychiError::ExecutionFailed(format!("zip task panicked: {e}")))?;

        match result {
            Ok(out) => Ok(ActionResult::ok(
                format!("Created {}", out.display()),
                OutputType::Status,
            )),
            Err(e) => Ok(ActionResult::err(e)),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        if partial.trim().is_empty() {
            return vec![CompletionItem {
                label: "zip <path...> to archive.zip".to_string(),
                icon_path: Some("__info__".to_string()),
                score: 1,
                description: Some("Bundle files/folders into a .zip (or .tar.gz)".to_string()),
                ..Default::default()
            }];
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inputs_and_out() {
        let (ins, out) = parse_args("a.txt b.txt to bundle.zip");
        assert_eq!(ins, vec!["a.txt", "b.txt"]);
        assert_eq!(out.as_deref(), Some("bundle.zip"));
    }

    #[test]
    fn parses_inputs_without_out() {
        let (ins, out) = parse_args("a.txt b.txt");
        assert_eq!(ins, vec!["a.txt", "b.txt"]);
        assert_eq!(out, None);
    }

    #[test]
    fn zip_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; it flattens to the string
        // the parser already understands.
        assert_eq!(
            zip_args_to_flat(r#"{"paths":["a.txt","b.txt"],"output":"bundle.zip"}"#),
            "a.txt b.txt to bundle.zip"
        );
        // No output → default sibling archive.
        assert_eq!(
            zip_args_to_flat(r#"{"paths":["~/Docs/report.pdf"]}"#),
            "~/Docs/report.pdf"
        );
        // Empty output is treated as absent.
        assert_eq!(
            zip_args_to_flat(r#"{"paths":["a.txt"],"output":""}"#),
            "a.txt"
        );
        // No inputs → empty string (execute reports the usage error).
        assert_eq!(zip_args_to_flat(r#"{"paths":[]}"#), "");
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(
            zip_args_to_flat("a.txt b.txt to bundle.zip"),
            "a.txt b.txt to bundle.zip"
        );
        assert_eq!(zip_args_to_flat("a.txt"), "a.txt");
    }

    #[test]
    fn zip_args_malformed_json_falls_back_to_raw() {
        assert_eq!(
            zip_args_to_flat(r#"{"paths": broken"#),
            r#"{"paths": broken"#
        );
    }

    #[test]
    fn zip_schema_requires_paths_only() {
        let schema = zip_input_schema();
        assert_eq!(schema["required"], serde_json::json!(["paths"]));
        assert_eq!(schema["properties"]["paths"]["type"], "array");
    }
}
