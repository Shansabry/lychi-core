//! Config schema migration.
//!
//! `config.toml` has no built-in versioning in TOML itself, so a field that is
//! **renamed, moved, or removed** would otherwise vanish silently on the next
//! load: the old key becomes an unknown field, `toml` drops it, and the value is
//! gone. (Adding a field is already safe — the default-overlay load in
//! [`super::Config::load`] fills any missing field from `Config::default()`.)
//!
//! This module closes that gap the same way the redb rows do with their
//! `[ver][body]` envelope: a `[meta] schema_version` stamp plus an ordered list
//! of migrations. Each migration rewrites the **raw `toml::Value`** — the only
//! stage where a renamed/removed key still exists, before it is merged onto the
//! defaults and deserialized into the typed [`super::Config`].
//!
//! ## The contract
//!
//! - A file with no `[meta]` section reads as version 0 ("legacy") and every
//!   migration runs.
//! - A file at version `N` runs migrations `N+1..=CURRENT`.
//! - After the pipeline the caller stamps [`CURRENT_SCHEMA_VERSION`], so the
//!   next save persists the new version and the migrations don't run again.
//!
//! ## Adding a migration
//!
//! 1. Bump [`CURRENT_SCHEMA_VERSION`] in `schema.rs` by one.
//! 2. Push a `Migration` here whose `to_version` equals the new number, whose
//!    `apply` mutates the raw `toml::Value` (rename a key, move a section, drop
//!    a dead one), and whose entry carries a one-line reason.
//! 3. Add a round-trip test proving an old-shaped file lands on the new field.
//!
//! Migrations must be **pure and idempotent on their input shape**: they see a
//! `toml::Value::Table` (or should no-op if not) and must never panic. A
//! migration that can't find what it expected simply leaves the value alone —
//! the default-overlay merge then supplies the default, which is the same safe
//! outcome as a fresh install.

use super::schema::CURRENT_SCHEMA_VERSION;

/// One ordered schema step. `to_version` is the version the file is AT once
/// `apply` has run.
struct Migration {
    to_version: u32,
    reason: &'static str,
    apply: fn(&mut toml::Value),
}

/// The ordered migration list. Index is irrelevant; `to_version` is the key.
/// MUST be sorted ascending by `to_version` and contiguous from 1.
fn migrations() -> Vec<Migration> {
    vec![
        // v1: the first versioned schema. There is nothing structural to move
        // from the pre-versioning shape (every field that existed then still
        // exists under the same key), so this step only exists to establish the
        // stamp: a legacy file (version 0) is declared current at 1 without any
        // key rewrite. Real renames/removals get their own step from v2 on.
        Migration {
            to_version: 1,
            reason: "establish schema versioning (no key changes)",
            apply: |_value| {},
        },
    ]
}

/// Read the `[meta] schema_version` out of a raw parsed config, defaulting to 0
/// (legacy / pre-versioning) when absent or malformed.
fn read_version(value: &toml::Value) -> u32 {
    value
        .get("meta")
        .and_then(|m| m.get("schema_version"))
        .and_then(|v| v.as_integer())
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

/// Migrate a raw parsed `config.toml` value up to [`CURRENT_SCHEMA_VERSION`].
///
/// Runs on the `toml::Value` BEFORE the default-overlay merge, so renamed and
/// removed keys are still present to be moved. Returns the number of migrations
/// applied (0 when the file is already current) so the caller can log it.
///
/// Does not stamp the version into the value — the caller stamps the typed
/// [`super::Config`] after deserialization, which is the struct that gets saved.
pub(super) fn migrate_value(value: &mut toml::Value) -> u32 {
    let from = read_version(value);
    if from >= CURRENT_SCHEMA_VERSION {
        return 0;
    }
    let mut applied = 0;
    for m in migrations() {
        if m.to_version > from {
            (m.apply)(value);
            tracing::info!("config migrated to schema v{} ({})", m.to_version, m.reason);
            applied += 1;
        }
    }
    applied
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list must be contiguous 1..=CURRENT and sorted — an off-by-one here
    /// would skip a migration and reintroduce the silent-drop bug this prevents.
    #[test]
    fn migration_list_is_contiguous_and_current() {
        let ms = migrations();
        assert_eq!(
            ms.last().map(|m| m.to_version),
            Some(CURRENT_SCHEMA_VERSION),
            "the last migration must reach CURRENT_SCHEMA_VERSION"
        );
        for (i, m) in ms.iter().enumerate() {
            assert_eq!(
                m.to_version as usize,
                i + 1,
                "migrations must be contiguous starting at 1"
            );
        }
    }

    /// A file with no [meta] is treated as legacy (version 0) and gets brought
    /// current.
    #[test]
    fn legacy_file_without_meta_is_migrated() {
        let mut v: toml::Value = toml::from_str("[general]\ntheme = \"dark\"\n").unwrap();
        assert_eq!(read_version(&v), 0);
        let applied = migrate_value(&mut v);
        assert_eq!(applied, CURRENT_SCHEMA_VERSION);
    }

    /// A file already at CURRENT runs nothing.
    #[test]
    fn current_file_is_untouched() {
        let src = format!("[meta]\nschema_version = {CURRENT_SCHEMA_VERSION}\n");
        let mut v: toml::Value = toml::from_str(&src).unwrap();
        assert_eq!(read_version(&v), CURRENT_SCHEMA_VERSION);
        assert_eq!(migrate_value(&mut v), 0);
    }

    /// A malformed schema_version (wrong type) is read as legacy, not a panic.
    #[test]
    fn malformed_version_reads_as_legacy() {
        let mut v: toml::Value = toml::from_str("[meta]\nschema_version = \"nope\"\n").unwrap();
        assert_eq!(read_version(&v), 0);
        // Must not panic.
        let _ = migrate_value(&mut v);
    }
}
