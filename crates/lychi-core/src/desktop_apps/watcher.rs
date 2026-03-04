use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Guards against overlapping rebuilds: set true while a rebuild thread is running.
static REBUILDING: AtomicBool = AtomicBool::new(false);

/// Spawn the watcher thread. Call once from Tauri setup.
///
/// `shutdown` is shared with the main process; when set to `true` the watcher
/// loop exits cleanly.
pub fn start(shutdown: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("app-index-watcher".into())
        .spawn(move || run_watcher(shutdown))
        .expect("failed to spawn app-index-watcher");
}

fn run_watcher(shutdown: Arc<AtomicBool>) {
    let dirs = super::parse::watch_dirs();

    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
    let mut watcher = match RecommendedWatcher::new(tx, Config::default()) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("[app-index] watcher init failed: {e}");
            return;
        }
    };

    for dir in &dirs {
        if dir.exists()
            && let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive)
        {
            tracing::warn!("[app-index] failed to watch {}: {e}", dir.display());
        }
    }

    // Debounce: rebuild once after 2s quiet period.
    // Rebuild runs on a separate thread so this loop stays responsive
    // during a potentially slow AppIndex::build() call.
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
            Ok(Err(e)) => tracing::warn!("[app-index] watch error: {e}"),
            Err(_) => {} // timeout — fall through to debounce check
        }

        if pending && last_event.elapsed() >= Duration::from_secs(2) {
            pending = false;
            // At-most-one rebuild in flight at a time
            if !REBUILDING.swap(true, Ordering::AcqRel) {
                std::thread::Builder::new()
                    .name("app-index-rebuild".into())
                    .spawn(|| {
                        tracing::info!("[app-index] rebuilding after filesystem change");
                        super::index::rebuild_app_index();
                        REBUILDING.store(false, Ordering::Release);
                    })
                    .ok();
            }
        }
    }
}
