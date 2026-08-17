use std::process::{Command, Stdio};

use async_trait::async_trait;

use crate::action_registry::grammar::{ArgKind, Grammar, Operand, ToolGroup, Verb};
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;

use super::clipboard::write_to_clipboard;

/// Internal RGB representation.
#[derive(Debug, Clone, Copy)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

impl Rgb {
    fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    fn to_rgb_string(self) -> String {
        format!("rgb({}, {}, {})", self.r, self.g, self.b)
    }

    fn to_hsl_string(self) -> String {
        let (h, s, l) = rgb_to_hsl(self.r, self.g, self.b);
        format!("hsl({h}, {s}%, {l}%)")
    }

    fn all_formats(self) -> String {
        format!(
            "HEX: {}\nRGB: {}\nHSL: {}",
            self.to_hex(),
            self.to_rgb_string(),
            self.to_hsl_string()
        )
    }

    /// Euclidean distance squared in RGB space.
    fn distance_sq(self, other: Rgb) -> u32 {
        let dr = self.r as i32 - other.r as i32;
        let dg = self.g as i32 - other.g as i32;
        let db = self.b as i32 - other.b as i32;
        (dr * dr + dg * dg + db * db) as u32
    }
}

// ── Parsing ────────────────────────────────────────────────────────────

/// Parse a color from any supported format.
fn parse_color(input: &str) -> Option<Rgb> {
    let input = input.trim();

    // Try hex first
    if let Some(rgb) = parse_hex(input) {
        return Some(rgb);
    }

    // Try rgb()
    if let Some(rgb) = parse_rgb_func(input) {
        return Some(rgb);
    }

    // Try hsl()
    if let Some(rgb) = parse_hsl_func(input) {
        return Some(rgb);
    }

    None
}

fn parse_hex(input: &str) -> Option<Rgb> {
    let hex = input.strip_prefix('#').unwrap_or(input);

    // Validate hex chars
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    match hex.len() {
        3 => {
            // Shorthand: #F53 → #FF5533
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            Some(Rgb { r, g, b })
        }
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Rgb { r, g, b })
        }
        8 => {
            // RRGGBBAA — ignore alpha
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Rgb { r, g, b })
        }
        _ => None,
    }
}

fn parse_rgb_func(input: &str) -> Option<Rgb> {
    let inner = input
        .strip_prefix("rgb(")
        .or_else(|| input.strip_prefix("RGB("))
        .and_then(|s| s.strip_suffix(')'))?;

    let parts: Vec<&str> = inner.split(',').map(|s| s.trim()).collect();
    if parts.len() != 3 {
        return None;
    }

    let r: u8 = parts[0].parse().ok()?;
    let g: u8 = parts[1].parse().ok()?;
    let b: u8 = parts[2].parse().ok()?;
    Some(Rgb { r, g, b })
}

fn parse_hsl_func(input: &str) -> Option<Rgb> {
    let inner = input
        .strip_prefix("hsl(")
        .or_else(|| input.strip_prefix("HSL("))
        .and_then(|s| s.strip_suffix(')'))?;

    let parts: Vec<&str> = inner
        .split(',')
        .map(|s| s.trim().trim_end_matches('%'))
        .collect();
    if parts.len() != 3 {
        return None;
    }

    let h: f64 = parts[0].parse().ok()?;
    let s: f64 = parts[1].parse().ok()?;
    let l: f64 = parts[2].parse().ok()?;

    Some(hsl_to_rgb(h, s, l))
}

