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
    show(Toast::new(summary, body).timeout_ms(timeout_ms));
}

/// A notification to display. Built here rather than by each caller so the
/// thread-safety rule below cannot be bypassed by constructing a
/// `notify_rust::Notification` directly.
pub struct Toast {
    summary: String,
    body: String,
    icon: Option<String>,
    appname: Option<String>,
    timeout_ms: u32,
}

impl Toast {
    pub fn new(summary: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            body: body.into(),
            icon: None,
            appname: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
    /// An icon name, or an absolute image path to render as a thumbnail.
    pub fn icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }
    pub fn appname(mut self, name: impl Into<String>) -> Self {
        self.appname = Some(name.into());
        self
    }
    pub fn timeout_ms(mut self, ms: u32) -> Self {
        self.timeout_ms = ms;
        self
    }
}

/// Show a notification from ANY thread, including a tokio worker.
///
/// `notify_rust` talks to the notification daemon over D-Bus via `zbus`, whose
/// blocking API calls `zbus::block_on` internally. Calling that on a thread
/// that is already driving a tokio runtime panics with "Cannot start a runtime
/// from within a runtime" — which is exactly what bare `screenshot` did on
/// 2026-08-03: the capture succeeded, then the confirmation notification killed
/// the worker and the whole action reported failure.
///
/// The pre-existing callers happened to be safe by accident, not by design:
/// `timer_monitor_loop` is a plain `std::thread`, and reminders piggyback on
/// it. Only the screenshot handler is an async fn on a tokio worker, so it was
/// the only one that could hit this — and it did.
///
/// Dispatching onto a plain `std::thread` here makes the rule structural: no
/// caller has to know which kind of thread it is on, which is the only version
/// of this that stays fixed. Fire-and-forget: a notification must never delay
/// or fail the action that triggered it.
pub fn show(toast: Toast) {
    std::thread::spawn(move || {
        let mut n = notify_rust::Notification::new();
        n.summary(&toast.summary)
            .body(&toast.body)
            .timeout(notify_rust::Timeout::Milliseconds(toast.timeout_ms));
        if let Some(ref icon) = toast.icon {
            n.icon(icon);
        }
        if let Some(ref app) = toast.appname {
            n.appname(app);
        }
        if let Err(e) = n.show() {
            // Debug, not warn: no notification daemon is a normal configuration
            // on a minimal desktop, not something the user needs telling about.
            tracing::debug!("[notify] could not show notification: {e}");
        }
    });
}
