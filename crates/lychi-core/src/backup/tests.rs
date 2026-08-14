use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

/// Each test gets its own XDG data dir, because `paths::backups_dir()` reads
/// the environment. Tests run in parallel, so the counter (not a timestamp)
/// guarantees uniqueness.
struct Sandbox {
    dir: PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Sandbox {
    fn new(tag: &str) -> Self {
        // `set_var` is process-global, so these tests serialise on one lock.
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "lychi-backup-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(dir.join("data")).unwrap();
        fs::create_dir_all(dir.join("config")).unwrap();
        // SAFETY: serialised by ENV_LOCK for the lifetime of this Sandbox.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", dir.join("data"));
            std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
        }
        Self { dir, _guard: guard }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn test_db(sb: &Sandbox) -> Arc<Database> {
    crate::db::open_database(&sb.dir.join("test.redb")).unwrap()
}

fn put(db: &Arc<Database>, table: TableDefinition<&str, &[u8]>, k: &str, v: &[u8]) {
    let txn = db.begin_write().unwrap();
    {
        let mut t = txn.open_table(table).unwrap();
        t.insert(k, v).unwrap();
    }
    txn.commit().unwrap();
}

fn get(db: &Arc<Database>, table: TableDefinition<&str, &[u8]>, k: &str) -> Option<Vec<u8>> {
    let txn = db.begin_read().unwrap();
    let t = txn.open_table(table).unwrap();
    t.get(k).unwrap().map(|v| v.value().to_vec())
}

#[test]
fn round_trip_restores_every_row() {
    let sb = Sandbox::new("roundtrip");
    let db = test_db(&sb);
    put(&db, crate::db::TODOS, "h1", b"one");
    put(&db, crate::db::NOTES, "n1", b"a note");
    put(&db, crate::db::SNIPPETS, "c1", b"a clip");

    let info = create(&db, BackupKind::Manual, "test", "0.1.0").unwrap();
    assert!(info.manifest.is_some());

    // Destroy the live data.
    {
        let txn = db.begin_write().unwrap();
        {
            let mut t = txn.open_table(crate::db::TODOS).unwrap();
            t.retain(|_, _| false).unwrap();
            let mut t = txn.open_table(crate::db::NOTES).unwrap();
            t.retain(|_, _| false).unwrap();
        }
        txn.commit().unwrap();
    }
    assert!(get(&db, crate::db::TODOS, "h1").is_none());

    let report = restore(&db, Path::new(&info.path), "0.1.0").unwrap();
    assert!(report.rows_restored >= 3);
    assert_eq!(
        get(&db, crate::db::TODOS, "h1").as_deref(),
        Some(&b"one"[..])
    );
    assert_eq!(
        get(&db, crate::db::NOTES, "n1").as_deref(),
        Some(&b"a note"[..])
    );
    assert_eq!(
        get(&db, crate::db::SNIPPETS, "c1").as_deref(),
        Some(&b"a clip"[..])
    );
}

/// Restore must be reversible: the state it replaced is itself backed up.
#[test]
fn restore_snapshots_the_state_it_replaces() {
    let sb = Sandbox::new("safety");
    let db = test_db(&sb);
    put(&db, crate::db::TODOS, "old", b"original");
    let first = create(&db, BackupKind::Manual, "first", "0.1.0").unwrap();

    // Move on to a different state, then go back to the first backup.
    put(&db, crate::db::TODOS, "new", b"later");
    let report = restore(&db, Path::new(&first.path), "0.1.0").unwrap();

    // The "later" row is gone from live data...
    assert!(get(&db, crate::db::TODOS, "new").is_none());
    // ...but recoverable from the safety backup the restore took.
    let safety = list()
        .into_iter()
        .find(|b| b.name == report.safety_backup)
        .expect("safety backup should be listed");
    restore(&db, Path::new(&safety.path), "0.1.0").unwrap();
    assert_eq!(
        get(&db, crate::db::TODOS, "new").as_deref(),
        Some(&b"later"[..])
    );
}

/// Replace, not merge — a row absent from the backup must not survive.
#[test]
fn restore_replaces_rather_than_merges() {
    let sb = Sandbox::new("replace");
    let db = test_db(&sb);
    put(&db, crate::db::TODOS, "kept", b"in backup");
    let info = create(&db, BackupKind::Manual, "t", "0.1.0").unwrap();

    put(&db, crate::db::TODOS, "extra", b"added after");
    restore(&db, Path::new(&info.path), "0.1.0").unwrap();

    assert_eq!(
        get(&db, crate::db::TODOS, "kept").as_deref(),
        Some(&b"in backup"[..])
    );
    assert!(
        get(&db, crate::db::TODOS, "extra").is_none(),
        "a row not in the backup must not survive a replace-restore"
    );
}

/// A truncated archive must be caught BEFORE any live data is touched.
#[test]
fn a_truncated_archive_is_refused_and_changes_nothing() {
    let sb = Sandbox::new("truncated");
    let db = test_db(&sb);
    put(&db, crate::db::TODOS, "live", b"precious");
    let info = create(&db, BackupKind::Manual, "t", "0.1.0").unwrap();

    // Chop the archive in half.
    let bytes = fs::read(&info.path).unwrap();
    fs::write(&info.path, &bytes[..bytes.len() / 2]).unwrap();

    let before = list().len();
    let err = restore(&db, Path::new(&info.path), "0.1.0");
    assert!(err.is_err(), "a truncated archive must not restore");

    // Live data intact, and no safety backup was taken — we failed before it.
    assert_eq!(
        get(&db, crate::db::TODOS, "live").as_deref(),
        Some(&b"precious"[..])
    );
    assert_eq!(
        list().len(),
        before,
        "a refused restore must not create backups"
    );
}

/// A backup from a newer Lychi is refused rather than half-understood.
#[test]
fn a_newer_backup_is_refused() {
    let sb = Sandbox::new("newer");
    let db = test_db(&sb);
    put(&db, crate::db::TODOS, "x", b"y");
    let info = create(&db, BackupKind::Manual, "t", "9.9.9").unwrap();

    let err = restore(&db, Path::new(&info.path), "0.1.0");
    assert!(
        err.is_err(),
        "a backup from a newer version must be refused"
    );
    let msg = format!("{}", err.unwrap_err());
    assert!(
        msg.contains("update Lychi"),
        "message should tell the user what to do: {msg}"
    );
}

#[test]
fn version_comparison() {
    assert!(is_newer("0.2.0", "0.1.0"));
    assert!(is_newer("1.0.0", "0.9.9"));
    assert!(!is_newer("0.1.0", "0.1.0"));
    assert!(!is_newer("0.1.0", "0.2.0"));
    // Pre-release and `v` prefixes parse.
    assert!(is_newer("v0.2.0-beta.1", "0.1.0"));
    // Unparseable never blocks a restore.
    assert!(!is_newer("garbage", "0.1.0"));
    assert!(!is_newer("0.1.0", "garbage"));
}

/// Tables are enumerated from the database, so a table this file has never
/// heard of is still backed up and restored.
#[test]
fn an_unknown_table_is_captured_without_code_changes() {
    let sb = Sandbox::new("unknown");
    let db = test_db(&sb);
    let custom: TableDefinition<&str, &[u8]> = TableDefinition::new("a_future_table");
    put(&db, custom, "k", b"v");

    let info = create(&db, BackupKind::Manual, "t", "0.1.0").unwrap();
    assert!(
        info.manifest
            .as_ref()
            .unwrap()
            .tables
            .iter()
            .any(|(n, _)| n == "a_future_table"),
        "enumeration should have found the unknown table"
    );

    {
        let txn = db.begin_write().unwrap();
        {
            let mut t = txn.open_table(custom).unwrap();
            t.retain(|_, _| false).unwrap();
        }
        txn.commit().unwrap();
    }
    restore(&db, Path::new(&info.path), "0.1.0").unwrap();
    assert_eq!(get(&db, custom, "k").as_deref(), Some(&b"v"[..]));
}

#[test]
fn config_and_scripts_round_trip() {
    let sb = Sandbox::new("files");
    let db = test_db(&sb);

    let cfg = crate::paths::config_file();
    fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    fs::write(&cfg, b"[general]\ntheme = \"dark\"\n").unwrap();
    let scripts = crate::paths::scripts_dir();
    fs::create_dir_all(&scripts).unwrap();
    fs::write(scripts.join("hello.sh"), b"#!/bin/sh\necho hi\n").unwrap();

    let info = create(&db, BackupKind::Manual, "t", "0.1.0").unwrap();
    let m = info.manifest.as_ref().unwrap();
    assert!(m.has_config && m.has_scripts);

    fs::write(&cfg, b"[general]\ntheme = \"light\"\n").unwrap();
    fs::remove_file(scripts.join("hello.sh")).unwrap();

    let report = restore(&db, Path::new(&info.path), "0.1.0").unwrap();
    assert!(report.config_restored && report.scripts_restored);
    assert!(fs::read_to_string(&cfg).unwrap().contains("dark"));
    assert!(scripts.join("hello.sh").exists());
}

/// DATA-8: clipboard image PNGs must ride along in the archive and come back on
/// restore. Without this a restore round-trip leaves the rows' path references
/// dangling and the startup orphan-GC deletes the images.
#[test]
fn clipboard_images_round_trip() {
    let sb = Sandbox::new("clipimg");
    let db = test_db(&sb);

    let imgdir = crate::paths::clipboard_images_dir();
    fs::create_dir_all(&imgdir).unwrap();
    let png = imgdir.join("abc123.png");
    // A minimal but real PNG signature + some bytes.
    fs::write(&png, b"\x89PNG\r\n\x1a\nfake-image-bytes").unwrap();

    let info = create(&db, BackupKind::Manual, "t", "0.1.0").unwrap();
    assert!(
        info.manifest.as_ref().unwrap().has_clipboard_images,
        "the backup must record that it carries image files"
    );

    // Simulate the loss: the PNG is deleted (as the orphan-GC would).
    fs::remove_file(&png).unwrap();
    assert!(!png.exists());

    restore(&db, Path::new(&info.path), "0.1.0").unwrap();
    assert!(png.exists(), "the clipboard image must be restored to disk");
    assert_eq!(
        fs::read(&png).unwrap(),
        b"\x89PNG\r\n\x1a\nfake-image-bytes",
        "the exact bytes must round-trip"
    );
}

/// An archive is untrusted input: a `../` entry must never write outside the
/// scripts directory.
#[test]
fn path_traversal_in_an_archive_is_refused() {
    let sb = Sandbox::new("traversal");
    let db = test_db(&sb);
    let info = create(&db, BackupKind::Manual, "t", "0.1.0").unwrap();

    // Rebuild the archive with a malicious entry appended.
    let manifest = read_manifest(Path::new(&info.path)).unwrap();
    let evil = sb.dir.join("evil.tar.gz");
    {
        let f = fs::File::create(&evil).unwrap();
        let enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        let mb = serde_json::to_vec(&manifest).unwrap();
        append_bytes(&mut tar, MANIFEST_NAME, &mb).unwrap();
        // `tar::Builder::append_data` refuses `..`, which is exactly why a
        // hostile archive would not be built with it. Write the header
        // directly, the way a crafted archive arrives from outside.
        let evil_name = "config/scripts/../../../pwned.sh";
        let payload = b"evil";
        let mut h = tar::Header::new_gnu();
        h.set_size(payload.len() as u64);
        h.set_mode(0o600);
        h.set_entry_type(tar::EntryType::Regular);
        h.as_gnu_mut()
            .unwrap()
            .name
            .get_mut(..evil_name.len())
            .unwrap()
            .copy_from_slice(evil_name.as_bytes());
        h.set_cksum();
        tar.append(&h, &payload[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
    }

    let _ = restore(&db, &evil, "0.1.0");
    assert!(
        !sb.dir.join("pwned.sh").exists() && !sb.dir.join("config/pwned.sh").exists(),
        "path traversal must not write outside the scripts directory"
    );
}

/// Manual backups are the ones a user took deliberately; housekeeping must
/// never evict them.
#[test]
fn pruning_keeps_manual_backups() {
    let sb = Sandbox::new("prune");
    let db = test_db(&sb);
    put(&db, crate::db::TODOS, "k", b"v");

    let manual = create(&db, BackupKind::Manual, "keep me", "0.1.0").unwrap();
    for i in 0..(AUTO_RETAIN + 4) {
        create(&db, BackupKind::Automatic, &format!("auto {i}"), "0.1.0").unwrap();
    }

    let all = list();
    assert!(
        all.iter().any(|b| b.name == manual.name),
        "a manual backup must survive automatic pruning"
    );
    let autos = all
        .iter()
        .filter(|b| {
            b.manifest
                .as_ref()
                .is_some_and(|m| m.kind == BackupKind::Automatic)
        })
        .count();
    assert!(
        autos <= AUTO_RETAIN,
        "expected <= {AUTO_RETAIN} autos, got {autos}"
    );
}

#[test]
fn delete_refuses_paths_outside_the_backups_dir() {
    let sb = Sandbox::new("delete");
    let _db = test_db(&sb);
    assert!(delete("../../etc/passwd").is_err());
    assert!(delete("/etc/passwd").is_err());
}

#[test]
fn list_is_newest_first() {
    let sb = Sandbox::new("order");
    let db = test_db(&sb);
    put(&db, crate::db::TODOS, "k", b"v");
    for i in 0..3 {
        create(&db, BackupKind::Manual, &format!("b{i}"), "0.1.0").unwrap();
    }
    let all = list();
    let times: Vec<u64> = all
        .iter()
        .filter_map(|b| b.manifest.as_ref().map(|m| m.created_at))
        .collect();
    let mut sorted = times.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(times, sorted, "list() must be newest-first");
}

#[test]
fn restorability_is_reported_for_the_ui() {
    let m = Manifest {
        archive_version: ARCHIVE_VERSION,
        app_version: "0.1.0".into(),
        created_at: 0,
        kind: BackupKind::Manual,
        reason: String::new(),
        tables: vec![],
        schema_version: crate::db::SCHEMA_VERSION,
        has_config: false,
        has_scripts: false,
        has_clipboard_images: false,
    };
    let ok = BackupInfo {
        path: "/x".into(),
        name: "x".into(),
        size_bytes: 0,
        manifest: Some(m.clone()),
    };
    assert!(ok.is_restorable("0.1.0"));
    assert!(
        !ok.is_restorable("0.0.9"),
        "a newer backup is not restorable"
    );

    let unreadable = BackupInfo {
        manifest: None,
        ..ok.clone()
    };
    assert!(
        !unreadable.is_restorable("0.1.0"),
        "an archive with no manifest must never be restorable"
    );

    let future = BackupInfo {
        manifest: Some(Manifest {
            archive_version: ARCHIVE_VERSION + 1,
            ..m
        }),
        ..ok
    };
    assert!(!future.is_restorable("0.1.0"));
}

#[test]
fn timestamp_is_sortable_and_correct() {
    // 1786050000000 == 2026-08-06T21:00:00Z (verified independently).
    assert_eq!(stamp(1_786_050_000_000), "20260806-210000");
    // Lexical order matches chronological order — what makes filenames sort.
    assert!(stamp(1_786_050_000_000) < stamp(1_786_150_000_000));
    // A leap day, to exercise the civil-from-days arithmetic.
    // 1709208000000 == 2024-02-29T12:00:00Z.
    assert_eq!(stamp(1_709_208_000_000), "20240229-120000");
}

/// A fresh install has nothing worth preserving — do not litter the backups
/// directory on first launch.
#[test]
fn first_run_takes_no_backup_but_records_the_version() {
    let sb = Sandbox::new("firstrun");
    let db = test_db(&sb);
    assert!(backup_if_upgraded(&db, "0.1.0").is_none());
    assert!(
        list().is_empty(),
        "a fresh install should not create a backup"
    );
    // Second launch of the SAME version is also a no-op.
    assert!(backup_if_upgraded(&db, "0.1.0").is_none());
    assert!(list().is_empty());
}

/// The upgrade case: the pre-upgrade state is captured before the new version
/// has written anything.
#[test]
fn an_upgrade_snapshots_the_old_version_first() {
    let sb = Sandbox::new("upgrade");
    let db = test_db(&sb);
    backup_if_upgraded(&db, "0.1.0");
    put(&db, crate::db::TODOS, "pre", b"data from 0.1.0");

    let taken = backup_if_upgraded(&db, "0.2.0").expect("an upgrade must take a backup");
    let m = taken.manifest.as_ref().unwrap();
    assert_eq!(m.kind, BackupKind::Automatic);
    assert!(
        m.reason.contains("0.2.0"),
        "reason should name the target version"
    );
    // Stamped with the version that produced the DATA, so it stays restorable.
    assert_eq!(m.app_version, "0.1.0");

    // Re-running 0.2.0 does not take another.
    assert!(backup_if_upgraded(&db, "0.2.0").is_none());

    // And the snapshot really holds the pre-upgrade row.
    {
        let txn = db.begin_write().unwrap();
        {
            let mut t = txn.open_table(crate::db::TODOS).unwrap();
            t.retain(|_, _| false).unwrap();
        }
        txn.commit().unwrap();
    }
    restore(&db, Path::new(&taken.path), "0.2.0").unwrap();
    assert_eq!(
        get(&db, crate::db::TODOS, "pre").as_deref(),
        Some(&b"data from 0.1.0"[..])
    );
}

/// One rolling automatic archive, refreshed — not accumulated.
#[test]
fn the_hourly_backup_replaces_the_previous_one() {
    let sb = Sandbox::new("hourly");
    let db = test_db(&sb);
    put(&db, crate::db::TODOS, "k", b"v");

    let first = hourly_backup(&db, "0.1.0").expect("first run must back up");
    // Force the interval to have elapsed.
    crate::config::db::save_setting(&db, LAST_AUTO_BACKUP_KEY, "0").unwrap();
    let second = hourly_backup(&db, "0.1.0").expect("an elapsed interval must back up");

    assert_ne!(first.name, second.name);
    let rolling: Vec<_> = list()
        .into_iter()
        .filter(|b| b.manifest.as_ref().is_some_and(|m| m.reason == "hourly"))
        .collect();
    assert_eq!(
        rolling.len(),
        1,
        "expected ONE rolling backup, got {rolling:?}"
    );
    assert_eq!(
        rolling[0].name, second.name,
        "the newest must be the survivor"
    );
}

/// Within the hour it must not fire again — otherwise every summon writes an
/// archive.
#[test]
fn a_second_summon_within_the_hour_does_nothing() {
    let sb = Sandbox::new("interval");
    let db = test_db(&sb);
    put(&db, crate::db::TODOS, "k", b"v");

    assert!(hourly_backup(&db, "0.1.0").is_some());
    assert!(
        hourly_backup(&db, "0.1.0").is_none(),
        "a summon inside the interval must not take another backup"
    );
}

/// The rolling refresh must never evict a pre-upgrade snapshot: that marks the
/// riskiest moment for data and has to outlive the next hour.
#[test]
fn the_rolling_refresh_spares_upgrade_and_manual_backups() {
    let sb = Sandbox::new("spare");
    let db = test_db(&sb);
    put(&db, crate::db::TODOS, "k", b"v");

    let manual = create(&db, BackupKind::Manual, "keep me", "0.1.0").unwrap();
    let upgrade = create(
        &db,
        BackupKind::Automatic,
        "before upgrade to 0.2.0",
        "0.1.0",
    )
    .unwrap();

    // Refresh several times. Each pass calls `prune_rolling`, so if that
    // dropped its `reason == "hourly"` check the upgrade snapshot would be
    // deleted on the very first refresh — two passes only proves the newest
    // rolling one survives, which is a different claim.
    for _ in 0..3 {
        crate::config::db::save_setting(&db, LAST_AUTO_BACKUP_KEY, "0").unwrap();
        hourly_backup(&db, "0.1.0").unwrap();
    }

    let names: Vec<String> = list().into_iter().map(|b| b.name).collect();
    assert!(
        names.contains(&manual.name),
        "a manual backup was evicted: {names:?}"
    );
    assert!(
        names.contains(&upgrade.name),
        "a pre-upgrade backup was evicted: {names:?}"
    );
}

/// A clock that jumps backwards must defer, not fire on every summon.
#[test]
fn a_backwards_clock_does_not_cause_a_backup_storm() {
    let sb = Sandbox::new("clock");
    let db = test_db(&sb);
    put(&db, crate::db::TODOS, "k", b"v");

    hourly_backup(&db, "0.1.0").unwrap();
    // "Last backup" in the future — what a backwards clock jump looks like.
    let future = crate::db::now_millis() + 10 * AUTO_BACKUP_INTERVAL_MS;
    crate::config::db::save_setting(&db, LAST_AUTO_BACKUP_KEY, &future.to_string()).unwrap();

    assert!(
        hourly_backup(&db, "0.1.0").is_none(),
        "a backwards clock must defer, not back up repeatedly"
    );
}
