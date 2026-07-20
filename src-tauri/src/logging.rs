//! Logging + crash observability.
//!
//! The production-standard `tracing` setup: two layers over one event stream —
//!   • a **JSON, non-blocking, daily-rotating file** log in the data dir, so a
//!     beta user can send a structured trace of a bad session (and it's ready to
//!     feed a log pipeline later); writes go through a dedicated worker thread so
//!     they never block the launcher's hot path.
//!   • a **pretty stderr** layer for reading during development.
//!
//! Both honour `RUST_LOG` (default `info`). Plus a panic hook that writes a crash
//! file before the process dies — otherwise a panic in an AppImage vanishes with
//! stderr and leaves nothing to debug.

use std::io::Write;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Directory where log + crash files live: `<data_dir>/logs/`.
fn log_dir() -> std::path::PathBuf {
    let dir = lychi_core::paths::data_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Initialise logging. Returns a `WorkerGuard` that MUST be held for the lifetime
/// of the program — dropping it flushes and stops the non-blocking file writer,
/// so buffered logs would be lost on exit. The caller keeps it alive (leak/store).
pub fn init() -> WorkerGuard {
    let dir = log_dir();

    // Daily-rotating file appender (lychi.log.YYYY-MM-DD), wrapped in a
    // non-blocking writer backed by a dedicated worker thread.
    let file_appender = tracing_appender::rolling::daily(&dir, "lychi.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    // Each layer gets its own env filter (they can't share one instance).
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // File: structured JSON (parseable, aggregatable, pipeline-ready).
    let file_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(file_writer)
        .with_ansi(false)
        .with_filter(filter());

    // Console: human-readable text for dev.
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(filter());

    tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .init();

    install_panic_hook();

    tracing::info!(
        "logging initialised (json file + stderr) at {}",
        dir.display()
    );
    guard
}

/// The launcher's own resource usage, read cheaply from `/proc/self`. Logged
/// periodically so a beta report of "it got sluggish / used too much RAM" has
/// data behind it.
#[derive(Debug, Default)]
struct ResourceSnapshot {
    /// Resident set size (physical memory) in KB.
    rss_kb: u64,
    /// Virtual memory size in KB.
    vsz_kb: u64,
    /// Number of OS threads in the process.
    threads: u64,
    /// Open file descriptors.
    open_fds: u64,
}

fn read_resources() -> ResourceSnapshot {
    let mut snap = ResourceSnapshot::default();
    // /proc/self/status has VmRSS, VmSize, Threads as "key:\tN kB" lines.
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            let val = || {
                line.split_whitespace()
                    .nth(1)
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0)
            };
            match line {
                _ if line.starts_with("VmRSS:") => snap.rss_kb = val(),
                _ if line.starts_with("VmSize:") => snap.vsz_kb = val(),
                _ if line.starts_with("Threads:") => snap.threads = val(),
                _ => {}
            }
        }
    }
    // Open fds = entries in /proc/self/fd.
    if let Ok(fds) = std::fs::read_dir("/proc/self/fd") {
        snap.open_fds = fds.count() as u64;
    }
    snap
}

/// Spawn a low-priority thread that logs a health snapshot (resources + context
/// metrics) every `interval`. Cheap (~a few /proc reads) and off the hot path.
/// The returned thread runs for the process lifetime.
pub fn spawn_health_monitor(interval: std::time::Duration) {
    std::thread::Builder::new()
        .name("lychi-health".into())
        .spawn(move || {
            // A short initial delay so the first snapshot reflects a warmed
            // process, not the launch spike.
            std::thread::sleep(std::time::Duration::from_secs(30));
            loop {
                let r = read_resources();
                let m = lychi_core::context::metrics::snapshot();
                tracing::info!(
                    rss_mb = r.rss_kb / 1024,
                    vsz_mb = r.vsz_kb / 1024,
                    threads = r.threads,
                    open_fds = r.open_fds,
                    stale_hits = m.soft_stale_hit + m.hard_stale_hit,
                    ctx_refreshes = m.stale_refresh_triggered,
                    "[health] resource snapshot"
                );
                std::thread::sleep(interval);
            }
        })
        .expect("failed to spawn health monitor thread");
}

/// Write a crash file on panic (in addition to the default hook, which still logs
/// to stderr/the file layer via the panic message). A launcher that panics in a
/// bundled AppImage otherwise dies silently — this leaves a trail the user can
/// send: panic message, location, thread, and a backtrace.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        let thread = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();
        let backtrace = std::backtrace::Backtrace::force_capture();

        // Log it (goes to both sinks) …
        tracing::error!(
            panic.location = %location,
            panic.thread = %thread,
            panic.message = %message,
            "PANIC"
        );

        // … and write a standalone crash file the user can attach to a report.
        let crash_path = log_dir().join("last-crash.log");
        if let Ok(mut f) = std::fs::File::create(&crash_path) {
            let _ = writeln!(
                f,
                "Lychi crash\nversion: {}\nthread: {thread}\nlocation: {location}\nmessage: {message}\n\nbacktrace:\n{backtrace}",
                env!("CARGO_PKG_VERSION"),
            );
        }

        // Preserve default behaviour (stderr print) too.
        default_hook(info);
    }));
}
