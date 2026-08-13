//! The zero state — what the launcher shows on an empty prompt.
//!
//! An explicit sectioned composer, not a ranked pool. With nothing typed there
//! is no query to rank against; these rows are browsable offers, and what the
//! user needs from them is STABILITY — the same muscle-memory list on every
//! summon. Exactly two sections:
//!
//! 1. **Pins** — commands the user chose (⌘K → "Pin to top"). Always first,
//!    always all of them, in pin order. The ownership backbone: pins never
//!    decay, never reshuffle, never enter the CTR/suppression loops.
//! 2. **Recent apps** — frecency-ranked launches, filling the rest.
//!
//! Nothing else. Context-derived command rows (workspace memory, clipboard
//! actions) were tried here and removed by decision 2026-08-11: they vary
//! with cwd/clipboard, which reads as inconsistency, and the commands a user
//! actually wants at the top are exactly what pins express better. Context
//! actions still surface once the user TYPES a matching keyword
//! (`context::suggestions::typed_matches`), and the usage data keeps being
//! recorded — re-offering them here is a config flag away if ever wanted.
//!
//! This mirrors the empty states users already trust: Raycast (Favorites
//! first, then recents) and PowerToys Command Palette (pinned, then recents).

use std::sync::Arc;

use redb::Database;

use crate::action_registry::CompletionItem;
use crate::db::frecency;
use crate::pins::store::PinsStore;

/// Target row budget. Pins are never trimmed (the user chose them); recent
/// apps fill whatever room is left.
const TARGET_ROWS: usize = 8;

/// Recent apps may fill the whole budget — with pins the only other section,
/// there is nothing to reserve slots for.
const MAX_RECENT_APPS: usize = TARGET_ROWS;

/// Compose the empty-prompt list. Needs no environment context at all — the
/// first summon after launch runs before any context gather has landed, and
/// pins + recent apps must show regardless (a blank launcher on the first
/// open was one of the reported inconsistencies).
pub fn compose(
    db: &Arc<Database>,
    cfg: &crate::config::schema::SuggestionsConfig,
) -> Vec<CompletionItem> {
    let mut items: Vec<CompletionItem> = Vec::new();
    // Dedupe on the effective run string. Pins are inserted first, so a pinned
    // app also present in recents shows once — as the pin.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let push = |items: &mut Vec<CompletionItem>,
                seen: &mut std::collections::HashSet<String>,
                item: CompletionItem| {
        let key = crate::pins::normalize_run(item.run.as_deref().unwrap_or(&item.label));
        if seen.insert(key) {
            items.push(item);
        }
    };

    for pin in pin_rows(db) {
        push(&mut items, &mut seen, pin);
    }

    // Pins are never trimmed — the user chose exactly these. Apps fill the
    // remaining room up to the target.
    let budget = TARGET_ROWS.max(items.len());

    // Recents honour the config flag; pins do not — they are explicit user
    // configuration (like quicklinks), not inferred history.
    if cfg.zero_state_recents {
        for app in recent_apps(db) {
            if items.len() >= budget {
                break;
            }
            push(&mut items, &mut seen, app);
        }
    }

    // A brand-new user with nothing at all still gets one honest hint, never
    // a blank panel or a wall of speculative priors.
    if items.is_empty() {
        return vec![hint_item()];
    }
    items
}

/// The user's pins as rows, in pin order. An app pin gets the real app icon
/// (resolved lazily — at most `MAX_PINS` rows ever pay for one); anything else
/// carries the `__pinned__` glyph.
fn pin_rows(db: &Arc<Database>) -> Vec<CompletionItem> {
    let pins = PinsStore::new().list(db).unwrap_or_default();
    let index = crate::desktop_apps::app_index();
    pins.into_iter()
        .map(|pin| {
            let lowered = pin.run.to_lowercase();
            let app_icon = lowered
                .strip_prefix("open ")
                .map(str::trim)
                .and_then(|app| index.by_name_exact(app))
                .map(|entry| {
                    entry
                        .icon_path
                        .get_or_init(|| {
                            entry
                                .icon
                                .as_deref()
                                .and_then(crate::action_registry::handlers::icons::resolve_icon)
                        })
                        .clone()
                });
            CompletionItem {
                label: pin.label,
                icon_path: app_icon
                    .flatten()
                    .or_else(|| Some("__pinned__".to_string())),
                score: 0,
                reason: Some("Pinned".to_string()),
                run: Some(pin.run),
                pinned: true,
                ..Default::default()
            }
        })
        .collect()
}

