//! User-pinned zero-state rows — the launcher's Favorites.
//!
//! A pin is a command the user chose to keep at the top of the empty prompt
//! (⌘K → "Pin to top"). Pins are the ownership half of the zero state: unlike
//! recents they never move, never decay, and never enter the CTR/suppression
//! learning loops — the user put them there, and only the user removes them.

pub mod store;

use serde::{Deserialize, Serialize};

/// A pin as the UI sees it, in display order.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct PinItem {
    /// The exact command Enter runs (original casing).
    pub run: String,
    /// Display text.
    pub label: String,
    /// Explicit order — lower shows first.
    pub position: u32,
    pub created_at: u64,
}

/// The canonical pin identity for a command: trimmed, inner whitespace
/// collapsed, lowercased. The table key — so is-pinned / unpin resolve from
/// any row's run string without a secondary index, and "open  Spotify" and
/// "Open Spotify" are the same pin.
pub fn normalize_run(run: &str) -> String {
    run.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
