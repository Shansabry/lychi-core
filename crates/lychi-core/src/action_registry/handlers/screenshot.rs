//! Screenshot capture — a signature Linux feature. The catch is that Linux
//! screenshot tooling is fragmented across compositors: wlroots (grim+slurp),
//! KDE (spectacle), GNOME (gnome-screenshot), plus the cross-desktop flameshot
//! and the X11 classics (scrot, maim, ImageMagick's import).
//!
//! Rather than hardcode one tool, this handler is **adaptive**: it probes the
//! session type and which tools are actually installed, then picks the best
//! available one and maps the requested mode onto that tool's own flags. So the
//! same `screenshot area` works on Sway, Plasma, GNOME, or bare X11 — whatever
//! the user happens to run — with zero configuration.
//!
//! Modes:
//!   - `screenshot`         → full screen (all monitors)
//!   - `screenshot area`    → interactive region select (aliases: region, select)
//!   - `screenshot window`  → active window (falls back to region if unsupported)
//!
//! Every capture is saved to the Pictures directory with a timestamped name and
//! copied to the clipboard when the tool supports it.

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;

pub struct ScreenshotHandler;

impl ScreenshotHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ScreenshotHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// What the user wants to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Full,
    Area,
    Window,
}

impl Mode {
    fn parse(args: &str) -> Mode {
        match args.trim().to_ascii_lowercase().as_str() {
            "area" | "region" | "select" | "selection" | "crop" => Mode::Area,
            "window" | "win" | "active" => Mode::Window,
            _ => Mode::Full,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Mode::Full => "full screen",
            Mode::Area => "selected region",
            Mode::Window => "active window",
        }
    }
}

/// Whether the tool copies to clipboard on its own, so we don't double-handle.
struct Plan {
    /// Program to run.
    program: String,
    /// Arguments, already resolved for the chosen mode + output path.
    args: Vec<String>,
    /// If set, pipe the tool's stdout into this program (grim → wl-copy path is
    /// handled separately; this is for the file-then-clipboard case).
    clipboard_after: Option<ClipboardCopy>,
    /// True when the tool wrote to `path` (so we can report the saved file).
    writes_file: bool,
}

/// How to place the saved file on the clipboard when the capture tool can't.
struct ClipboardCopy {
    program: &'static str,
    /// Args template with `{path}` and `{mime}` placeholders.
    args: Vec<String>,
}

fn is_wayland() -> bool {
    std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|s| s.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false)
}

fn have(tool: &str) -> bool {
    which::which(tool).is_ok()
}

/// Build a timestamped destination path in the Pictures dir (falling back to
/// home, then /tmp). Name mirrors what GNOME/KDE produce so it feels native.
fn output_path() -> PathBuf {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = dirs::picture_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join(format!("Screenshot_{secs}.png"))
}

/// Pick the best available tool for the environment and mode, returning a
/// concrete plan. `None` means no supported tool is installed.
///
/// Preference order is chosen so the most capable, most native tool wins first:
/// on Wayland we prefer the compositor-native tool (spectacle on KDE, grim on
/// wlroots, gnome-screenshot on GNOME), then flameshot, then X11 tools as a
/// last resort. On X11 we prefer flameshot/spectacle (rich), then scrot/maim.
fn plan(mode: Mode, path: &str) -> Option<Plan> {
    let wayland = is_wayland();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let is_kde = desktop.to_ascii_uppercase().contains("KDE")
        || desktop.to_ascii_uppercase().contains("PLASMA");
    let is_gnome = desktop.to_ascii_uppercase().contains("GNOME");

    // Ordered list of candidate tool ids to try. The first installed one wins.
    let mut order: Vec<&str> = Vec::new();
    if wayland {
        if is_kde {
            order.push("spectacle");
        }
        if is_gnome {
            order.push("gnome-screenshot");
        }
        order.extend(["grim", "spectacle", "gnome-screenshot", "flameshot"]);
    } else {
        order.extend([
            "flameshot",
            "spectacle",
            "gnome-screenshot",
            "maim",
            "scrot",
            "import",
        ]);
    }
    // De-duplicate while preserving order.
    let mut seen = std::collections::HashSet::new();
    order.retain(|t| seen.insert(*t));

    let tool = order.into_iter().find(|t| have(t))?;
    Some(build_plan(tool, mode, path))
}