/// Recently-used APPS, most-used first — the stable backbone of the list.
///
/// `app_launcher` records every launch under the app's lowercased display name
/// (`handlers/app_launcher.rs`), in a flat keyspace it shares with `history:`,
/// `win:`, `ws:`, `sug:` and bare file paths. There is no "this is an app"
/// marker, so the `AppIndex` lookup IS the test: a key that resolves to an
/// installed app is one, and a key that does not is skipped. An app since
/// uninstalled therefore disappears on its own.
///
/// The `run` is `open <Name>`, so Enter launches it — the same string
/// `AppLauncher::completions` emits, so selection behaves identically whether
/// the row came from here or from typing.
fn recent_apps(db: &Arc<Database>) -> Vec<CompletionItem> {
    let index = crate::desktop_apps::app_index();

    // get_scores already applies the circadian affinity multiplier AND rides
    // the generation-keyed entry cache.
    let mut scored: Vec<(&crate::desktop_apps::DesktopEntry, f64)> = frecency::get_scores(db)
        .into_iter()
        // Cheap pre-filter: everything namespaced (`history:`, `win:`, `ws:`)
        // or absolute is definitely not an app name. Correctness still rests
        // on the index lookup below; this only avoids probes.
        .filter(|(key, _)| !key.contains(':') && !key.starts_with('/'))
        .filter_map(|(key, score)| index.by_name_exact(&key).map(|e| (e, score)))
        .collect();

    // Score desc, then name for a deterministic order when scores tie.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.name.cmp(&b.0.name))
    });

    scored
        .into_iter()
        // TAKE BEFORE RESOLVING ICONS. This ordering is the startup guard, not
        // a style choice: resolving every app's icon eagerly cost 6.5s of
        // warmup (see `handlers/app_launcher.rs` and the icon-warmup work), so
        // only the handful actually shown may pay it. Each `OnceLock` then
        // caches for the process lifetime.
        .take(MAX_RECENT_APPS)
        .map(|(entry, _)| {
            let icon_path = entry
                .icon_path
                .get_or_init(|| {
                    entry
                        .icon
                        .as_deref()
                        .and_then(crate::action_registry::handlers::icons::resolve_icon)
                })
                .clone();
            CompletionItem {
                label: entry.name.clone(),
                icon_path,
                score: 0,
                run: Some(format!("open {}", entry.name)),
                // A recently-used app — unambiguously an app, so typed (see
                // CompletionKind::App). Pins are NOT stamped (mixed app/URL).
                kind: Some(crate::action_registry::CompletionKind::App),
                ..Default::default()
            }
        })
        .collect()
}

