//! Archive extraction — a deterministic (no-AI) launcher action.
//!
//! `extract <archive> [to <dir>]` / `unzip <archive>` — unpack a `.zip`,
//! `.tar.gz`/`.tgz`, or `.tar` into a directory. Defaults to a NEW sibling
//! folder named after the archive (`report.zip` → `report/`).
//!
//! Extraction writes MANY files → `Medium` risk (confirmation) unless it lands
//! in a fresh, non-existing sibling folder (the safe common case), which is
//! `Low`. Path-traversal (zip-slip) and symlink entries are refused in
//! `crate::files::archive`.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
    RiskAssessment, RiskContext, RiskLevel, Trigger,
};
use crate::error::LychiError;
use crate::files::archive::extract_archive;
use crate::files::paths::{expand_home, sibling_output};

pub struct ExtractHandler;

impl ExtractHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ExtractHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse `extract <archive> [to <dir>]` → (archive, Some(dir)|None).
fn parse_args(args: &str) -> Option<(String, Option<String>)> {
    let args = args.trim();
    if args.is_empty() {
        return None;
    }
    if let Some(idx) = args.rfind(" to ") {
        let archive = args[..idx].trim().to_string();
        let dir = args[idx + 4..].trim().to_string();
        if !archive.is_empty() && !dir.is_empty() {
            return Some((archive, Some(dir)));
        }
    }
    Some((args.to_string(), None))
}

/// The destination directory for an archive: an explicit `dir` (expanded) or a
/// fresh sibling folder named after the archive stem (strip a trailing
/// `.tar.gz`/`.tgz`/`.zip`/`.tar`).
fn dest_for(archive: &std::path::Path, explicit: Option<&str>) -> PathBuf {
    if let Some(d) = explicit {
        return expand_home(d);
    }
    let name = archive.to_string_lossy();
    let lower = name.to_ascii_lowercase();
    // sibling_output with an empty extension gives `<stem>` next to the archive,
    // but for `.tar.gz` the stem still carries `.tar` — trim it.
    let base = sibling_output(archive, "", Some(""));
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        // sibling_output stripped only the last extension; drop a trailing `.tar`.
        let s = base.to_string_lossy();
        return PathBuf::from(s.strip_suffix(".tar").unwrap_or(&s).to_string());
    }
    base
}

/// The JSON Schema for `extract`'s args: a required `archive` path plus an
/// optional `destination` directory. Emitted as the tool's `input_schema` so a
/// constrained model sends the path and destination as separate fields instead
/// of guessing the ` to ` syntax.
fn extract_input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "archive": { "type": "string",
                         "description": "Path to the archive to unpack: .zip, .tar.gz, .tgz, or .tar. ~ expands to the home directory (e.g. \"~/Downloads/report.zip\")." },
            "destination": { "type": "string",
                             "description": "Directory to extract into. Omit for the safe default: a fresh sibling folder named after the archive (report.zip → report/). An explicit destination (or one that already exists) asks the user for confirmation." }
        },
        "required": ["archive"],
        "additionalProperties": false
    })
}

/// Normalize the tool's `args` to the flat `"<archive> [to <dir>]"` string the
/// parser already understands. A constrained model sends the structured JSON
/// (`{"archive":"a.zip","destination":"out"}`); a human or legacy/flat caller
/// sends the string directly. Runs first in BOTH `execute` and `assess_risk` so
/// the schema path can never diverge from the flat path's risk assessment.
fn extract_args_to_flat(args: &str) -> String {
    let t = args.trim();
    if !t.starts_with('{') {
        return t.to_string();
    }
    match serde_json::from_str::<serde_json::Value>(t) {
        Ok(v) => {
            let archive = v
                .get("archive")
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .trim();
            let dest = v
                .get("destination")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .trim();
            if dest.is_empty() {
                archive.to_string()
            } else {
                format!("{archive} to {dest}")
            }
        }
        // Not the JSON we expected — fall back to the raw string; the parser
        // will handle it (or reject it) as usual.
        Err(_) => t.to_string(),
    }
}

#[async_trait]
impl ActionHandler for ExtractHandler {
    fn triggers(&self) -> &'static [Trigger] {
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["extract", "unzip"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "extract"
    }

    fn mutates_state(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Extract an archive: extract <archive.zip|.tar.gz> [to <dir>]"
    }
    fn usage(&self) -> &str {
        "extract <archive.zip|.tar.gz> [to <dir>]"
    }
    fn input_schema(&self) -> Option<serde_json::Value> {
        Some(extract_input_schema())
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Files
    }