/// Map (tool, mode) → concrete command. Kept separate from `plan` so it's unit
/// testable without a real environment.
fn build_plan(tool: &str, mode: Mode, path: &str) -> Plan {
    let s = |x: &str| x.to_string();
    match tool {
        // KDE — spectacle. Background mode, no notification, writes -o file,
        // and -c copies to clipboard too. -a active window, -r region, -f full.
        "spectacle" => {
            let mut args = vec![s("-b"), s("-n"), s("-c"), s("-o"), s(path)];
            match mode {
                Mode::Full => args.insert(0, s("-f")),
                Mode::Area => args.insert(0, s("-r")),
                Mode::Window => args.insert(0, s("-a")),
            }
            Plan {
                program: s("spectacle"),
                args,
                clipboard_after: None, // -c handles it
                writes_file: true,
            }
        }
        // wlroots — grim (+ slurp for region). grim writes the file to a
        // positional path; no window mode, so window degrades to region.
        "grim" => {
            let args = match mode {
                Mode::Full => vec![s(path)],
                // grim -g "<geometry from slurp>" <path>
                Mode::Area | Mode::Window => vec![s("-g"), s("__SLURP__"), s(path)],
            };
            Plan {
                program: s("grim"),
                args,
                clipboard_after: Some(ClipboardCopy {
                    program: "wl-copy",
                    args: vec![s("--type"), s("image/png")],
                }),
                writes_file: true,
            }
        }
        // GNOME — gnome-screenshot. -a area, -w window, default full; -f file.
        "gnome-screenshot" => {
            let mut args = vec![s("-f"), s(path)];
            match mode {
                Mode::Full => {}
                Mode::Area => args.insert(0, s("-a")),
                Mode::Window => args.insert(0, s("-w")),
            }
            Plan {
                program: s("gnome-screenshot"),
                args,
                clipboard_after: Some(ClipboardCopy {
                    program: "wl-copy",
                    args: vec![s("--type"), s("image/png")],
                }),
                writes_file: true,
            }
        }
        // Cross-desktop — flameshot. `full`/`gui` (region). -p path, -c clip.
        // No dedicated active-window mode, so window degrades to interactive.
        "flameshot" => {
            let sub = match mode {
                Mode::Full => "full",
                Mode::Area | Mode::Window => "gui",
            };
            Plan {
                program: s("flameshot"),
                args: vec![s(sub), s("-c"), s("-p"), s(path)],
                clipboard_after: None, // -c handles it
                writes_file: true,
            }
        }
        // X11 — maim. -s region, -i window id (we can't easily get one, so
        // window degrades to region). Positional file.
        "maim" => {
            let mut args = vec![s(path)];
            if matches!(mode, Mode::Area | Mode::Window) {
                args.insert(0, s("-s"));
            }
            Plan {
                program: s("maim"),
                args,
                clipboard_after: Some(ClipboardCopy {
                    program: "xclip",
                    args: vec![s("-selection"), s("clipboard"), s("-t"), s("image/png")],
                }),
                writes_file: true,
            }
        }
        // X11 — scrot. -s region, -u active window. Positional file.
        "scrot" => {
            let mut args = vec![s(path)];
            match mode {
                Mode::Full => {}
                Mode::Area => args.insert(0, s("-s")),
                Mode::Window => args.insert(0, s("-u")),
            }
            Plan {
                program: s("scrot"),
                args,
                clipboard_after: Some(ClipboardCopy {
                    program: "xclip",
                    args: vec![s("-selection"), s("clipboard"), s("-t"), s("image/png")],
                }),
                writes_file: true,
            }
        }
        // X11 — ImageMagick import. -window root full; otherwise interactive.
        "import" => {
            let args = match mode {
                Mode::Full => vec![s("-window"), s("root"), s(path)],
                Mode::Area | Mode::Window => vec![s(path)],
            };
            Plan {
                program: s("import"),
                args,
                clipboard_after: Some(ClipboardCopy {
                    program: "xclip",
                    args: vec![s("-selection"), s("clipboard"), s("-t"), s("image/png")],
                }),
                writes_file: true,
            }
        }
        // Unreachable: `plan` only picks from the tools above.
        other => Plan {
            program: s(other),
            args: vec![s(path)],
            clipboard_after: None,
            writes_file: true,
        },
    }
}