/// The empty-state hint shown to a brand-new user with no recents.
fn hint_item() -> CompletionItem {
    CompletionItem {
        label: "Type a command, or search the web".to_string(),
        icon_path: Some("__info__".to_string()),
        score: 0,
        description: Some("Your recent commands will appear here".to_string()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::SuggestionsConfig;
    use crate::desktop_apps::index;
    use crate::pins::store::PinsStore;

    fn cfg() -> SuggestionsConfig {
        SuggestionsConfig::default()
    }

    /// Pin the global app index for the duration of a test.
    ///
    /// The index is process-wide, so these tests must serialise on its lock
    /// and restore the real index afterwards — otherwise they assert whatever
    /// the machine happens to have installed, which passes on a developer
    /// desktop and fails on a bare CI runner.
    fn with_apps(names: &[(&str, &str)]) -> impl Drop {
        struct Restore {
            _lock: std::sync::MutexGuard<'static, ()>,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                index::rebuild_app_index();
            }
        }
        let guard = index::test_index_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        index::set_app_index_for_test(
            names
                .iter()
                .map(|(name, exec)| {
                    index::tests::make_entry(name, exec, &[], None, Some(&name.to_lowercase()))
                })
                .collect(),
        );
        Restore { _lock: guard }
    }

    // ── Pins ────────────────────────────────────────────────────────────

    /// The ownership contract: pins come first, in pin order, and compose
    /// WITHOUT context — the first summon after launch must never be blank.
    #[test]
    fn pins_come_first_in_pin_order_even_without_context() {
        let _idx = with_apps(&[]);
        let db = crate::db::open_test_database();
        let store = PinsStore::new();
        store.add(&db, "cargo test", "cargo test").unwrap();
        store.add(&db, "open Notes", "Notes").unwrap();

        let items = compose(&db, &cfg());
        assert_eq!(items[0].label, "cargo test");
        assert_eq!(items[1].label, "Notes");
        assert!(items[0].pinned && items[1].pinned);
        assert_eq!(items[0].reason.as_deref(), Some("Pinned"));
    }

    /// A pinned app absorbs its recent-app twin: one row, and it's the pin.
    #[test]
    fn a_pinned_app_dedupes_the_recent_app_row() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        frecency::record(&db, "spotify").unwrap();
        PinsStore::new()
            .add(&db, "open Spotify", "Spotify")
            .unwrap();

        let items = compose(&db, &cfg());
        let spotify_rows = items
            .iter()
            .filter(|i| {
                crate::pins::normalize_run(i.run.as_deref().unwrap_or(&i.label)) == "open spotify"
            })
            .count();
        assert_eq!(spotify_rows, 1, "pin + recent must collapse: {items:?}");
        assert!(items[0].pinned, "the surviving row is the pin");
    }

    /// `pinned` is a pin-only verdict — recents and hints never carry it.
    #[test]
    fn only_pin_rows_carry_the_pinned_flag() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        frecency::record(&db, "spotify").unwrap();
        PinsStore::new()
            .add(&db, "cargo test", "cargo test")
            .unwrap();

        let items = compose(&db, &cfg());
        for item in &items {
            assert_eq!(
                item.pinned,
                item.reason.as_deref() == Some("Pinned"),
                "pinned flag leaked: {item:?}"
            );
        }
    }

    /// Pins are explicit configuration, not history: the recents flag must
    /// not hide them.
    #[test]
    fn pins_survive_zero_state_recents_off() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        frecency::record(&db, "spotify").unwrap();
        PinsStore::new()
            .add(&db, "cargo test", "cargo test")
            .unwrap();

        let off = SuggestionsConfig {
            zero_state_recents: false,
            ..SuggestionsConfig::default()
        };
        let items = compose(&db, &off);
        assert_eq!(items.len(), 1, "only the pin: {items:?}");
        assert_eq!(items[0].label, "cargo test");
    }

    // ── Sections and budget ─────────────────────────────────────────────

    /// Eight pins own the whole panel; recents wait their turn elsewhere.
    #[test]
    fn a_full_pin_board_leaves_no_room_for_recents() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        frecency::record(&db, "spotify").unwrap();
        let store = PinsStore::new();
        for i in 0..crate::pins::store::MAX_PINS {
            store
                .add(&db, &format!("cmd {i}"), &format!("Cmd {i}"))
                .unwrap();
        }

        let items = compose(&db, &cfg());
        assert_eq!(items.len(), TARGET_ROWS);
        assert!(items.iter().all(|i| i.pinned), "{items:?}");
    }

    /// Decision 2026-08-11: the empty prompt shows pins and apps, NOTHING
    /// else. Workspace commands and clipboard actions — even well-used or
    /// freshly-copied ones — never appear; they vary with cwd/clipboard,
    /// which is exactly the inconsistency the zero state exists to avoid.
    #[test]
    fn command_rows_never_reach_the_empty_prompt() {
        let _idx = with_apps(&[]);
        let db = crate::db::open_test_database();
        // A well-established workspace habit (passes any use-count gate)...
        frecency::record_workspace(&db, "/home/u/proj", "cargo test").unwrap();
        frecency::record_workspace(&db, "/home/u/proj", "cargo test").unwrap();
        frecency::record_workspace(&db, "/home/u/proj", "cargo test").unwrap();
        // ...and a fresh clipboard capture.
        crate::clipboard::store::ClipboardStore::new()
            .push(&db, "https://example.com/x")
            .unwrap();

        let items = compose(&db, &cfg());
        assert!(
            !items.iter().any(|i| i.label == "cargo test"),
            "a workspace command reached the empty prompt: {items:?}"
        );
        assert!(
            !items
                .iter()
                .any(|i| i.run.as_deref() == Some("open https://example.com/x")),
            "a clipboard action reached the empty prompt: {items:?}"
        );
    }

    /// A brand-new user sees the honest hint, not an empty list.
    #[test]
    fn no_data_yields_the_hint() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        // The frecency entry cache is process-global and generation-keyed; a
        // test that never writes would read the PREVIOUS test's entries. One
        // namespaced write (never a row) points the cache at this db.
        frecency::record(&db, "history:cache-warm").unwrap();
        let items = compose(&db, &cfg());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].icon_path.as_deref(), Some("__info__"));
    }

    /// Dirty git repo, terminal focused, but nothing used: the zero state
    /// must NOT invent git commands. Context actions are typed-gated.
    #[test]
    fn speculative_context_never_reaches_the_empty_prompt() {
        let _idx = with_apps(&[]);
        let db = crate::db::open_test_database();
        frecency::record(&db, "history:open firefox").unwrap();
        frecency::record(&db, "history:web rust docs").unwrap();

        let items = compose(&db, &cfg());
        assert!(
            !items.iter().any(|i| i.label.starts_with("git ")),
            "speculative git commands must never appear: {items:?}"
        );
        assert!(
            !items.iter().any(|i| i.label == "web rust docs"),
            "a raw command string leaked into the zero state: {items:?}"
        );
    }

    // ── Recent apps (moved from context::suggestions) ───────────────────

    /// The headline: a launched app comes back as an APP ROW, not the literal
    /// text of the command that launched it.
    #[test]
    fn a_launched_app_returns_as_an_app_row() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        // Exactly what `app_launcher` records on launch.
        frecency::record(&db, "spotify").unwrap();

        let items = compose(&db, &cfg());

        let row = items
            .iter()
            .find(|i| i.label == "Spotify")
            .unwrap_or_else(|| panic!("no Spotify app row: {items:?}"));
        assert_eq!(row.run.as_deref(), Some("open Spotify"));
        assert!(
            !items.iter().any(|i| i.label == "open spotify"),
            "the raw command string is still being shown: {items:?}"
        );
    }

    /// Launching an app writes BOTH `spotify` and `history:open spotify`, so
    /// without run-keyed dedupe the user would see the app row AND the literal
    /// text — the exact bug this change exists to remove.
    #[test]
    fn the_history_twin_of_an_app_never_appears() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        frecency::record(&db, "spotify").unwrap();
        frecency::record(&db, "history:open spotify").unwrap();

        let items = compose(&db, &cfg());
        let opens: Vec<&CompletionItem> = items
            .iter()
            .filter(|i| {
                i.run
                    .as_deref()
                    .unwrap_or(&i.label)
                    .to_lowercase()
                    .starts_with("open spotify")
            })
            .collect();
        assert_eq!(
            opens.len(),
            1,
            "expected exactly one Spotify row: {items:?}"
        );
        assert_eq!(opens[0].label, "Spotify");
    }

    /// The keyspace is flat and shared. Only keys that resolve to an installed
    /// app may become rows.
    #[test]
    fn non_app_frecency_keys_never_become_rows() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        for key in [
            "history:web rust docs",
            "win:firefox",
            "ws:/home/u/p:cargo test",
            "/home/u/notes.md",
            "sug:something",
        ] {
            frecency::record(&db, key).unwrap();
        }

        let items = compose(&db, &cfg());

        // Assert on the COUNT, not on the labels. Five non-app keys were
        // recorded and no app key was, so the only correct answer is zero
        // app rows.
        assert!(
            items
                .iter()
                .all(|i| i.run.is_none()
                    || !i.run.as_deref().unwrap_or_default().starts_with("open ")),
            "a non-app frecency key produced an app row: {items:?}"
        );
    }

    /// An app recorded once and since uninstalled must not linger as a dead
    /// row that launches nothing.
    #[test]
    fn an_uninstalled_app_is_dropped() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        frecency::record(&db, "spotify").unwrap();
        frecency::record(&db, "an-app-that-was-removed").unwrap();

        let items = compose(&db, &cfg());
        assert!(
            !items
                .iter()
                .any(|i| i.label.eq_ignore_ascii_case("an-app-that-was-removed")),
            "an uninstalled app is still listed: {items:?}"
        );
    }

    #[test]
    fn app_rows_respect_the_cap() {
        let apps: Vec<(String, String)> = (0..10)
            .map(|i| (format!("App{i}"), format!("/usr/bin/app{i}")))
            .collect();
        let refs: Vec<(&str, &str)> = apps.iter().map(|(n, e)| (n.as_str(), e.as_str())).collect();
        let _idx = with_apps(&refs);

        let db = crate::db::open_test_database();
        for (name, _) in &apps {
            frecency::record(&db, &name.to_lowercase()).unwrap();
        }

        let items = compose(&db, &cfg());
        assert!(items.len() <= TARGET_ROWS, "over the total cap: {items:?}");
        let app_rows = items.iter().filter(|i| i.label.starts_with("App")).count();
        assert!(
            app_rows <= MAX_RECENT_APPS,
            "expected <= {MAX_RECENT_APPS} app rows, got {app_rows}"
        );
    }

    /// The startup guard, enforced rather than commented. Resolving every
    /// app's icon eagerly cost 6.5s of warmup; only the rows actually shown
    /// may pay for one. Pin rows count toward the same allowance.
    #[test]
    fn only_the_shown_rows_resolve_an_icon() {
        let apps: Vec<(String, String)> = (0..10)
            .map(|i| (format!("App{i}"), format!("/usr/bin/app{i}")))
            .collect();
        let refs: Vec<(&str, &str)> = apps.iter().map(|(n, e)| (n.as_str(), e.as_str())).collect();
        let _idx = with_apps(&refs);

        let db = crate::db::open_test_database();
        for (name, _) in &apps {
            frecency::record(&db, &name.to_lowercase()).unwrap();
        }
        PinsStore::new().add(&db, "open App9", "App9").unwrap();

        let _ = compose(&db, &cfg());

        let resolved = index::app_index()
            .entries
            .iter()
            .filter(|e| e.icon_path.get().is_some())
            .count();
        assert!(
            resolved <= MAX_RECENT_APPS + 1,
            "resolved {resolved} icons for at most {} visible rows — \
             the take-before-resolve ordering has been lost",
            MAX_RECENT_APPS + 1
        );
    }

    /// The index lookup — not the `:`/`/` pre-filter — is what decides whether
    /// a frecency key names an app. A key that is NOT an app name must produce
    /// no row even though it passes the pre-filter.
    #[test]
    fn a_plain_non_app_key_produces_no_row() {
        let _idx = with_apps(&[("Spotify", "/usr/bin/spotify")]);
        let db = crate::db::open_test_database();
        // No colon, no leading slash — sails past the pre-filter, and is still
        // not an installed app.
        frecency::record(&db, "definitely-not-an-app").unwrap();

        let items = compose(&db, &cfg());
        assert!(
            !items.iter().any(|i| i.run.is_some()),
            "a key that is not an app produced a launchable row: {items:?}"
        );
    }
}