// ── Color space conversions ─────────────────────────────────────────────

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (u16, u16, u16) {
    let r = r as f64 / 255.0;
    let g = g as f64 / 255.0;
    let b = b as f64 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;

    if (max - min).abs() < f64::EPSILON {
        return (0, 0, (l * 100.0).round() as u16);
    }

    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };

    let h = if (max - r).abs() < f64::EPSILON {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if (max - g).abs() < f64::EPSILON {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };

    (
        (h * 360.0).round() as u16,
        (s * 100.0).round() as u16,
        (l * 100.0).round() as u16,
    )
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> Rgb {
    let s = s / 100.0;
    let l = l / 100.0;
    let h = ((h % 360.0) + 360.0) % 360.0; // normalize

    if s.abs() < f64::EPSILON {
        let v = (l * 255.0).round() as u8;
        return Rgb { r: v, g: v, b: v };
    }

    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;

    let hue_to_rgb = |t: f64| -> f64 {
        let t = ((t % 1.0) + 1.0) % 1.0;
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 1.0 / 2.0 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };

    let h_norm = h / 360.0;
    Rgb {
        r: (hue_to_rgb(h_norm + 1.0 / 3.0) * 255.0).round() as u8,
        g: (hue_to_rgb(h_norm) * 255.0).round() as u8,
        b: (hue_to_rgb(h_norm - 1.0 / 3.0) * 255.0).round() as u8,
    }
}

// ── Tailwind colors ─────────────────────────────────────────────────────

/// Nearest Tailwind color name by Euclidean distance in RGB space.
fn nearest_tailwind(color: Rgb) -> (&'static str, u32) {
    TAILWIND_COLORS
        .iter()
        .map(|&(name, r, g, b)| (name, color.distance_sq(Rgb { r, g, b })))
        .min_by_key(|&(_, d)| d)
        .unwrap_or(("unknown", u32::MAX))
}

/// Subset of Tailwind CSS v3 color palette (all 50-950 shades).
static TAILWIND_COLORS: &[(&str, u8, u8, u8)] = &[
    // slate
    ("slate-50", 248, 250, 252),
    ("slate-100", 241, 245, 249),
    ("slate-200", 226, 232, 240),
    ("slate-300", 203, 213, 225),
    ("slate-400", 148, 163, 184),
    ("slate-500", 100, 116, 139),
    ("slate-600", 71, 85, 105),
    ("slate-700", 51, 65, 85),
    ("slate-800", 30, 41, 59),
    ("slate-900", 15, 23, 42),
    ("slate-950", 2, 6, 23),
    // gray
    ("gray-50", 249, 250, 251),
    ("gray-100", 243, 244, 246),
    ("gray-200", 229, 231, 235),
    ("gray-300", 209, 213, 219),
    ("gray-400", 156, 163, 175),
    ("gray-500", 107, 114, 128),
    ("gray-600", 75, 85, 99),
    ("gray-700", 55, 65, 81),
    ("gray-800", 31, 41, 55),
    ("gray-900", 17, 24, 39),
    ("gray-950", 3, 7, 18),
    // zinc
    ("zinc-50", 250, 250, 250),
    ("zinc-100", 244, 244, 245),
    ("zinc-200", 228, 228, 231),
    ("zinc-300", 212, 212, 216),
    ("zinc-400", 161, 161, 170),
    ("zinc-500", 113, 113, 122),
    ("zinc-600", 82, 82, 91),
    ("zinc-700", 63, 63, 70),
    ("zinc-800", 39, 39, 42),
    ("zinc-900", 24, 24, 27),
    ("zinc-950", 9, 9, 11),
    // neutral
    ("neutral-50", 250, 250, 250),
    ("neutral-100", 245, 245, 245),
    ("neutral-200", 229, 229, 229),
    ("neutral-300", 212, 212, 212),
    ("neutral-400", 163, 163, 163),
    ("neutral-500", 115, 115, 115),
    ("neutral-600", 82, 82, 82),
    ("neutral-700", 64, 64, 64),
    ("neutral-800", 38, 38, 38),
    ("neutral-900", 23, 23, 23),
    ("neutral-950", 10, 10, 10),
    // red
    ("red-50", 254, 242, 242),
    ("red-100", 254, 226, 226),
    ("red-200", 254, 202, 202),
    ("red-300", 252, 165, 165),
    ("red-400", 248, 113, 113),
    ("red-500", 239, 68, 68),
    ("red-600", 220, 38, 38),
    ("red-700", 185, 28, 28),
    ("red-800", 153, 27, 27),
    ("red-900", 127, 29, 29),
    ("red-950", 69, 10, 10),
    // orange
    ("orange-50", 255, 247, 237),
    ("orange-100", 255, 237, 213),
    ("orange-200", 254, 215, 170),
    ("orange-300", 253, 186, 116),
    ("orange-400", 251, 146, 60),
    ("orange-500", 249, 115, 22),
    ("orange-600", 234, 88, 12),
    ("orange-700", 194, 65, 12),
    ("orange-800", 154, 52, 18),
    ("orange-900", 124, 45, 18),
    ("orange-950", 67, 20, 7),
    // amber
    ("amber-50", 255, 251, 235),
    ("amber-100", 254, 243, 199),
    ("amber-200", 253, 230, 138),
    ("amber-300", 252, 211, 77),
    ("amber-400", 251, 191, 36),
    ("amber-500", 245, 158, 11),
    ("amber-600", 217, 119, 6),
    ("amber-700", 180, 83, 9),
    ("amber-800", 146, 64, 14),
    ("amber-900", 120, 53, 15),
    ("amber-950", 69, 26, 3),
    // yellow
    ("yellow-50", 254, 252, 232),
    ("yellow-100", 254, 249, 195),
    ("yellow-200", 254, 240, 138),
    ("yellow-300", 253, 224, 71),
    ("yellow-400", 250, 204, 21),
    ("yellow-500", 234, 179, 8),
    ("yellow-600", 202, 138, 4),
    ("yellow-700", 161, 98, 7),
    ("yellow-800", 133, 77, 14),
    ("yellow-900", 113, 63, 18),
    ("yellow-950", 66, 32, 6),
    // lime
    ("lime-50", 247, 254, 231),
    ("lime-100", 236, 252, 203),
    ("lime-200", 217, 249, 157),
    ("lime-300", 190, 242, 100),
    ("lime-400", 163, 230, 53),
    ("lime-500", 132, 204, 22),
    ("lime-600", 101, 163, 13),
    ("lime-700", 77, 124, 15),
    ("lime-800", 63, 98, 18),
    ("lime-900", 54, 83, 20),
    ("lime-950", 26, 46, 5),
    // green
    ("green-50", 240, 253, 244),
    ("green-100", 220, 252, 231),
    ("green-200", 187, 247, 208),
    ("green-300", 134, 239, 172),
    ("green-400", 74, 222, 128),
    ("green-500", 34, 197, 94),
    ("green-600", 22, 163, 74),
    ("green-700", 21, 128, 61),
    ("green-800", 22, 101, 52),
    ("green-900", 20, 83, 45),
    ("green-950", 5, 46, 22),
    // emerald
    ("emerald-50", 236, 253, 245),
    ("emerald-100", 209, 250, 229),
    ("emerald-200", 167, 243, 208),
    ("emerald-300", 110, 231, 183),
    ("emerald-400", 52, 211, 153),
    ("emerald-500", 16, 185, 129),
    ("emerald-600", 5, 150, 105),
    ("emerald-700", 4, 120, 87),
    ("emerald-800", 6, 95, 70),
    ("emerald-900", 6, 78, 59),
    ("emerald-950", 2, 44, 34),
    // teal
    ("teal-50", 240, 253, 250),
    ("teal-100", 204, 251, 241),
    ("teal-200", 153, 246, 228),
    ("teal-300", 94, 234, 212),
    ("teal-400", 45, 212, 191),
    ("teal-500", 20, 184, 166),
    ("teal-600", 13, 148, 136),
    ("teal-700", 15, 118, 110),
    ("teal-800", 17, 94, 89),
    ("teal-900", 19, 78, 74),
    ("teal-950", 4, 47, 46),
    // cyan
    ("cyan-50", 236, 254, 255),
    ("cyan-100", 207, 250, 254),
    ("cyan-200", 165, 243, 252),
    ("cyan-300", 103, 232, 249),
    ("cyan-400", 34, 211, 238),
    ("cyan-500", 6, 182, 212),
    ("cyan-600", 8, 145, 178),
    ("cyan-700", 14, 116, 144),
    ("cyan-800", 21, 94, 117),
    ("cyan-900", 22, 78, 99),
    ("cyan-950", 8, 51, 68),
    // sky
    ("sky-50", 240, 249, 255),
    ("sky-100", 224, 242, 254),
    ("sky-200", 186, 230, 253),
    ("sky-300", 125, 211, 252),
    ("sky-400", 56, 189, 248),
    ("sky-500", 14, 165, 233),
    ("sky-600", 2, 132, 199),
    ("sky-700", 3, 105, 161),
    ("sky-800", 7, 89, 133),
    ("sky-900", 12, 74, 110),
    ("sky-950", 8, 47, 73),
    // blue
    ("blue-50", 239, 246, 255),
    ("blue-100", 219, 234, 254),
    ("blue-200", 191, 219, 254),
    ("blue-300", 147, 197, 253),
    ("blue-400", 96, 165, 250),
    ("blue-500", 59, 130, 246),
    ("blue-600", 37, 99, 235),
    ("blue-700", 29, 78, 216),
    ("blue-800", 30, 64, 175),
    ("blue-900", 30, 58, 138),
    ("blue-950", 23, 37, 84),
    // indigo
    ("indigo-50", 238, 242, 255),
    ("indigo-100", 224, 231, 255),
    ("indigo-200", 199, 210, 254),
    ("indigo-300", 165, 180, 252),
    ("indigo-400", 129, 140, 248),
    ("indigo-500", 99, 102, 241),
    ("indigo-600", 79, 70, 229),
    ("indigo-700", 67, 56, 202),
    ("indigo-800", 55, 48, 163),
    ("indigo-900", 49, 46, 129),
    ("indigo-950", 30, 27, 75),
    // violet
    ("violet-50", 245, 243, 255),
    ("violet-100", 237, 233, 254),
    ("violet-200", 221, 214, 254),
    ("violet-300", 196, 181, 253),
    ("violet-400", 167, 139, 250),
    ("violet-500", 139, 92, 246),
    ("violet-600", 124, 58, 237),
    ("violet-700", 109, 40, 217),
    ("violet-800", 91, 33, 182),
    ("violet-900", 76, 29, 149),
    ("violet-950", 46, 16, 101),
    // purple
    ("purple-50", 250, 245, 255),
    ("purple-100", 243, 232, 255),
    ("purple-200", 233, 213, 255),
    ("purple-300", 216, 180, 254),
    ("purple-400", 192, 132, 252),
    ("purple-500", 168, 85, 247),
    ("purple-600", 147, 51, 234),
    ("purple-700", 126, 34, 206),
    ("purple-800", 107, 33, 168),
    ("purple-900", 88, 28, 135),
    ("purple-950", 59, 7, 100),
    // fuchsia
    ("fuchsia-50", 253, 244, 255),
    ("fuchsia-100", 250, 232, 255),
    ("fuchsia-200", 245, 208, 254),
    ("fuchsia-300", 240, 171, 252),
    ("fuchsia-400", 232, 121, 249),
    ("fuchsia-500", 217, 70, 239),
    ("fuchsia-600", 192, 38, 211),
    ("fuchsia-700", 162, 28, 175),
    ("fuchsia-800", 134, 25, 143),
    ("fuchsia-900", 112, 26, 117),
    ("fuchsia-950", 74, 4, 78),
    // pink
    ("pink-50", 253, 242, 248),
    ("pink-100", 252, 231, 243),
    ("pink-200", 251, 207, 232),
    ("pink-300", 249, 168, 212),
    ("pink-400", 244, 114, 182),
    ("pink-500", 236, 72, 153),
    ("pink-600", 219, 39, 119),
    ("pink-700", 190, 24, 93),
    ("pink-800", 157, 23, 77),
    ("pink-900", 131, 24, 67),
    ("pink-950", 80, 7, 36),
    // rose
    ("rose-50", 255, 241, 242),
    ("rose-100", 255, 228, 230),
    ("rose-200", 254, 205, 211),
    ("rose-300", 253, 164, 175),
    ("rose-400", 251, 113, 133),
    ("rose-500", 244, 63, 94),
    ("rose-600", 225, 29, 72),
    ("rose-700", 190, 18, 60),
    ("rose-800", 159, 18, 57),
    ("rose-900", 136, 19, 55),
    ("rose-950", 76, 5, 25),
    // white & black
    ("white", 255, 255, 255),
    ("black", 0, 0, 0),
];

// ── System color picker ─────────────────────────────────────────────────

/// Try to launch a system color picker and return the selected hex color.
fn pick_system_color() -> Result<String, LychiError> {
    // Detection chain: hyprpicker → kcolorchooser → xcolor → gpick
    let tools = [
        ("hyprpicker", &["--autocopy"][..]),
        ("kcolorchooser", &[]),
        ("xcolor", &[]),
        ("gpick", &["--pick"][..]),
    ];

    for (tool, args) in &tools {
        if which::which(tool).is_ok() {
            let output = Command::new(tool)
                .args(*args)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .map_err(|e| LychiError::ExecutionFailed(format!("Failed to run {tool}: {e}")))?;

            if output.status.success() {
                let hex = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !hex.is_empty() {
                    return Ok(hex);
                }
            }
        }
    }

    Err(LychiError::ExecutionFailed(
        "No color picker found. Install one of: hyprpicker, kcolorchooser, xcolor, gpick"
            .to_string(),
    ))
}

// ── Handler ─────────────────────────────────────────────────────────────

/// `color`'s argument surface: a single free-form action whose flat form IS
/// the color value (or the `picker` subcommand). The JSON Schema derives from
/// this; the drift test pins its renderings to `parse_color` and the picker
/// match in `execute`.
const COLOR_GRAMMAR: Grammar = Grammar {
    verbs: &[Verb {
        name: "",
        desc: "Convert a color between hex, RGB, and HSL, name the nearest Tailwind CSS \
               color, and copy the hex value to the clipboard. Fully local, instant, \
               read-only.",
        mutates: false,
        operands: &[Operand {
            name: "value",
            desc: "The color to convert, in any supported format: hex \"#FF5733\", \
                   shorthand \"#F53\", bare \"ff5733\", or 8-digit-with-alpha \
                   \"#FF5733FF\" (alpha ignored); \"rgb(255, 87, 51)\"; \
                   \"hsl(11, 100%, 60%)\". The special value \"picker\" opens the \
                   system color picker (hyprpicker/kcolorchooser/xcolor/gpick) and \
                   converts whatever the user picks on screen.",
            required: true,
            kind: ArgKind::Text,
            prefix: None,
        }],
    }],
};

pub struct ColorHandler;

impl Default for ColorHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ColorHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ActionHandler for ColorHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["color", "colour"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "color"
    }

    fn description(&self) -> &str {
        "Convert colors between hex, RGB, HSL and find nearest Tailwind match"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }
    fn grammar(&self) -> Option<Grammar> {
        Some(COLOR_GRAMMAR)
    }
    fn tool_group(&self) -> ToolGroup {
        ToolGroup::Utils
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let args = args.trim();

        if args.is_empty() {
            return Ok(ActionResult::ok(
                "Usage:\n  color #FF5733      — convert hex color\n  color rgb(255,87,51) — convert RGB\n  color hsl(11,100%,60%) — convert HSL\n  color picker        — open system color picker",
                OutputType::Text,
            ));
        }

        // System color picker subcommand
        if args.eq_ignore_ascii_case("picker") || args.eq_ignore_ascii_case("pick") {
            let hex = pick_system_color()?;
            if let Some(rgb) = parse_color(&hex) {
                let (tw_name, _) = nearest_tailwind(rgb);
                let _ = write_to_clipboard(&rgb.to_hex());
                return Ok(ActionResult::ok(
                    format!(
                        "{}\nTailwind: {tw_name}\n\n📋 Hex copied to clipboard",
                        rgb.all_formats()
                    ),
                    OutputType::Terminal,
                ));
            }
            // Couldn't parse picker output — just return it
            return Ok(ActionResult::ok(hex, OutputType::Terminal));
        }

        // Parse the color input
        let rgb = match parse_color(args) {
            Some(c) => c,
            None => {
                return Ok(ActionResult::err(format!(
                    "Couldn't parse color: '{args}'\nSupported: #hex, rgb(r,g,b), hsl(h,s%,l%)"
                )));
            }
        };

        let (tw_name, tw_dist) = nearest_tailwind(rgb);
        let tw_label = if tw_dist == 0 {
            format!("Tailwind: {tw_name} (exact)")
        } else {
            format!("Tailwind: ~{tw_name}")
        };

        let _ = write_to_clipboard(&rgb.to_hex());

        Ok(ActionResult::ok(
            format!(
                "{}\n{tw_label}\n\n📋 Hex copied to clipboard",
                rgb.all_formats()
            ),
            OutputType::Terminal,
        ))
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let partial = partial.trim();

        // If input looks like a parseable color, show live preview
        if let Some(rgb) = parse_color(partial) {
            let (tw_name, _) = nearest_tailwind(rgb);
            let hex = rgb.to_hex();
            return vec![
                CompletionItem::new(hex.clone(), None, 1000)
                    .with_run(format!("color {hex}"))
                    .with_description(format!(
                        "{} · {} · ~{tw_name}",
                        rgb.to_rgb_string(),
                        rgb.to_hsl_string()
                    )),
            ];
        }

        // Static hints
        vec![
            CompletionItem {
                label: "picker".to_string(),
                icon_path: None,
                score: 900,
                description: Some("Open system color picker".to_string()),
                reason: None,
                thumb_b64: None,
                run: Some("color picker".to_string()),
                ..Default::default()
            },
            CompletionItem {
                label: "#hex".to_string(),
                icon_path: None,
                score: 800,
                description: Some("e.g. #FF5733, #F53".to_string()),
                reason: None,
                thumb_b64: None,
                // Syntax example — fill "color " so the user types the value.
                fill: Some("color ".to_string()),
                ..Default::default()
            },
            CompletionItem {
                label: "rgb(r, g, b)".to_string(),
                icon_path: None,
                score: 700,
                description: Some("e.g. rgb(255, 87, 51)".to_string()),
                reason: None,
                thumb_b64: None,
                fill: Some("color ".to_string()),
                ..Default::default()
            },
            CompletionItem {
                label: "hsl(h, s%, l%)".to_string(),
                icon_path: None,
                score: 600,
                description: Some("e.g. hsl(11, 100%, 60%)".to_string()),
                reason: None,
                thumb_b64: None,
                fill: Some("color ".to_string()),
                ..Default::default()
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_6digit() {
        let rgb = parse_hex("#FF5733").unwrap();
        assert_eq!((rgb.r, rgb.g, rgb.b), (255, 87, 51));
    }

    #[test]
    fn parse_hex_no_hash() {
        let rgb = parse_hex("ff5733").unwrap();
        assert_eq!((rgb.r, rgb.g, rgb.b), (255, 87, 51));
    }

    #[test]
    fn parse_hex_shorthand() {
        let rgb = parse_hex("#F53").unwrap();
        assert_eq!((rgb.r, rgb.g, rgb.b), (255, 85, 51));
    }

    #[test]
    fn parse_hex_8digit_alpha() {
        let rgb = parse_hex("#FF5733FF").unwrap();
        assert_eq!((rgb.r, rgb.g, rgb.b), (255, 87, 51));
    }

    #[test]
    fn parse_rgb_function() {
        let rgb = parse_rgb_func("rgb(255, 87, 51)").unwrap();
        assert_eq!((rgb.r, rgb.g, rgb.b), (255, 87, 51));
    }

    #[test]
    fn parse_hsl_function() {
        let rgb = parse_hsl_func("hsl(0, 0%, 100%)").unwrap();
        assert_eq!((rgb.r, rgb.g, rgb.b), (255, 255, 255));

        let rgb = parse_hsl_func("hsl(0, 0%, 0%)").unwrap();
        assert_eq!((rgb.r, rgb.g, rgb.b), (0, 0, 0));

        let rgb = parse_hsl_func("hsl(0, 100%, 50%)").unwrap();
        assert_eq!((rgb.r, rgb.g, rgb.b), (255, 0, 0));
    }

    #[test]
    fn hsl_rgb_roundtrip() {
        // Test that converting RGB → HSL → RGB gives back the same values (within rounding)
        let original = Rgb {
            r: 100,
            g: 150,
            b: 200,
        };
        let (h, s, l) = rgb_to_hsl(original.r, original.g, original.b);
        let back = hsl_to_rgb(h as f64, s as f64, l as f64);
        // Allow ±2 for rounding
        assert!(
            (original.r as i16 - back.r as i16).unsigned_abs() <= 2,
            "r mismatch: {} vs {}",
            original.r,
            back.r
        );
        assert!(
            (original.g as i16 - back.g as i16).unsigned_abs() <= 2,
            "g mismatch: {} vs {}",
            original.g,
            back.g
        );
        assert!(
            (original.b as i16 - back.b as i16).unsigned_abs() <= 2,
            "b mismatch: {} vs {}",
            original.b,
            back.b
        );
    }

    #[test]
    fn nearest_tailwind_exact() {
        let red500 = Rgb {
            r: 239,
            g: 68,
            b: 68,
        };
        let (name, dist) = nearest_tailwind(red500);
        assert_eq!(name, "red-500");
        assert_eq!(dist, 0);
    }

    #[test]
    fn nearest_tailwind_approximate() {
        // Pure red should be close to red-500 or red-600
        let (name, _) = nearest_tailwind(Rgb { r: 255, g: 0, b: 0 });
        assert!(name.starts_with("red-"), "expected red-*, got {name}");
    }

    #[test]
    fn invalid_input() {
        assert!(parse_color("not a color").is_none());
        assert!(parse_color("#GGG").is_none());
        assert!(parse_color("rgb(300, 0, 0)").is_none()); // 300 > u8::MAX
    }

    #[test]
    fn color_args_flatten_from_structured_json() {
        // The grammar's flat rendering must be exactly what the parser
        // accepts: color values via `parse_color`, and the literal "picker"
        // subcommand `execute` matches case-insensitively.
        let flat = COLOR_GRAMMAR
            .flatten_json(r##"{"value":"#FF5733"}"##)
            .unwrap();
        assert_eq!(flat, "#FF5733");
        assert!(parse_color(&flat).is_some());

        let flat = COLOR_GRAMMAR
            .flatten_json(r#"{"value":"rgb(255, 87, 51)"}"#)
            .unwrap();
        assert!(parse_color(&flat).is_some());

        let flat = COLOR_GRAMMAR
            .flatten_json(r#"{"value":"hsl(11, 100%, 60%)"}"#)
            .unwrap();
        assert!(parse_color(&flat).is_some());

        let flat = COLOR_GRAMMAR.flatten_json(r#"{"value":"picker"}"#).unwrap();
        assert!(flat.eq_ignore_ascii_case("picker"));

        // Flat/legacy callers pass through untouched (caller keeps raw).
        assert_eq!(COLOR_GRAMMAR.flatten_json("#FF5733"), None);
    }

    #[test]
    fn hex_output_lowercase() {
        let rgb = Rgb {
            r: 255,
            g: 87,
            b: 51,
        };
        assert_eq!(rgb.to_hex(), "#ff5733");
    }
}
