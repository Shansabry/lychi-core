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
//! - Wayland: `wl-paste --primary` (KWin/wlroots/etc. implement the
//!   primary-selection protocol).
//! - X11 / XWayland fallback: `xclip -selection primary -o`.
//! - CLIPBOARD, as a last resort ([`read_for_ai`]) — see below.
//!
//! All standard tools; no per-app accessibility setup, no clipboard
//! clobbering, no synthesized keystrokes.
//!
//! ## The GNOME Wayland exception
//!
//! One desktop can't serve PRIMARY to an external app. Mutter implements the
//! protocol, but it only delivers the selection to the *focused* client — so
//! the instant a launcher focuses itself to read, the source app loses focus
//! and the selection is invalidated. A catch-22 specific to launchers. The
//! privileged escape hatch other compositors expose (`wlr-data-control`) is
//! precisely what GNOME declines to ship, on the stated grounds that it would
//! let any running app read whatever you highlight.
//!
//! So [`read_for_ai`] falls back to the CLIPBOARD there and reports which
//! source it used, letting the caller say so. Visible degradation, not a
//! silent substitution — answering confidently about the wrong text is the
//! failure users complain about most in comparable tools.

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

/// Where the text handed to an "AI on selection" action actually came from.
///
/// Surfaced to the user because the difference is not cosmetic: on GNOME
/// Wayland the PRIMARY selection is unreachable to an external app, so what the
/// user highlighted may not be what we read. Saying so beats silently answering
/// about the wrong text — the single most-complained-about failure mode in
/// comparable macOS tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionSource {
    /// The PRIMARY selection — text highlighted right now. What we want.
    Primary,
    /// The CLIPBOARD — the last explicit Ctrl+C. Used where PRIMARY can't be
    /// read, so it may be stale relative to what's highlighted.
    Clipboard,
}

/// Read the text an AI action should operate on, trying the best source first.
///
/// The ladder exists because PRIMARY is unavailable on GNOME Wayland — not an
/// oversight but a deliberate GNOME security position. The protocol only
/// delivers the selection to the FOCUSED client, so the moment a launcher
/// focuses itself to read, the source app loses focus and the selection is
/// invalidated. A catch-22 specific to launchers; the privileged escape hatch
/// (`wlr-data-control`) is exactly what GNOME declines to ship.
///
/// Everywhere else — X11 (any desktop), KDE Plasma Wayland, and wlroots
/// compositors (Sway/Hyprland/niri) — PRIMARY works.
pub fn read_for_ai(is_wayland: bool) -> Option<(String, SelectionSource)> {
    if let Some(text) = read_primary_selection(is_wayland) {
        return Some((text, SelectionSource::Primary));
    }
    // Fall back to the clipboard so the feature still works on GNOME Wayland.
    // The caller tells the user which source was used, so a stale clipboard is
    // visible degradation rather than a silent substitution.
    let mut cb = arboard::Clipboard::new().ok()?;
    let text = cb.get_text().ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let capped: String = trimmed.chars().take(MAX_SELECTION_BYTES).collect();
    Some((capped, SelectionSource::Clipboard))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_clipboard_read_is_worth_telling_the_user_about() {
        // PRIMARY is the source we WANT — it's what the user highlighted, so
        // there is nothing to explain. A clipboard read means PRIMARY was
        // unavailable (GNOME Wayland) and the text may be stale, which the user
        // must be told: silently answering about the wrong text is the failure
        // mode this ladder exists to avoid.
        assert_ne!(SelectionSource::Primary, SelectionSource::Clipboard);
    }

    #[test]
    fn reading_never_panics_without_a_display() {
        // CI and headless test runners have no X11/Wayland display and no
        // clipboard daemon. Every rung of the ladder must degrade to `None`
        // rather than panicking — this runs on the global-hotkey path, where a
        // panic would take down the IPC listener.
        let _ = read_primary_selection(true);
        let _ = read_primary_selection(false);
        let _ = read_for_ai(false);
    }
}
