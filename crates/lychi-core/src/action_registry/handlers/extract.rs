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
    fn category(&self) -> CommandCategory {
        CommandCategory::Files
    }

    /// Low when unpacking into a fresh (non-existing) sibling folder — the safe
    /// default; Medium (confirm) when the destination already exists or was
    /// explicitly chosen (could scatter files into a populated directory).
    fn assess_risk(&self, args: &str, _ctx: &RiskContext<'_>) -> RiskAssessment {
        let Some((archive_str, dir)) = parse_args(args) else {
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
        let Some((archive_str, dir)) = parse_args(args) else {
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
}