    /// Low when unpacking into a fresh (non-existing) sibling folder — the safe
    /// default; Medium (confirm) when the destination already exists or was
    /// explicitly chosen (could scatter files into a populated directory).
    fn assess_risk(&self, args: &str, _ctx: &RiskContext<'_>) -> RiskAssessment {
        // Flatten a structured tool call first so the schema path assesses the
        // same string `execute` will run.
        let flat = extract_args_to_flat(args);
        let Some((archive_str, dir)) = parse_args(&flat) else {
            return RiskAssessment::level(RiskLevel::Low);
        };
        let archive = expand_home(&archive_str);
        let dest = dest_for(&archive, dir.as_deref());
        if dir.is_some() || dest.exists() {
            RiskAssessment::confirm(format!("Extract into {}?", dest.display()))
        } else {
            RiskAssessment::level(RiskLevel::Low)
        }
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // A constrained model sends `{"archive":..,"destination":..}`; flatten
        // it (and a plain-string caller passes through) before parsing.
        let flat = extract_args_to_flat(args);
        let Some((archive_str, dir)) = parse_args(&flat) else {
            return Ok(ActionResult::err(
                "Usage: extract <archive.zip | .tar.gz> [to <dir>]",
            ));
        };
        let archive = expand_home(&archive_str);
        if !archive.exists() {
            return Ok(ActionResult::err(format!(
                "No such file: {}",
                archive.display()
            )));
        }
        let dest = dest_for(&archive, dir.as_deref());

        let result = tokio::task::spawn_blocking(move || extract_archive(&archive, &dest))
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("extract task panicked: {e}")))?;

        match result {
            Ok(ex) => Ok(ActionResult::ok(
                format!("Extracted {} file(s) into {}", ex.files, ex.dest.display()),
                OutputType::Status,
            )),
            Err(e) => Ok(ActionResult::err(e)),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        if partial.trim().is_empty() {
            return vec![CompletionItem {
                label: "extract <archive.zip>".to_string(),
                icon_path: Some("__info__".to_string()),
                score: 1,
                description: Some("Unpack a .zip / .tar.gz / .tar".to_string()),
                ..Default::default()
            }];
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parses_with_and_without_dest() {
        assert_eq!(
            parse_args("a.zip to out"),
            Some(("a.zip".to_string(), Some("out".to_string())))
        );
        assert_eq!(parse_args("a.zip"), Some(("a.zip".to_string(), None)));
        assert_eq!(parse_args("  "), None);
    }

    #[test]
    fn dest_defaults_to_sibling_stem() {
        assert_eq!(
            dest_for(Path::new("/a/report.zip"), None),
            PathBuf::from("/a/report")
        );
    }

    #[test]
    fn dest_strips_tar_gz() {
        assert_eq!(
            dest_for(Path::new("/a/bundle.tar.gz"), None),
            PathBuf::from("/a/bundle")
        );
    }

    #[test]
    fn dest_honors_explicit_dir() {
        assert_eq!(
            dest_for(Path::new("/a/x.zip"), Some("/tmp/here")),
            PathBuf::from("/tmp/here")
        );
    }

    #[test]
    fn extract_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; it flattens to the string
        // the parser already understands.
        assert_eq!(
            extract_args_to_flat(r#"{"archive":"~/Downloads/report.zip","destination":"~/out"}"#),
            "~/Downloads/report.zip to ~/out"
        );
        // No destination → just the archive (safe sibling-folder default).
        assert_eq!(
            extract_args_to_flat(r#"{"archive":"a.tar.gz"}"#),
            "a.tar.gz"
        );
        // Empty destination is treated as absent.
        assert_eq!(
            extract_args_to_flat(r#"{"archive":"a.zip","destination":""}"#),
            "a.zip"
        );
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(extract_args_to_flat("a.zip to out"), "a.zip to out");
        assert_eq!(extract_args_to_flat("a.zip"), "a.zip");
    }

    #[test]
    fn extract_args_malformed_json_falls_back_to_raw() {
        assert_eq!(
            extract_args_to_flat(r#"{"archive": broken"#),
            r#"{"archive": broken"#
        );
    }

    #[test]
    fn extract_schema_requires_archive_only() {
        let schema = extract_input_schema();
        assert_eq!(schema["required"], serde_json::json!(["archive"]));
        assert!(schema["properties"]["destination"].is_object());
    }
}
