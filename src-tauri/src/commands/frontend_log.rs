//! Frontend errors, written into the same log file as everything else.
//!
//! Without this the two halves of the app report to different places: Rust goes
//! to `~/.local/share/lychi/logs`, while the webview's `console.error` goes to a
//! devtools console nobody has open. So a WebProcess that dies mid-keystroke
//! produces a log that looks *healthy* — warmups fine, IPC fine, no errors — and
//! the only symptom is a blank window.
//!
//! That is not hypothetical. A tester reported "it crashes when I type about
//! three characters", and the log he sent showed nothing but successful startup:
//! every line proving the backend was fine, and not one line about the thing
//! that broke. He diagnosed it himself — "the logs are only for the backend, the
//! thing that's crashing is the frontend" — which is the gap this closes.
//!
//! Deliberately small: a level, a message, and an optional stack. No structured
//! payload, because the point is that a user can paste one log file and have it
//! contain the failure.

use serde::Deserialize;

/// A log record originating in the webview.
#[derive(Debug, Deserialize, specta::Type)]
pub struct FrontendLog {
    /// `error` | `warn` | `info`. Anything else is logged at info.
    pub level: String,
    pub message: String,
    /// JS stack trace when the source was an exception.
    pub stack: Option<String>,
}

/// Record a frontend event in the backend log.
///
/// Never fails: a logging call that can error is a logging call that gets
/// wrapped in a try/catch and then quietly dropped.
#[tauri::command]
#[specta::specta]
pub fn log_frontend(entry: FrontendLog) {
    let FrontendLog {
        level,
        message,
        stack,
    } = entry;
    // Tagged `[ui]` AND stamped with `source = "frontend"`.
    //
    // Both, because they serve different readers. The `[ui]` prefix is what a
    // human scanning a pasted terminal log sees; `source` is a structured field
    // the JSON log layer emits, so `jq 'select(.source=="frontend")'` isolates
    // the webview's half of a 500-line file.
    //
    // Needed because the module path lies here: every one of these lines is
    // emitted from `lychi_app::commands::frontend_log`, which names the bridge
    // rather than where the failure happened. Without an explicit marker a
    // reader would attribute a webview crash to a Rust command.
    match level.as_str() {
        "error" => match stack {
            Some(s) => tracing::error!(source = "frontend", stack = %s, "[ui] {message}"),
            None => tracing::error!(source = "frontend", "[ui] {message}"),
        },
        "warn" => tracing::warn!(source = "frontend", "[ui] {message}"),
        _ => tracing::info!(source = "frontend", "[ui] {message}"),
    }
}
