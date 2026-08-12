//! Image format conversion — a deterministic (no-AI) launcher action.
//!
//! `convert <path> to <png|jpg|webp|gif|bmp|tiff>`
//!
//! Writes a NEW file next to the original with the new extension
//! (`img.png` → `img.webp`), never overwriting the source. Logic lives in
//! `crate::files::image_ops::convert_file`.

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType, Trigger,
};
use crate::error::LychiError;
use crate::files::image_ops::convert_file;
use crate::files::paths::expand_home;

pub struct ConvertImageHandler;

impl ConvertImageHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ConvertImageHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Split `convert <path> to <fmt>` on the LAST ` to ` (paths may contain " to ").
fn split_path_and_format(args: &str) -> Option<(String, String)> {
    let args = args.trim();
    if let Some(idx) = args.rfind(" to ") {
        let path = args[..idx].trim().to_string();
        let fmt = args[idx + 4..].trim().to_string();
        if !path.is_empty() && !fmt.is_empty() {
            return Some((path, fmt));
        }
    }
    // Fall back to the last token as the format.
    let (path, fmt) = args.rsplit_once(char::is_whitespace)?;
    let path = path.trim();
    let fmt = fmt.trim();
    if path.is_empty() || fmt.is_empty() {
        return None;
    }
    Some((path.to_string(), fmt.to_string()))
}

#[async_trait]
impl ActionHandler for ConvertImageHandler {
    fn triggers(&self) -> &'static [Trigger] {
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["convert"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "convert"
    }

    fn mutates_state(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Convert an image: convert <path> to <png | jpg | webp | gif | bmp | tiff>"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Files
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let Some((path_str, fmt)) = split_path_and_format(args) else {
            return Ok(ActionResult::err(
                "Usage: convert <image> to <png | jpg | webp | gif | bmp | tiff>",
            ));
        };
        let src = expand_home(&path_str);
        if !src.exists() {
            return Ok(ActionResult::err(format!(
                "No such file: {}",
                src.display()
            )));
        }

        let result = tokio::task::spawn_blocking(move || convert_file(&src, &fmt))
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("convert task panicked: {e}")))?;

        match result {
            Ok(out) => Ok(ActionResult::ok(
                format!("Saved {}", out.display()),
                OutputType::Status,
            )),
            Err(e) => Ok(ActionResult::err(e)),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        if partial.trim().is_empty() {
            return vec![CompletionItem {
                label: "convert <image> to webp".to_string(),
                icon_path: Some("__info__".to_string()),
                score: 1,
                description: Some(
                    "Convert image format (png, jpg, webp, gif, bmp, tiff)".to_string(),
                ),
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
    fn splits_on_to() {
        let (p, f) = split_path_and_format("~/a/img.png to webp").unwrap();
        assert_eq!(p, "~/a/img.png");
        assert_eq!(f, "webp");
    }

    #[test]
    fn splits_trailing_token() {
        let (p, f) = split_path_and_format("img.png jpg").unwrap();
        assert_eq!(p, "img.png");
        assert_eq!(f, "jpg");
    }
}
