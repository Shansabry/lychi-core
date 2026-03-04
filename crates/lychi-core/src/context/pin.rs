//! Pinned workspace — global runtime override for IDE workspace detection.
//!
//! When set, `gather()` uses the pinned path instead of auto-detecting from
//! the active window title. Seeded from `config.toml` at startup, can also
//! be set/cleared at runtime via the `pin workspace` action handler.

use std::sync::Mutex;

static PINNED: Mutex<Option<String>> = Mutex::new(None);

/// Set the pinned workspace path. Pass `None` to clear.
pub fn set(path: Option<String>) {
    *PINNED.lock().unwrap() = path;
}

/// Get the current pinned workspace path, if any.
pub fn get() -> Option<String> {
    PINNED.lock().unwrap().clone()
}
