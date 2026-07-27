//! Image resize — a deterministic (no-AI) launcher action.
//!
//! `resize <path> to <spec>` where `<spec>` is one of:
//!   - `800x600`  — exact width×height (aspect ratio NOT preserved)
//!   - `800`      — width 800, height scaled to preserve aspect ratio
//!   - `x600`     — height 600, width scaled to preserve aspect ratio
//!   - `50%`      — scale both dimensions to 50%
//!
//! Writes a NEW file next to the original (`img.jpg` → `img_800x600.jpg`),
//! never overwriting the source. The resize/dimension logic lives in
//! `crate::files::image_ops` (shared with `convert`); this handler is the thin
//! parse-and-dispatch wrapper.

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;
use crate::files::image_ops::{parse_spec, resize_file};
use crate::files::paths::expand_home;

pub struct ResizeImageHandler;

impl ResizeImageHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ResizeImageHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Split `args` into the image path and the spec, on the LAST ` to ` (so paths
/// containing " to " still work). Returns `(path, spec_str)`.
fn split_path_and_spec(args: &str) -> Option<(String, String)> {
    let args = args.trim();
    if let Some(idx) = args.rfind(" to ") {
        let path = args[..idx].trim().to_string();
        let spec = args[idx + 4..].trim().to_string();
        if !path.is_empty() && !spec.is_empty() {
            return Some((path, spec));
        }
    }
    let (path, spec) = args.rsplit_once(char::is_whitespace)?;
    let path = path.trim();
    let spec = spec.trim();
    if path.is_empty() || spec.is_empty() {
        return None;
    }
    Some((path.to_string(), spec.to_string()))
}

#[async_trait]
impl ActionHandler for ResizeImageHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["resize"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "resize"
    }

    fn description(&self) -> &str {
        "Resize an image: resize <path> to <800x600 | 800 | x600 | 50%>"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Files
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let Some((path_str, spec_str)) = split_path_and_spec(args) else {
            return Ok(ActionResult::err(
                "Usage: resize <image> to <800x600 | 800 | x600 | 50%>",
            ));
        };
        let Some(spec) = parse_spec(&spec_str) else {
            return Ok(ActionResult::err(format!(
                "Couldn't read size \"{spec_str}\". Try 800x600, 800, x600, or 50%"
            )));
        };

        let src = expand_home(&path_str);
        if !src.exists() {
            return Ok(ActionResult::err(format!(
                "No such file: {}",
                src.display()
            )));
        }

        // Heavy work off the async runtime.
        let result = tokio::task::spawn_blocking(move || resize_file(&src, spec))
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("resize task panicked: {e}")))?;

        match result {
            Ok((out, w, h)) => Ok(ActionResult::ok(
                format!("Saved {} ({w}×{h})", out.display()),
                OutputType::Status,
            )),
            Err(e) => Ok(ActionResult::err(e)),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let partial = partial.trim();
        if partial.is_empty() {
            return vec![CompletionItem {
                label: "resize <image> to 800x600".to_string(),
                icon_path: Some("__info__".to_string()),
                score: 1,
                description: Some("Resize an image (also: 800, x600, 50%)".to_string()),
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
    fn splits_path_and_spec_on_to() {
        let (p, s) = split_path_and_spec("~/Photos/img.jpg to 800x600").unwrap();
        assert_eq!(p, "~/Photos/img.jpg");
        assert_eq!(s, "800x600");
    }

    #[test]
    fn splits_path_with_spaces_using_last_to() {
        let (p, s) = split_path_and_spec("~/My Photos/to keep/img.jpg to 50%").unwrap();
        assert_eq!(p, "~/My Photos/to keep/img.jpg");
        assert_eq!(s, "50%");
    }

    #[test]
    fn splits_on_trailing_token_without_to() {
        let (p, s) = split_path_and_spec("img.png 640").unwrap();
        assert_eq!(p, "img.png");
        assert_eq!(s, "640");
    }
}
