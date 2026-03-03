//! Desktop application index — shared infrastructure for app lookup.
//!
//! Used by:
//! - `action_registry::handlers::app_launcher` (execute + completions)
//! - `intent::mod` (Phase 3 fallback routing)
//!
//! The index is built once at warmup and lives for the process lifetime.

pub mod entry;
pub mod index;
pub mod parse;

pub use entry::DesktopEntry;
pub use index::{AUTO_LAUNCH_THRESHOLD, AppIndex, CANDIDATE_THRESHOLD, app_index};
pub use parse::discover_entries;
