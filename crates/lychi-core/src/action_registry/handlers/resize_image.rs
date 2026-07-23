//! Image resize — a deterministic (no-AI) launcher action.
//!
//! `resize <path> to <spec>` where `<spec>` is one of:
//!   - `800x600`  — exact width×height (aspect ratio NOT preserved)
//!   - `800`      — width 800, height scaled to preserve aspect ratio
//!   - `x600`     — height 600, width scaled to preserve aspect ratio
//!   - `50%`      — scale both dimensions to 50%
//!
//! Writes a NEW file next to the original (`img.jpg` → `img_800x600.jpg`),
//! never overwriting the source. Format is inferred from the original
//! extension. Fully local; no network, no AI.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;

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

/// A parsed resize target.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ResizeSpec {
    /// Exact dimensions (aspect ratio not preserved).
    Exact(u32, u32),
    /// Fixed width, height scaled to preserve aspect.
    Width(u32),
    /// Fixed height, width scaled to preserve aspect.
    Height(u32),
    /// Percentage scale of both dimensions.
    Percent(f32),
}

/// Split `args` into the image path and the spec, on the LAST ` to ` (so paths
/// containing " to " still work). Returns `(path, spec_str)`.
fn split_path_and_spec(args: &str) -> Option<(String, String)> {
    let args = args.trim();
    // Prefer an explicit " to " separator.
    if let Some(idx) = args.rfind(" to ") {
        let path = args[..idx].trim().to_string();
        let spec = args[idx + 4..].trim().to_string();
        if !path.is_empty() && !spec.is_empty() {
            return Some((path, spec));
        }
    }
    // Fall back to the last whitespace-separated token as the spec.
    let (path, spec) = args.rsplit_once(char::is_whitespace)?;
    let path = path.trim();
    let spec = spec.trim();
    if path.is_empty() || spec.is_empty() {
        return None;
    }
    Some((path.to_string(), spec.to_string()))
}

/// Parse a spec string (`800x600`, `800`, `x600`, `50%`).
fn parse_spec(spec: &str) -> Option<ResizeSpec> {
    let spec = spec.trim().to_lowercase();
    if let Some(pct) = spec.strip_suffix('%') {
        let p: f32 = pct.trim().parse().ok()?;
        if p > 0.0 {
            return Some(ResizeSpec::Percent(p / 100.0));
        }
        return None;
    }
    if let Some((w, h)) = spec.split_once('x') {
        let w = w.trim();
        let h = h.trim();
        return match (w.is_empty(), h.is_empty()) {
            // "x600" → height only
            (true, false) => h.parse().ok().filter(|n| *n > 0).map(ResizeSpec::Height),
            // "800x" → width only
            (false, true) => w.parse().ok().filter(|n| *n > 0).map(ResizeSpec::Width),
            // "800x600" → exact
            (false, false) => {
                let w: u32 = w.parse().ok().filter(|n| *n > 0)?;
                let h: u32 = h.parse().ok().filter(|n| *n > 0)?;
                Some(ResizeSpec::Exact(w, h))
            }
            (true, true) => None,
        };
    }
    // Bare number → width, aspect-preserving.
    spec.parse().ok().filter(|n| *n > 0).map(ResizeSpec::Width)
}

/// Expand a leading `~` to the home directory.
fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(path)
}

/// Compute the output dimensions from the source size and spec.
fn target_dims(spec: ResizeSpec, src_w: u32, src_h: u32) -> (u32, u32) {
    match spec {
        ResizeSpec::Exact(w, h) => (w.max(1), h.max(1)),
        ResizeSpec::Width(w) => {
            let w = w.max(1);
            let h = ((w as f64 / src_w as f64) * src_h as f64).round() as u32;
            (w, h.max(1))
        }
        ResizeSpec::Height(h) => {
            let h = h.max(1);
            let w = ((h as f64 / src_h as f64) * src_w as f64).round() as u32;
            (w.max(1), h)
        }
        ResizeSpec::Percent(p) => {
            let w = ((src_w as f32) * p).round() as u32;
            let h = ((src_h as f32) * p).round() as u32;
            (w.max(1), h.max(1))
        }
    }
}

/// Build the output path: `img.jpg` + `800x600` → `img_800x600.jpg`.
fn output_path(src: &Path, w: u32, h: u32) -> PathBuf {
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("image");
    let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("png");
    let file = format!("{stem}_{w}x{h}.{ext}");
    match src.parent() {
        Some(dir) => dir.join(file),
        None => PathBuf::from(file),
    }
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
        // Only hint the syntax; the path itself is typed/pasted by the user.
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

/// Load, resize, and save. Returns `(output_path, out_w, out_h)` or an error msg.
fn resize_file(src: &Path, spec: ResizeSpec) -> Result<(PathBuf, u32, u32), String> {
    let img = image::open(src).map_err(|e| format!("Couldn't open image: {e}"))?;
    let (src_w, src_h) = (img.width(), img.height());
    if src_w == 0 || src_h == 0 {
        return Err("Image has zero dimensions".to_string());
    }
    let (w, h) = target_dims(spec, src_w, src_h);

    // Exact keeps both dims; the others already preserve aspect via target_dims,
    // so `resize_exact` applies the computed dimensions in every case.
    let resized = img.resize_exact(w, h, image::imageops::FilterType::Lanczos3);

    let out = output_path(src, w, h);
    resized
        .save(&out)
        .map_err(|e| format!("Couldn't save resized image: {e}"))?;
    Ok((out, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact() {
        assert_eq!(parse_spec("800x600"), Some(ResizeSpec::Exact(800, 600)));
        assert_eq!(parse_spec("800 x 600"), Some(ResizeSpec::Exact(800, 600)));
    }

    #[test]
    fn parses_width_only() {
        assert_eq!(parse_spec("800"), Some(ResizeSpec::Width(800)));
        assert_eq!(parse_spec("800x"), Some(ResizeSpec::Width(800)));
    }

    #[test]
    fn parses_height_only() {
        assert_eq!(parse_spec("x600"), Some(ResizeSpec::Height(600)));
    }

    #[test]
    fn parses_percent() {
        assert_eq!(parse_spec("50%"), Some(ResizeSpec::Percent(0.5)));
        assert_eq!(parse_spec("150%"), Some(ResizeSpec::Percent(1.5)));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_spec("big"), None);
        assert_eq!(parse_spec("0x0"), None);
        assert_eq!(parse_spec("-5"), None);
        assert_eq!(parse_spec("0%"), None);
    }

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

    #[test]
    fn width_preserves_aspect() {
        // 1000x500 → width 500 → height 250.
        assert_eq!(target_dims(ResizeSpec::Width(500), 1000, 500), (500, 250));
    }

    #[test]
    fn height_preserves_aspect() {
        // 1000x500 → height 250 → width 500.
        assert_eq!(target_dims(ResizeSpec::Height(250), 1000, 500), (500, 250));
    }

    #[test]
    fn percent_scales_both() {
        assert_eq!(target_dims(ResizeSpec::Percent(0.5), 1000, 500), (500, 250));
    }

    #[test]
    fn exact_ignores_aspect() {
        assert_eq!(
            target_dims(ResizeSpec::Exact(800, 800), 1000, 500),
            (800, 800)
        );
    }

    #[test]
    fn output_path_appends_dimensions() {
        let out = output_path(Path::new("/home/u/Photos/img.jpg"), 800, 600);
        assert_eq!(out, PathBuf::from("/home/u/Photos/img_800x600.jpg"));
    }
}
