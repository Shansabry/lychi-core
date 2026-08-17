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

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
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

/// `resize`'s argument surface: one free-form action whose flat form is
/// `<path> to <size>`. `size` stays free text because the spec mini-language
/// (`800x600` / `800` / `x600` / `50%`) is open-ended — the operand desc
/// carries the four forms `parse_spec` understands. The JSON Schema and the
/// structured→flat adapter both derive from this.
const RESIZE_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Resize an image to a target size. Writes a NEW file next to the \
               original (img.jpg → img_800x600.jpg); the source is never \
               overwritten. Use for shrinking screenshots/photos, making \
               thumbnails, or fitting an image to exact dimensions.",
        mutates: true,
        operands: &[
            Operand {
                name: "path",
                desc: "Path to the source image, e.g. \"~/Photos/img.jpg\". \
                       ~ expands to the home directory. Must be an existing image file.",
                required: true,
                kind: ArgKind::Text,
                prefix: None,
            },
            Operand {
                name: "size",
                desc: "Target size, one of four forms: \"800x600\" (exact \
                       width×height, aspect ratio NOT preserved), \"800\" (width, \
                       height scaled to keep the aspect ratio), \"x600\" (height, \
                       width scaled), or \"50%\" (scale both dimensions).",
                required: true,
                kind: ArgKind::Text,
                prefix: Some("to"),
            },
        ],
    }],
};

/// Normalize the tool's `args` to the flat `"<path> to <spec>"` string the
/// parser already understands. A constrained model sends the structured JSON
/// (`{"path":"img.jpg","size":"800x600"}`); a human or legacy/flat caller sends
/// the string directly, and malformed JSON falls back to the raw string — the
/// parser handles (or rejects) it as usual. Keeps `execute` on `&str`.
fn resize_args_to_flat(args: &str) -> String {
    RESIZE_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
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

    fn mutates_state(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Resize an image: resize <path> to <800x600 | 800 | x600 | 50%>"
    }
    fn usage(&self) -> &str {
        "resize <path> to <800x600|800|x600|50%>"
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(RESIZE_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Files
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Files
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // A constrained model sends `{"path":..,"size":..}`; flatten it (and a
        // plain-string caller passes through) before parsing.
        let flat = resize_args_to_flat(args);
        let Some((path_str, spec_str)) = split_path_and_spec(&flat) else {
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

    #[test]
    fn resize_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; it flattens to the string
        // the parser already understands.
        assert_eq!(
            resize_args_to_flat(r#"{"path":"~/Photos/img.jpg","size":"800x600"}"#),
            "~/Photos/img.jpg to 800x600"
        );
        // All four spec forms ride the same free `size` field.
        assert_eq!(
            resize_args_to_flat(r#"{"path":"img.png","size":"50%"}"#),
            "img.png to 50%"
        );
        // A path containing " to " survives: the appended spec is last, and
        // the parser splits on the LAST " to ".
        assert_eq!(
            resize_args_to_flat(r#"{"path":"~/My Photos/to keep/img.jpg","size":"x600"}"#),
            "~/My Photos/to keep/img.jpg to x600"
        );
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(
            resize_args_to_flat("~/Photos/img.jpg to 800x600"),
            "~/Photos/img.jpg to 800x600"
        );
        assert_eq!(resize_args_to_flat("img.png 640"), "img.png 640");
    }

    #[test]
    fn resize_args_malformed_json_falls_back_to_raw() {
        assert_eq!(
            resize_args_to_flat(r#"{"path": broken"#),
            r#"{"path": broken"#
        );
    }

    #[test]
    fn resize_schema_requires_path_and_size() {
        let schema = RESIZE_GRAMMAR.handler_schema();
        assert_eq!(schema["required"], serde_json::json!(["path", "size"]));
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
    }

    #[test]
    fn grammar_flat_rendering_is_accepted_by_the_parser() {
        // Drift guard: the grammar's flat rendering must round-trip through
        // the hand-written parser AND the spec mini-language, for every form.
        for spec in ["800x600", "800", "x600", "50%"] {
            let flat = resize_args_to_flat(&format!(
                r#"{{"path":"~/My Photos/to keep/img.jpg","size":"{spec}"}}"#
            ));
            let (p, s) = split_path_and_spec(&flat).unwrap();
            assert_eq!(p, "~/My Photos/to keep/img.jpg");
            assert_eq!(&s, spec);
            assert!(parse_spec(&s).is_some(), "parse_spec rejected {s}");
        }
    }
}
