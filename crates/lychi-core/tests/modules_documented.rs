//! Doc-drift guard: the load-bearing modules stay documented in `core/CLAUDE.md`.
//!
//! `CLAUDE.md` is the onboarding contract of a public repo courting external PRs,
//! and the first thing every AI session reads. It had drifted badly (documented
//! JSON history when it's redb, listed ~11 of 36 modules) because prose can't
//! fail a build (ARCH-6). This test makes the important half checkable: if a
//! load-bearing module is added without a mention in CLAUDE.md, CI fails here.
//!
//! Scope is deliberately the LOAD-BEARING set, not literally every dir — a
//! trivial helper module shouldn't force a doc edit, and an over-strict check
//! that fails on every new leaf just gets `#[ignore]`d. The list below is the
//! subsystems a contributor must know exist to navigate the codebase. Adding a
//! genuinely significant module means adding it here AND to CLAUDE.md together —
//! that pairing is the point.

/// Modules a newcomer must be able to find from CLAUDE.md. Keep in sync with the
/// structure block there; a new brick-level subsystem belongs in both.
const LOAD_BEARING: &[&str] = &[
    "action_registry",
    "rules",
    "intent",
    "executor",
    "providers",
    "coordinator",
    "suggestions",
    "context",
    "file_search",
    "desktop_apps",
    "clipboard",
    "db",
    "config",
    "mpris",
];

#[test]
fn load_bearing_modules_are_named_in_claude_md() {
    // The crate root is `crates/lychi-core`; CLAUDE.md is two levels up in the
    // repo root. Read it directly (not include_str!) so a missing file fails
    // loudly here rather than at compile time in an unrelated place.
    let claude_md = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../CLAUDE.md")
        .canonicalize()
        .expect("core/CLAUDE.md should exist two dirs above the crate");
    let text = std::fs::read_to_string(&claude_md).expect("read CLAUDE.md");

    let missing: Vec<&str> = LOAD_BEARING
        .iter()
        .copied()
        .filter(|m| !text.contains(m))
        .collect();

    assert!(
        missing.is_empty(),
        "CLAUDE.md is missing load-bearing modules: {missing:?}\n\
         Add them to the structure block (and drop any that were removed from \
         the LOAD_BEARING list in this test)."
    );
}

/// Guard against the specific stale claim that motivated this: CLAUDE.md must
/// not tell contributors state lives in a JSON history file — it's redb now.
#[test]
fn claude_md_does_not_claim_json_history_persistence() {
    let claude_md = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../CLAUDE.md")
        .canonicalize()
        .expect("core/CLAUDE.md should exist");
    let text = std::fs::read_to_string(&claude_md).expect("read CLAUDE.md");
    assert!(
        !text.contains("history.json"),
        "CLAUDE.md still references history.json; state moved to lychi.redb"
    );
}