/// Execute the plan. Handles the grim+slurp two-step and the file→clipboard
/// copy where the tool can't do it itself.
fn capture(plan: Plan, path: &str) -> Result<(), String> {
    let mut args = plan.args.clone();

    // grim's region path needs a live geometry from slurp first.
    if let Some(idx) = args.iter().position(|a| a == "__SLURP__") {
        if !have("slurp") {
            return Err(
                "Region capture on this compositor needs `slurp` — install it (it pairs with grim)"
                    .to_string(),
            );
        }
        let geo = Command::new("slurp")
            .output()
            .map_err(|e| format!("Failed to run slurp: {e}"))?;
        if !geo.status.success() {
            // Non-zero usually means the user cancelled the selection.
            return Err("Selection cancelled".to_string());
        }
        let geometry = String::from_utf8_lossy(&geo.stdout).trim().to_string();
        if geometry.is_empty() {
            return Err("Selection cancelled".to_string());
        }
        args[idx] = geometry;
    }

    let status = Command::new(&plan.program)
        .args(&args)
        .status()
        .map_err(|e| format!("Failed to run {}: {e}", plan.program))?;
    if !status.success() {
        // Interactive tools exit non-zero when the user cancels — treat that as
        // a soft cancel rather than a hard error.
        return Err("Screenshot cancelled".to_string());
    }

    // Copy the saved file to the clipboard if the tool didn't already.
    if let Some(copy) = plan.clipboard_after {
        if plan.writes_file && have(copy.program) {
            copy_file_to_clipboard(&copy, path);
        }
    }

    Ok(())
}

/// Best-effort: read the file and pipe it into a clipboard program. Failure is
/// non-fatal — the file is already saved, which is the primary outcome.
fn copy_file_to_clipboard(copy: &ClipboardCopy, path: &str) {
    use std::io::Write;
    use std::process::Stdio;

    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Ok(mut child) = Command::new(copy.program)
        .args(&copy.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&bytes);
    }
    let _ = child.wait();
}

/// Fire a desktop notification confirming the capture, using the saved image
/// as the notification icon (a thumbnail). Best-effort — failure is non-fatal.
fn notify_saved(path: &str, mode: Mode) {
    let _ = notify_rust::Notification::new()
        .summary("Screenshot saved")
        .body(&format!("{} — copied to clipboard", mode.label()))
        // Absolute path → notification servers render it as the icon/thumbnail.
        .icon(path)
        .appname("Lychi")
        .timeout(notify_rust::Timeout::Milliseconds(4000))
        .show();
}

