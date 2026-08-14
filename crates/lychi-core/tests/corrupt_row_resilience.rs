//! A corrupt row must cost you that row, not the whole feature.
//!
//! Every `get_*` in the stores used `?` inside the iteration, so one
//! undecodable row aborted the entire query. The user saw "all my notes are
//! gone" while 99 intact notes sat on disk with no way to reach them.
//!
//! This matters most on **downgrade**. postcard is not self-describing: a row
//! written by a newer schema is not detectably different from garbage. Someone
//! who tries a new version and rolls back should lose the rows whose shape
//! changed, not the feature.
//!
//! These tests write real garbage into real tables through the real store API,
//! rather than asserting on the decoder in isolation — the bug was never in the
//! decoder, it was in what each caller did with a failure.

use std::sync::Arc;

use lychi_core::db;
use redb::Database;

/// A throwaway DB. Mirrors `db::open_test_database` (which is `#[cfg(test)]`
/// and so invisible to an integration test) — counter-keyed, never timestamped,
/// because two parallel tests can read the same nanosecond.
fn test_db() -> Arc<Database> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "lychi-corrupt-{}-{}.redb",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("redb.bak"));
    db::open_database(&path).expect("test database")
}

/// Write bytes that are not a valid encoding of the row type.
///
/// Deliberately not random: this is what a *downgrade* actually leaves behind —
/// a well-formed row of a different shape — plus a truncation, which is what a
/// partial write leaves.
fn write_garbage(db: &Arc<Database>, table: redb::TableDefinition<&str, &[u8]>, key: &str) {
    let txn = db.begin_write().unwrap();
    {
        let mut t = txn.open_table(table).unwrap();
        t.insert(key, [0xFF_u8, 0xFF, 0xFF, 0xFF, 0xFF].as_slice())
            .unwrap();
    }
    txn.commit().unwrap();
}

#[test]
fn a_corrupt_note_does_not_hide_the_others() {
    let db = test_db();
    let store = lychi_core::notes::store::NotesStore::new();

    store.add_note(&db, "first").unwrap();
    store.add_note(&db, "second").unwrap();
    // UUID v7 keys sort by creation time, so a "z" key lands last — the corrupt
    // row must not stop iteration OR swallow rows after it.
    write_garbage(&db, db::NOTES, "aaaa-corrupt");
    write_garbage(&db, db::NOTES, "zzzz-corrupt");
    store.add_note(&db, "third").unwrap();

    let notes = store.get_notes(&db).unwrap();
    let texts: Vec<&str> = notes.iter().map(|n| n.text.as_str()).collect();
    assert_eq!(
        texts.len(),
        3,
        "the readable notes must all survive; got {texts:?}"
    );
    for want in ["first", "second", "third"] {
        assert!(texts.contains(&want), "{want} missing from {texts:?}");
    }
}

#[test]
fn counting_notes_skips_corrupt_rows_too() {
    // `notes_count` gates adding a note. If it aborted, the corrupt row would
    // make the notes feature unusable for *writing* as well as reading.
    let db = test_db();
    let store = lychi_core::notes::store::NotesStore::new();
    store.add_note(&db, "one").unwrap();
    write_garbage(&db, db::NOTES, "bad");

    assert_eq!(store.notes_count(&db).unwrap(), 1);
    // And the feature still works: adding must not fail.
    store
        .add_note(&db, "two")
        .expect("adding a note must still work with a corrupt row present");
    assert_eq!(store.notes_count(&db).unwrap(), 2);
}

#[test]
fn a_corrupt_todo_does_not_hide_the_others() {
    let db = test_db();
    let store = lychi_core::notes::store::NotesStore::new();
    store.add_todo(&db, "buy milk").unwrap();
    write_garbage(&db, db::TODOS, "bad");
    store.add_todo(&db, "call bank").unwrap();

    let todos = store.get_todos(&db).unwrap();
    assert_eq!(todos.len(), 2, "both readable todos must survive");
}

#[test]
fn the_unified_scratch_list_survives_corruption_in_either_table() {
    // `get_all_items` reads notes AND todos; a corrupt row in one must not
    // empty the other.
    let db = test_db();
    let store = lychi_core::notes::store::NotesStore::new();
    store.add_note(&db, "a note").unwrap();
    store.add_todo(&db, "a todo").unwrap();
    write_garbage(&db, db::NOTES, "bad-note");
    write_garbage(&db, db::TODOS, "bad-todo");

    let items = store.get_all_items(&db).unwrap();
    assert_eq!(items.len(), 2, "one good note + one good todo must survive");
}

// Clipboard history moved out of redb into a JSONL file, so "a corrupt row does
// not empty the history" is now the file store's line-level resilience: a
// garbage line between good ones is skipped, not fatal. Covered directly by
// `filestore::tests::corrupt_middle_line_is_skipped_not_fatal` and the
// clipboard store's own tests, so there is nothing to assert against the
// database here.

#[test]
fn a_corrupt_snippet_does_not_hide_the_others() {
    let db = test_db();
    let store = lychi_core::snippets::store::SnippetsStore::new();
    store.add_snippet(&db, "sig", "regards").unwrap();
    write_garbage(&db, db::SNIPPETS, "bad");

    let all = store.get_snippets(&db).unwrap();
    assert_eq!(all.len(), 1, "the readable snippet must survive");
}

#[test]
fn a_corrupt_setting_does_not_reset_every_setting() {
    // Settings are the worst case: an abort here means the app starts with
    // defaults and then SAVES them, so the corruption of one row silently
    // destroys the user's entire configuration.
    let db = test_db();
    lychi_core::config::db::save_setting(&db, "general.theme", "dark").unwrap();
    write_garbage(&db, db::SETTINGS, "general.corrupt");
    lychi_core::config::db::save_setting(&db, "general.font", "Inter").unwrap();

    let loaded = lychi_core::config::db::load_syncable(&db).unwrap();
    assert_eq!(
        loaded.get("general.theme").map(String::as_str),
        Some("dark")
    );
    assert_eq!(
        loaded.get("general.font").map(String::as_str),
        Some("Inter")
    );
}

#[test]
fn a_wholly_corrupt_table_is_empty_rather_than_an_error() {
    // The degenerate case: every row unreadable. "No notes" is a state the UI
    // already handles; an error is not, and would present as a broken app.
    let db = test_db();
    let store = lychi_core::notes::store::NotesStore::new();
    for i in 0..5 {
        write_garbage(&db, db::NOTES, &format!("bad-{i}"));
    }

    let notes = store
        .get_notes(&db)
        .expect("a fully corrupt table must read as empty, not as an error");
    assert!(notes.is_empty());
}
