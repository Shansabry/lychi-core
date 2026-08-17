//! Image format conversion — a deterministic (no-AI) launcher action.
//!
//! `convert <path> to <png|jpg|webp|gif|bmp|tiff>`
//!
//! Writes a NEW file next to the original with the new extension
//! (`img.png` → `img.webp`), never overwriting the source. Logic lives in
//! `crate::files::image_ops::convert_file`.

use async_trait::async_trait;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
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

/// The canonical target FORMATS the agent chooses between. The parser also
/// accepts the `jpeg`/`tif` aliases, but the schema constrains a model to one
/// canonical spelling each. Kept next to the parser it feeds
/// (`crate::files::image_ops::convert_file`).
const CONVERT_FORMATS: &[&str] = &["png", "jpg", "webp", "gif", "bmp", "tiff"];

/// `convert`'s argument surface: one free-form action whose flat form is
/// `<path> to <format>`. The `format` Choice shares [`CONVERT_FORMATS`] with
/// the converter, so the schema's enum and the formats the parser actually
/// handles can never drift. The JSON Schema and the structured→flat adapter
/// both derive from this.
const CONVERT_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Convert an image file to another format (png, jpg, webp, gif, bmp, \
               tiff). Writes a NEW file next to the original with the new extension \
               (photo.png → photo.webp); the source is never overwritten. Use when \
               the user wants an image in a different format — e.g. a webp for the \
               web, or a png from a tiff.",
        mutates: true,
        operands: &[
            Operand {
                name: "path",
                desc: "Path to the source image, e.g. \"~/Pictures/photo.png\". \
                       ~ expands to the home directory. Must be an existing image file.",
                required: true,
                kind: ArgKind::Text,
                prefix: None,
            },
            Operand {
                name: "format",
                desc: "The target image format to convert to.",
                required: true,
                kind: ArgKind::Choice(CONVERT_FORMATS),
                prefix: Some("to"),
            },
        ],
    }],
};

/// Normalize the tool's `args` to the flat `"<path> to <fmt>"` string the
/// parser already understands. A constrained model sends the structured JSON
/// (`{"path":"img.png","format":"webp"}`); a human or legacy/flat caller sends
/// the string directly, and malformed JSON falls back to the raw string — the
/// parser handles (or rejects) it as usual. Keeps `execute` on `&str`.
fn convert_args_to_flat(args: &str) -> String {
    CONVERT_GRAMMAR
        .flatten_json(args)
        .unwrap_or_else(|| args.trim().to_string())
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
    fn usage(&self) -> &str {
        "convert <path> to <png|jpg|webp|gif|bmp|tiff>"
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(CONVERT_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Files
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Files
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // A constrained model sends `{"path":..,"format":..}`; flatten it (and
        // a plain-string caller passes through) before parsing.
        let flat = convert_args_to_flat(args);
        let Some((path_str, fmt)) = split_path_and_format(&flat) else {
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

    #[test]
    fn convert_args_flatten_from_structured_json() {
        // A constrained model sends the typed object; it flattens to the string
        // the parser already understands.
        assert_eq!(
            convert_args_to_flat(r#"{"path":"~/a/img.png","format":"webp"}"#),
            "~/a/img.png to webp"
        );
        // A path containing " to " survives: the appended format is last, and
        // the parser splits on the LAST " to ".
        assert_eq!(
            convert_args_to_flat(r#"{"path":"~/My Photos/to keep/img.png","format":"jpg"}"#),
            "~/My Photos/to keep/img.png to jpg"
        );
        // A plain-string caller (human, legacy) passes straight through.
        assert_eq!(
            convert_args_to_flat("~/a/img.png to webp"),
            "~/a/img.png to webp"
        );
        assert_eq!(convert_args_to_flat("img.png jpg"), "img.png jpg");
    }

    #[test]
    fn convert_args_malformed_json_falls_back_to_raw() {
        assert_eq!(
            convert_args_to_flat(r#"{"path": broken"#),
            r#"{"path": broken"#
        );
    }

    #[test]
    fn convert_schema_enum_lists_the_supported_formats() {
        // The grammar-derived schema's format enum must be exactly
        // CONVERT_FORMATS, so the model is constrained to formats the
        // converter actually handles.
        let schema = CONVERT_GRAMMAR.handler_schema();
        let en = schema["properties"]["format"]["enum"].as_array().unwrap();
        assert_eq!(en.len(), CONVERT_FORMATS.len());
        for f in CONVERT_FORMATS {
            assert!(en.iter().any(|e| e == f), "enum missing {f}");
        }
        assert_eq!(schema["required"], serde_json::json!(["path", "format"]));
    }

    #[test]
    fn grammar_flat_rendering_is_accepted_by_the_parser() {
        // Drift guard: the grammar's flat rendering must round-trip through
        // the hand-written parser — every format, including a path with " to ".
        for fmt in CONVERT_FORMATS {
            let flat = convert_args_to_flat(&format!(
                r#"{{"path":"~/My Photos/to keep/img.png","format":"{fmt}"}}"#
            ));
            let (p, f) = split_path_and_format(&flat).unwrap();
            assert_eq!(p, "~/My Photos/to keep/img.png");
            assert_eq!(&f, fmt);
        }
    }
}
