//! Desktop notifications — the one place Lychi speaks to the user when the
//! launcher window isn't the right surface.
//!
//! Used by the background paths: a reminder firing, a timer finishing, a
//! screenshot saved, or a global hotkey that found nothing to act on. In every
//! case the user is looking at another application, so an in-launcher message
//! would go unseen.
//!
//! Best-effort by design: a missing or broken notification daemon must never
//! fail the action that triggered it.

/// How long a transient notification stays on screen.
const DEFAULT_TIMEOUT_MS: u32 = 4000;

/// Show a desktop notification. Silently does nothing if no daemon is running.
pub fn toast(summary: &str, body: &str) {
    toast_with_timeout(summary, body, DEFAULT_TIMEOUT_MS);
}

/// [`toast`] with an explicit dismissal timeout, for messages that need longer
/// on screen (a fired reminder) or shorter (a quick confirmation).
pub fn toast_with_timeout(summary: &str, body: &str, timeout_ms: u32) {
    if let Err(e) = notify_rust::Notification::new()
        .summary(summary)
        .body(body)
        .timeout(notify_rust::Timeout::Milliseconds(timeout_ms))
        .show()
    {
        // Debug, not warn: no notification daemon is a normal configuration on
        // a minimal desktop, not something the user needs telling about.
        tracing::debug!("[notify] could not show notification: {e}");
    }
}
