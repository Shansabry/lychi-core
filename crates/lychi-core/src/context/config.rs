//! Owned context configuration — the user-tunable knobs that steer context
//! detection (extra terminal/IDE window classes, extra project markers, the
//! pinned workspace).
//!
//! Previously these were five module-level `static`s written through four
//! separate fire-and-forget setters scattered across `active_window`, `ide`, and
//! `pin` — the exact "config pushed via scattered setters" anti-pattern the
//! architecture review flagged. Now they are one owned value with a single
//! `apply()` entry point, so a config change is one atomic call (the event-bus
//! `ConfigChanged` reactor calls `apply` instead of poking three modules).
//!
//! The values still *live* in module statics because they're read on the hot
//! context-detection path by pure functions deep in the call graph — passing a
//! borrowed config down to every one would be a large, low-value rewrite. The
//! win P4 targets is the unified, atomic *write* path, not per-instance storage
//! of a read-mostly, process-wide setting.

/// The user's context-detection configuration, applied atomically.
#[derive(Debug, Clone, Default)]
pub struct ContextConfig {
    /// Extra terminal WM classes (config.commands.extra_terminals).
    pub extra_terminals: Vec<String>,
    /// Extra IDE/editor WM classes (config.commands.extra_ides).
    pub extra_ides: Vec<String>,
    /// Extra strong project-root markers (config.projects.extra_strong_markers).
    pub extra_strong_markers: Vec<String>,
    /// Extra soft project-root markers (config.projects.extra_soft_markers).
    pub extra_soft_markers: Vec<String>,
    /// Pinned workspace path override (config.projects.pinned_workspace).
    pub pinned_workspace: Option<String>,
}

impl ContextConfig {
    /// Apply this configuration to the context detectors — a single atomic entry
    /// point replacing the scattered `register_extra_*` / `pin::set` calls.
    pub fn apply(&self) {
        super::active_window::register_extra_terminals(&self.extra_terminals);
        super::active_window::register_extra_ides(&self.extra_ides);
        super::ide::register_extra_markers(&self.extra_strong_markers, &self.extra_soft_markers);
        super::pin::set(self.pinned_workspace.clone());
    }
}