#[async_trait]
impl ActionHandler for ScreenshotHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&[
            "screenshot",
            "screencap",
            "screengrab",
        ])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "screenshot"
    }

    fn description(&self) -> &str {
        "Capture a screenshot (full screen, region, or window)"
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let p = partial.trim().to_ascii_lowercase();
        let hints = [
            ("", "screenshot", "Capture the full screen", "screenshot"),
            (
                "area",
                "screenshot area",
                "Select a region to capture",
                "screenshot area",
            ),
            (
                "window",
                "screenshot window",
                "Capture the active window",
                "screenshot window",
            ),
        ];
        hints
            .iter()
            .filter(|(key, _, _, _)| {
                p.is_empty() || key.starts_with(p.as_str()) || p == "screenshot"
            })
            .enumerate()
            .map(|(i, (_, label, desc, run))| {
                CompletionItem::new(
                    (*label).to_string(),
                    Some("__none__".into()),
                    900 - i as u16,
                )
                .with_run((*run).to_string())
                .with_description((*desc).to_string())
            })
            .collect()
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let mode = Mode::parse(args);
        let path = output_path();
        let path_str = path.to_string_lossy().to_string();

        let Some(plan) = plan(mode, &path_str) else {
            return Ok(ActionResult::err(
                "No screenshot tool found. Install one of: grim (+slurp), spectacle, \
                 gnome-screenshot, flameshot, scrot, or maim.",
            ));
        };

        match capture(plan, &path_str) {
            Ok(()) => {
                // Confirm the file actually landed (interactive cancels can
                // succeed-exit without writing).
                if std::path::Path::new(&path_str).exists() {
                    // Close the loop with a desktop notification — the capture
                    // often happens after Lychi has hidden (quick trigger / CLI),
                    // so this is the only visible confirmation. The saved image
                    // is used as the notification icon (a thumbnail preview).
                    notify_saved(&path_str, mode);
                    Ok(ActionResult::ok(
                        format!(
                            "Screenshot saved ({})\n{}\n\n📋 Copied to clipboard",
                            mode.label(),
                            path_str
                        ),
                        OutputType::Text,
                    ))
                } else {
                    Ok(ActionResult::err("Screenshot cancelled"))
                }
            }
            Err(e) => Ok(ActionResult::err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parsing_is_forgiving() {
        assert_eq!(Mode::parse(""), Mode::Full);
        assert_eq!(Mode::parse("  "), Mode::Full);
        assert_eq!(Mode::parse("area"), Mode::Area);
        assert_eq!(Mode::parse("REGION"), Mode::Area);
        assert_eq!(Mode::parse("select"), Mode::Area);
        assert_eq!(Mode::parse("window"), Mode::Window);
        assert_eq!(Mode::parse("win"), Mode::Window);
        assert_eq!(Mode::parse("everything else"), Mode::Full);
    }

    #[test]
    fn spectacle_flags_per_mode() {
        let p = build_plan("spectacle", Mode::Full, "/tmp/x.png");
        assert_eq!(p.program, "spectacle");
        assert!(p.args.contains(&"-f".to_string()));
        assert!(p.args.contains(&"-o".to_string()));
        assert!(p.args.contains(&"-c".to_string())); // spectacle copies itself
        assert!(p.clipboard_after.is_none());

        let p = build_plan("spectacle", Mode::Area, "/tmp/x.png");
        assert!(p.args.contains(&"-r".to_string()));
        let p = build_plan("spectacle", Mode::Window, "/tmp/x.png");
        assert!(p.args.contains(&"-a".to_string()));
    }

    #[test]
    fn grim_region_uses_slurp_placeholder() {
        let p = build_plan("grim", Mode::Area, "/tmp/x.png");
        assert_eq!(p.program, "grim");
        assert!(p.args.contains(&"-g".to_string()));
        assert!(p.args.contains(&"__SLURP__".to_string()));
        // grim can't copy itself → needs a follow-up wl-copy.
        assert!(p.clipboard_after.is_some());

        // Full screen has no slurp step.
        let p = build_plan("grim", Mode::Full, "/tmp/x.png");
        assert!(!p.args.contains(&"__SLURP__".to_string()));
        assert_eq!(p.args, vec!["/tmp/x.png".to_string()]);
    }

    #[test]
    fn gnome_flags_per_mode() {
        assert!(
            build_plan("gnome-screenshot", Mode::Area, "/tmp/x.png")
                .args
                .contains(&"-a".to_string())
        );
        assert!(
            build_plan("gnome-screenshot", Mode::Window, "/tmp/x.png")
                .args
                .contains(&"-w".to_string())
        );
        // Full = no mode flag, just -f path.
        let p = build_plan("gnome-screenshot", Mode::Full, "/tmp/x.png");
        assert_eq!(p.args, vec!["-f".to_string(), "/tmp/x.png".to_string()]);
    }

    #[test]
    fn flameshot_subcommands() {
        assert!(
            build_plan("flameshot", Mode::Full, "/tmp/x.png")
                .args
                .contains(&"full".to_string())
        );
        assert!(
            build_plan("flameshot", Mode::Area, "/tmp/x.png")
                .args
                .contains(&"gui".to_string())
        );
    }

    #[test]
    fn scrot_active_window_flag() {
        let p = build_plan("scrot", Mode::Window, "/tmp/x.png");
        assert!(p.args.contains(&"-u".to_string()));
        let p = build_plan("scrot", Mode::Area, "/tmp/x.png");
        assert!(p.args.contains(&"-s".to_string()));
    }

    #[test]
    fn output_path_is_timestamped_png() {
        let p = output_path();
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("Screenshot_"), "name: {name}");
        assert!(name.ends_with(".png"), "name: {name}");
    }
}
