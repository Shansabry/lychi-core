//! The trigger tables — pure data, shared by the intent router and the
//! action-registry's Guide catalog.
//!
//! # Why a leaf module
//!
//! These are just `(prefix, meaning)` pairs. They used to live in
//! `intent::patterns`, but `action_registry::registry::trigger_catalog` also
//! reads them — an `action_registry → intent` import that REVERSES the brick
//! graph's one-way `intent → action_registry` edge. Two bricks reaching for the
//! same data is fine; the fix is to put that data in a LEAF module (no
//! dependencies of its own) that both may read, rather than have one brick reach
//! up into the other. That also keeps the door open for plugin-supplied triggers
//! without re-fighting the layering.

/// Shorthand colon-prefix triggers → handler id. Longest match wins (multi-char
/// prefixes are checked before single-char). This is the single source of truth
/// for both routing (see `intent::patterns::route`) and the dynamic Guide's
/// Triggers list, so the two never drift.
pub static COLON_TRIGGERS: &[(&str, &str)] = &[
    ("bm:", "bm"),
    ("cl:", "clip"),
    ("sym:", "sym"),
    ("sys:", "system"),
    ("si:", "sysinfo"),
    ("yt:", "yt"),
    ("e:", "emoji"),
    ("u:", "unicode"),
    ("w:", "web"),
    ("r:", "run"),
    ("c:", "calc"),
    ("f:", "file"),
    ("o:", "open"),
    ("n:", "note"),
    ("m:", "media"),
    ("p:", "project"),
    ("tz:", "time"),
    ("al:", "alias"),
    ("sn:", "snip"),
    ("tm:", "timer"),
    ("rm:", "reminder"),
];

/// The structural (character-sigil) triggers — input shapes that route without a
/// keyword. These are truly structural (implemented in `intent::patterns`), so
/// they're a small fixed set paired here with a human description for the Guide.
pub static SIGIL_TRIGGERS: &[(&str, &str)] = &[
    ("=", "Evaluate a math or unit/currency expression"),
    (">", "Run a shell command"),
    ("/", "Fuzzy-search files"),
    ("~/", "Open a path from home"),
    ("@", "Reference a file inside a command"),
    ("#hex", "Preview a hex color"),
    ("example.com", "Open a URL"),
];
