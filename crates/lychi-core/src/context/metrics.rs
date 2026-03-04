//! Lightweight atomic counters for context system observability.
//!
//! All counters are process-lifetime totals — they reset on restart.
//! Read via the `ctx` debug command (`ctx metrics`) or tracing logs.
//!
//! Instrumented events:
//! - soft_stale_hit:                context was soft-stale when completions were requested
//! - hard_stale_hit:                context was hard-stale (>5min) when completions/routing ran
//! - stale_refresh_triggered:       async re-gather was kicked off due to staleness
//! - terminal_incoherent_filtered:  terminal_cwd was excluded because it's in a different project
//! - clipboard_expansion_used:      resolve_with_clipboard() successfully expanded a verb
//! - clipboard_expansion_miss_empty:  supported verb seen but clipboard was empty
//! - clipboard_expansion_miss_type:   supported verb seen but clipboard type didn't match
//! - terminal_probe_hit:              terminal probe returned a CWD (native API success)
//! - terminal_route_hit:               command routed to existing terminal
//! - terminal_route_busy:              routing skipped — terminal has foreground process
//! - terminal_route_fail:              routing protocol send failed
//! - terminal_route_no_protocol:       routing skipped — unsupported terminal emulator

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static SOFT_STALE_HIT: AtomicU64 = AtomicU64::new(0);
static HARD_STALE_HIT: AtomicU64 = AtomicU64::new(0);
static STALE_REFRESH_TRIGGERED: AtomicU64 = AtomicU64::new(0);
static TERMINAL_INCOHERENT_FILTERED: AtomicU64 = AtomicU64::new(0);
static CLIPBOARD_EXPANSION_USED: AtomicU64 = AtomicU64::new(0);
/// Supported verb seen but clipboard was empty (discoverability gap).
static CLIPBOARD_EXPANSION_MISS_EMPTY: AtomicU64 = AtomicU64::new(0);
/// Supported verb seen but clipboard content type didn't match (type coverage gap).
static CLIPBOARD_EXPANSION_MISS_TYPE: AtomicU64 = AtomicU64::new(0);
/// Terminal probe returned a CWD (measures value, not attempts).
static TERMINAL_PROBE_HIT: AtomicU64 = AtomicU64::new(0);
/// Command successfully routed to an existing terminal.
static TERMINAL_ROUTE_HIT: AtomicU64 = AtomicU64::new(0);
/// Routing skipped because terminal was busy (foreground process running).
static TERMINAL_ROUTE_BUSY: AtomicU64 = AtomicU64::new(0);
/// Routing attempted but protocol send failed.
static TERMINAL_ROUTE_FAIL: AtomicU64 = AtomicU64::new(0);
/// Routing skipped because terminal has no send protocol (unsupported emulator).
static TERMINAL_ROUTE_NO_PROTOCOL: AtomicU64 = AtomicU64::new(0);

/// Baseline snapshot for `--rate` delta computation.
/// Set by `reset_baseline()`; `None` until first reset.
static BASELINE: Mutex<Option<(ContextMetrics, Instant)>> = Mutex::new(None);

pub fn inc_soft_stale_hit() {
    SOFT_STALE_HIT.fetch_add(1, Ordering::Relaxed);
}

pub fn inc_hard_stale_hit() {
    HARD_STALE_HIT.fetch_add(1, Ordering::Relaxed);
}

pub fn inc_stale_refresh_triggered() {
    STALE_REFRESH_TRIGGERED.fetch_add(1, Ordering::Relaxed);
}

pub fn inc_terminal_incoherent_filtered() {
    TERMINAL_INCOHERENT_FILTERED.fetch_add(1, Ordering::Relaxed);
}

pub fn inc_clipboard_expansion_used() {
    CLIPBOARD_EXPANSION_USED.fetch_add(1, Ordering::Relaxed);
}

/// Clipboard was empty when a supported verb was seen.
pub fn inc_clipboard_expansion_miss_empty() {
    CLIPBOARD_EXPANSION_MISS_EMPTY.fetch_add(1, Ordering::Relaxed);
}

/// Clipboard had content but its type didn't match the verb.
pub fn inc_clipboard_expansion_miss_type() {
    CLIPBOARD_EXPANSION_MISS_TYPE.fetch_add(1, Ordering::Relaxed);
}

/// Terminal probe returned a CWD.
pub fn inc_terminal_probe_hit() {
    TERMINAL_PROBE_HIT.fetch_add(1, Ordering::Relaxed);
}

/// Command successfully routed to an existing terminal.
pub fn inc_terminal_route_hit() {
    TERMINAL_ROUTE_HIT.fetch_add(1, Ordering::Relaxed);
}

/// Routing skipped because terminal was busy.
pub fn inc_terminal_route_busy() {
    TERMINAL_ROUTE_BUSY.fetch_add(1, Ordering::Relaxed);
}

/// Routing protocol send failed.
pub fn inc_terminal_route_fail() {
    TERMINAL_ROUTE_FAIL.fetch_add(1, Ordering::Relaxed);
}

