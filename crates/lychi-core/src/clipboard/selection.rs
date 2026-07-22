//! Reading the PRIMARY selection — the text the user has *highlighted* in the
//! focused window, without needing them to copy it (Ctrl+C).
//!
//! On Linux, highlighting text puts it in the PRIMARY selection (the buffer
//! middle-click pastes), distinct from the CLIPBOARD (Ctrl+C). This lets an AI
//! command act on "what I have selected right now" — e.g. `summarize` with no
//! typed text summarizes the highlighted paragraph.
//!
//! Read-only and lazy: called only when an AI action needs input the user didn't
//! type. We never *watch* the selection (no per-keystroke cost, no continuously
//! reading what the user highlights).
//!
//! Mechanism, in order:
//!   - Wayland: `wl-paste --primary` (KWin/wlroots/etc. implement the
//!     primary-selection protocol).
//!   - X11 / XWayland fallback: `xclip -selection primary -o`.
//! Both are the standard tools; no per-app accessibility setup, no clipboard
//! clobbering, no synthesized keystrokes.

use std::process::Command;

/// Cap on how much selected text we ingest, so a "select all" in a huge file
/// can't blow up a prompt. Generous — a few pages of prose.
const MAX_SELECTION_BYTES: usize = 100_000;

/// Read the current PRIMARY selection (highlighted, not-yet-copied text).
/// Returns `None` if nothing is selected, the tools are missing, or the text is
/// blank. `is_wayland` picks the primary path; the other is tried as a fallback
/// regardless (XWayland apps expose their selection via X11 even on Wayland).
pub fn read_primary_selection(is_wayland: bool) -> Option<String> {
    // Preferred path first, then the other as a fallback.
    let attempts: [&str; 2] = if is_wayland {
        ["wl-paste", "xclip"]
    } else {
        ["xclip", "wl-paste"]
    };
    for tool in attempts {
        if let Some(text) = read_with(tool) {
            return Some(text);
        }
    }
    None
}

/// Run one selection-reading tool, returning trimmed non-empty text.
fn read_with(tool: &str) -> Option<String> {
    let output = match tool {
        "wl-paste" => Command::new("wl-paste")
            .args(["--primary", "--no-newline", "--type", "text/plain"])
            .output(),
        "xclip" => Command::new("xclip")
            .args(["-selection", "primary", "-o"])
            .output(),
        _ => return None,
    }
    .ok()?;

    if !output.status.success() {
        return None;
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.len() > MAX_SELECTION_BYTES {
        // Truncate on a char boundary.
        let mut end = MAX_SELECTION_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
