//! QR code generator — `qr <text>` renders a scannable QR as inline SVG.
//!
//! SVG, NOT Unicode blocks: an "ASCII" QR made of ▀▄█ glyphs is a *terminal*
//! hack and is NOT reliably scannable in a GUI — anti-aliased text blurs the
//! module edges into grey and phone scanners reject the ambiguity (research
//! 2026-07). SVG modules are perfect crisp squares at any size. Handy for
//! beaming a URL, wifi string (`WIFI:S:net;T:WPA;P:pass;;`), or any text to a
//! phone. Fully local.

use async_trait::async_trait;
use qrcode::render::svg;
use qrcode::{EcLevel, QrCode};

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;

/// `qr`'s argument surface: a single free-form action whose flat form IS the
/// text to encode. The JSON Schema derives from this; the drift test pins its
/// rendering to `render_qr`.
const QR_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Render text or a URL as a scannable QR code, shown as a crisp SVG in \
               the launcher — handy for beaming a link, wifi credentials, or any short \
               text to a phone. Fully local; nothing is stored or sent anywhere.",
        mutates: false,
        operands: &[Operand {
            name: "text",
            desc: "The content to encode: a URL (\"https://lychi.app\"), plain text, or \
                   a wifi string in the standard format \
                   \"WIFI:S:<ssid>;T:WPA;P:<password>;;\".",
            required: true,
            kind: ArgKind::Text,
            prefix: None,
        }],
    }],
};

pub struct QrHandler;

impl QrHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for QrHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Render `text` to a scannable SVG QR (black modules, white background), or an
/// error message. The SVG has no fixed pixel size — it scales crisply to
/// whatever the frontend sets — with the spec quiet zone so scanners find the
/// edges.
fn render_qr(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Usage: qr <text or url>".to_string());
    }
    // Medium EC: robust to a little display noise without bloating the code.
    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M)
        .map_err(|e| format!("Couldn't build QR: {e}"))?;
    let svg = code
        .render::<svg::Color>()
        .min_dimensions(200, 200)
        .quiet_zone(true) // spec quiet zone — scanners need the blank border
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    Ok(svg)
}

#[async_trait]
impl ActionHandler for QrHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["qr"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "qr"
    }

    fn description(&self) -> &str {
        "Generate a QR code from text or a URL"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(QR_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Utils
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let text = partial.trim();
        if text.is_empty() {
            return Vec::new();
        }
        // Just a run affordance — the QR itself is shown on execute (too big for
        // a completion row).
        vec![
            CompletionItem::new(format!("QR: {text}"), Some("__none__".into()), 100)
                .with_run(format!("qr {text}"))
                .with_description("Enter to render"),
        ]
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        match render_qr(args) {
            // SVG output — the frontend embeds it inline, crisp and scannable.
            Ok(svg) => Ok(ActionResult::ok(svg, OutputType::Svg)),
            Err(e) => Ok(ActionResult::err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_svg_qr() {
        let svg = render_qr("https://lychi.app").unwrap();
        // Valid SVG markup with rendered modules.
        assert!(svg.contains("<svg"));
        assert!(svg.contains("viewBox") || svg.contains("width"));
        assert!(svg.contains("<path") || svg.contains("<rect"));
    }

    #[test]
    fn empty_is_usage_error() {
        assert!(render_qr("   ").is_err());
    }

    #[test]
    fn handles_wifi_string() {
        assert!(render_qr("WIFI:S:MyNet;T:WPA;P:secret;;").is_ok());
    }

    #[test]
    fn qr_args_flatten_from_structured_json() {
        // The grammar's flat rendering must be exactly what `render_qr`
        // accepts.
        let flat = QR_GRAMMAR
            .flatten_json(r#"{"text":"https://lychi.app"}"#)
            .unwrap();
        assert_eq!(flat, "https://lychi.app");
        assert!(render_qr(&flat).is_ok());
        // Flat/legacy callers pass through untouched (caller keeps raw).
        assert_eq!(QR_GRAMMAR.flatten_json("https://lychi.app"), None);
    }
}
