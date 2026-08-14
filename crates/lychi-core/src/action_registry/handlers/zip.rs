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
    fn category(&self) -> CommandCategory {
        CommandCategory::Files
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let (input_strs, out_str) = parse_args(args);
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
}
