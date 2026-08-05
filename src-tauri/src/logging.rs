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
    /// Peak RSS ever reached (`VmHWM`). RSS alone can't distinguish "never grew"
    /// from "grew and gave it back", and those need opposite fixes: a high peak
    /// with low current RSS is transient churn, while peak == current is memory
    /// genuinely still held.
    rss_peak_kb: u64,
    /// Anonymous resident memory (`RssAnon`) — the heap and allocator arenas, as
    /// opposed to file-backed pages (`RssFile`: the binary, mmapped libraries).
    ///
    /// This is the field that localizes a memory problem. In a debug build the
    /// binary alone contributes tens of MB of `RssFile` that release builds
    /// don't have, so bare RSS overstates the real cost. Growth that is ours to
    /// fix shows up here and nowhere else.
    rss_anon_kb: u64,
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
                _ if line.starts_with("VmHWM:") => snap.rss_peak_kb = val(),
                _ if line.starts_with("RssAnon:") => snap.rss_anon_kb = val(),
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

/// Periodic upkeep that has nowhere better to live: cheap, idempotent, and
/// tolerant of running late.
///
/// One tick thread rather than one per chore. Each of these could arm its own
/// timer, but then each owns a lifecycle to get wrong, and none of them needs
/// that precision — the whole point is that being a minute late costs nothing.
/// A task that needs to run *promptly*, or that can block, does not belong
/// here; give it its own thread.
/// How often [`Upkeep::tick`] runs, independent of the health-log interval.
///
/// Shorter than any task's own deadline so a task with an N-minute timeout
/// resolves within roughly N, not 2N.
const UPKEEP_TICK: std::time::Duration = std::time::Duration::from_secs(60);

pub trait Upkeep: Send + Sync {
    /// Short name for the log line when this reports doing something.
    fn name(&self) -> &'static str;
    /// Run one pass. Returns true if it actually did work, so the tick can say
    /// so — silent housekeeping is housekeeping nobody can debug.
    fn tick(&self) -> bool;
}

/// Spawn a low-priority thread that logs a health snapshot (resources + context
/// metrics) every `interval` and runs each registered [`Upkeep`] task.
/// Cheap (~a few /proc reads) and off the hot path. Runs for the process
/// lifetime.
pub fn spawn_health_monitor(interval: std::time::Duration, upkeep: Vec<Box<dyn Upkeep>>) {
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
                    // Anonymous (heap/arena) vs peak: together these say whether
                    // a large RSS is our allocations, transient churn already
                    // returned, or just file-backed pages from the binary.
                    rss_anon_mb = r.rss_anon_kb / 1024,
                    rss_peak_mb = r.rss_peak_kb / 1024,
                    vsz_mb = r.vsz_kb / 1024,
                    threads = r.threads,
                    open_fds = r.open_fds,
                    // Indexed path count, so memory growth can be attributed to
                    // the index rather than guessed at. A flat count beside a
                    // rising rss_anon_mb means the leak is elsewhere.
                    indexed_paths = lychi_core::file_search::indexed_path_count(),
                    stale_hits = m.soft_stale_hit + m.hard_stale_hit,
                    ctx_refreshes = m.stale_refresh_triggered,
                    "[health] resource snapshot"
                );
                // Upkeep runs on a shorter cadence than the health log: a
                // 5-minute idle timeout checked every 5 minutes resolves in up
                // to 10, so the sleep is subdivided. Each pass is an atomic
                // load unless a task's deadline has actually passed.
                let mut slept = std::time::Duration::ZERO;
                while slept < interval {
                    let step = UPKEEP_TICK.min(interval - slept);
                    std::thread::sleep(step);
                    slept += step;
                    for task in &upkeep {
                        if task.tick() {
                            tracing::debug!(task = task.name(), "[upkeep] released resources");
                        }
                    }
                }
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
