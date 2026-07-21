//! Filesystem watcher for the Script Commands directory. On any add/remove/edit
//! it debounce-fires a caller-supplied callback (which re-scans + re-registers
//! the handler). Modeled on the desktop-apps watcher, but the rebuild action is
//! injected so this stays Tauri-free — the src-tauri layer supplies the closure
//! that takes the executor lock and re-registers.

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Spawn the scripts-dir watcher thread. Call once at startup.
///
/// - `dir`: the scripts directory to watch (created if missing).
/// - `shutdown`: shared flag; when set true the loop exits cleanly.
/// - `on_change`: invoked (on a spawned thread) after a debounced change — it
///   re-scans the dir and re-registers the handler + keywords.
pub fn start(
    dir: PathBuf,
    shutdown: Arc<AtomicBool>,
    on_change: Arc<dyn Fn() + Send + Sync>,
) {
    std::thread::Builder::new()
        .name("scripts-watcher".into())
        .spawn(move || run_watcher(dir, shutdown, on_change))
        .expect("failed to spawn scripts-watcher");
}

fn run_watcher(dir: PathBuf, shutdown: Arc<AtomicBool>, on_change: Arc<dyn Fn() + Send + Sync>) {
    // Ensure the dir exists so the watcher has something to watch and users have
    // a place to drop scripts.
    if !dir.exists() && let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("[scripts] could not create {}: {e}", dir.display());
    }

    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
    let mut watcher = match RecommendedWatcher::new(tx, Config::default()) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("[scripts] watcher init failed: {e}");
            return;
        }
    };
    if dir.exists()
        && let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive)
    {
        tracing::warn!("[scripts] failed to watch {}: {e}", dir.display());
    }

    // At-most-one rebuild in flight.
    let rebuilding = Arc::new(AtomicBool::new(false));
    let mut pending = false;
    let mut last_event = Instant::now();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Ok(_)) => {
                pending = true;
                last_event = Instant::now();
            }
            Ok(Err(e)) => tracing::warn!("[scripts] watch error: {e}"),
            Err(_) => {}
        }
        if pending && last_event.elapsed() >= Duration::from_secs(2) {
            pending = false;
            if !rebuilding.swap(true, Ordering::AcqRel) {
                let cb = on_change.clone();
                let flag = rebuilding.clone();
                std::thread::Builder::new()
                    .name("scripts-rebuild".into())
                    .spawn(move || {
                        tracing::info!("[scripts] reloading after filesystem change");
                        cb();
                        flag.store(false, Ordering::Release);
                    })
                    .ok();
            }
        }
    }
}