/// Routing skipped — terminal emulator has no send protocol.
pub fn inc_terminal_route_no_protocol() {
    TERMINAL_ROUTE_NO_PROTOCOL.fetch_add(1, Ordering::Relaxed);
}

/// Snapshot of all counters. Returned as a flat struct for easy serialization/display.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextMetrics {
    pub soft_stale_hit: u64,
    pub hard_stale_hit: u64,
    pub stale_refresh_triggered: u64,
    pub terminal_incoherent_filtered: u64,
    pub clipboard_expansion_used: u64,
    pub clipboard_expansion_miss_empty: u64,
    pub clipboard_expansion_miss_type: u64,
    pub terminal_probe_hit: u64,
    pub terminal_route_hit: u64,
    pub terminal_route_busy: u64,
    pub terminal_route_fail: u64,
    pub terminal_route_no_protocol: u64,
}

impl ContextMetrics {
    /// Total clipboard expansion misses (empty + type mismatch).
    pub fn clipboard_expansion_miss(&self) -> u64 {
        self.clipboard_expansion_miss_empty + self.clipboard_expansion_miss_type
    }

    /// Compute element-wise delta: `self - baseline` (saturating).
    pub fn delta(&self, baseline: &ContextMetrics) -> ContextMetrics {
        ContextMetrics {
            soft_stale_hit: self.soft_stale_hit.saturating_sub(baseline.soft_stale_hit),
            hard_stale_hit: self.hard_stale_hit.saturating_sub(baseline.hard_stale_hit),
            stale_refresh_triggered: self
                .stale_refresh_triggered
                .saturating_sub(baseline.stale_refresh_triggered),
            terminal_incoherent_filtered: self
                .terminal_incoherent_filtered
                .saturating_sub(baseline.terminal_incoherent_filtered),
            clipboard_expansion_used: self
                .clipboard_expansion_used
                .saturating_sub(baseline.clipboard_expansion_used),
            clipboard_expansion_miss_empty: self
                .clipboard_expansion_miss_empty
                .saturating_sub(baseline.clipboard_expansion_miss_empty),
            clipboard_expansion_miss_type: self
                .clipboard_expansion_miss_type
                .saturating_sub(baseline.clipboard_expansion_miss_type),
            terminal_probe_hit: self
                .terminal_probe_hit
                .saturating_sub(baseline.terminal_probe_hit),
            terminal_route_hit: self
                .terminal_route_hit
                .saturating_sub(baseline.terminal_route_hit),
            terminal_route_busy: self
                .terminal_route_busy
                .saturating_sub(baseline.terminal_route_busy),
            terminal_route_fail: self
                .terminal_route_fail
                .saturating_sub(baseline.terminal_route_fail),
            terminal_route_no_protocol: self
                .terminal_route_no_protocol
                .saturating_sub(baseline.terminal_route_no_protocol),
        }
    }
}

pub fn snapshot() -> ContextMetrics {
    ContextMetrics {
        soft_stale_hit: SOFT_STALE_HIT.load(Ordering::Relaxed),
        hard_stale_hit: HARD_STALE_HIT.load(Ordering::Relaxed),
        stale_refresh_triggered: STALE_REFRESH_TRIGGERED.load(Ordering::Relaxed),
        terminal_incoherent_filtered: TERMINAL_INCOHERENT_FILTERED.load(Ordering::Relaxed),
        clipboard_expansion_used: CLIPBOARD_EXPANSION_USED.load(Ordering::Relaxed),
        clipboard_expansion_miss_empty: CLIPBOARD_EXPANSION_MISS_EMPTY.load(Ordering::Relaxed),
        clipboard_expansion_miss_type: CLIPBOARD_EXPANSION_MISS_TYPE.load(Ordering::Relaxed),
        terminal_probe_hit: TERMINAL_PROBE_HIT.load(Ordering::Relaxed),
        terminal_route_hit: TERMINAL_ROUTE_HIT.load(Ordering::Relaxed),
        terminal_route_busy: TERMINAL_ROUTE_BUSY.load(Ordering::Relaxed),
        terminal_route_fail: TERMINAL_ROUTE_FAIL.load(Ordering::Relaxed),
        terminal_route_no_protocol: TERMINAL_ROUTE_NO_PROTOCOL.load(Ordering::Relaxed),
    }
}

/// Record the current counter values as a baseline for `--rate` delta reporting.
pub fn reset_baseline() {
    if let Ok(mut guard) = BASELINE.lock() {
        *guard = Some((snapshot(), Instant::now()));
    }
}

/// Return `(delta, elapsed_secs, baseline_at)` since the last `reset_baseline()` call.
/// Returns `None` if no baseline has been set yet.
pub fn rate_since_baseline() -> Option<(ContextMetrics, f64, Instant)> {
    let guard = BASELINE.lock().ok()?;
    let (baseline, at) = (*guard).as_ref()?;
    let elapsed = at.elapsed().as_secs_f64();
    let current = snapshot();
    Some((current.delta(baseline), elapsed, *at))
}
