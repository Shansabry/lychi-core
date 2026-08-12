//! Sanitising the environment for spawned external programs.
//!
//! Lychi ships as an **AppImage**, whose `AppRun` (via linuxdeploy's GTK plugin)
//! exports a pile of `GTK_*` / `GDK_*` / `GIO_*` variables — plus `LD_LIBRARY_PATH`
//! and `XDG_DATA_DIRS` — all pointing into the AppImage's `/tmp/.mount_*` FUSE
//! mount. That is correct for Lychi's OWN process (it must load its bundled GTK),
//! but it is **inherited by every child Lychi spawns**.
//!
//! When Lychi launches another GTK program — a terminal (`gnome-terminal`), a
//! browser, an editor — that child then tries to load GTK modules, pixbuf
//! loaders, GIO modules, and (via `LD_LIBRARY_PATH`) shared libraries from
//! *Lychi's* mount rather than the system. Version mismatch or a path into a
//! FUSE mount that vanishes makes GTK/GLib init fail — the well-known AppImage
//! "spawned app crashes intermittently" bug. It presented as `gnome-terminal`
//! crashing right after `ssh` (its D-Bus client/server start straddles a clean
//! vs polluted env, hence *intermittent*); Qt terminals like `konsole` are
//! unaffected because none of these variables touch Qt.
//!
//! The fix: strip the AppImage-injected variables before spawning any external
//! program, so it runs in the USER's real desktop environment. A no-op when not
//! running from an AppImage (`$APPDIR` unset) — dev, RPM and deb installs are
//! untouched.

/// Variables the AppImage sets FRESH (no pre-AppImage value to preserve): remove
/// them entirely so the child falls back to the system defaults.
const APPIMAGE_SET_VARS: &[&str] = &[
    "APPDIR",
    "GTK_DATA_PREFIX",
    "GTK_EXE_PREFIX",
    "GTK_PATH",
    "GTK_IM_MODULE_FILE",
    "GTK_THEME",
    "GDK_BACKEND",
    "GDK_PIXBUF_MODULE_FILE",
    "GIO_EXTRA_MODULES",
    "GSETTINGS_SCHEMA_DIR",
];

/// Variables the AppImage PREPENDS its mount paths onto (a real system value may
/// follow): drop only the mount entries, keep the rest. Unset if nothing remains.
const APPIMAGE_PREPENDED_VARS: &[&str] = &["LD_LIBRARY_PATH", "XDG_DATA_DIRS"];

/// The mount-path marker every AppImage-injected path contains. `$APPDIR` is
/// `/tmp/.mount_<name><rand>` (or an extracted AppDir), so filtering on this
/// prefix removes exactly the AppImage's own entries.
fn is_appimage_path(entry: &str, appdir: &str) -> bool {
    entry.starts_with(appdir) || entry.starts_with("/tmp/.mount_")
}

/// Whether this process is running from an AppImage (its `AppRun` set `APPDIR`).
fn appdir() -> Option<String> {
    std::env::var("APPDIR").ok().filter(|s| !s.is_empty())
}

/// Compute the `(key, value-or-None)` env overrides to apply to a spawned child
/// so it runs in the user's real desktop environment rather than inheriting
/// Lychi's AppImage sandbox env. `None` value = unset the variable.
///
/// Pure and parameterised over the current env (`get`) so it is testable without
/// touching the real process environment. Returns an EMPTY list when not running
/// from an AppImage — the caller then spawns with the inherited env unchanged.
pub fn desktop_env_overrides(
    get: impl Fn(&str) -> Option<String>,
) -> Vec<(String, Option<String>)> {
    let Some(appdir) = get("APPDIR").filter(|s| !s.is_empty()) else {
        return Vec::new();
    };

    let mut overrides: Vec<(String, Option<String>)> = Vec::new();

    // Set-fresh vars: unset entirely.
    for &var in APPIMAGE_SET_VARS {
        overrides.push((var.to_string(), None));
    }

    // Prepended vars: keep only the non-AppImage entries.
    for &var in APPIMAGE_PREPENDED_VARS {
        let Some(value) = get(var) else { continue };
        let kept: Vec<&str> = value
            .split(':')
            .filter(|e| !e.is_empty() && !is_appimage_path(e, &appdir))
            .collect();
        overrides.push((
            var.to_string(),
            if kept.is_empty() {
                None
            } else {
                Some(kept.join(":"))
            },
        ));
    }

    overrides
}

/// Apply [`desktop_env_overrides`] to a `std::process::Command`, reading from the
/// live process environment. Call before spawning any external program that is
/// NOT part of Lychi (a terminal, a browser, an editor, a package manager) so it
/// runs in the user's real environment. A no-op outside an AppImage.
pub fn sanitize_command(command: &mut std::process::Command) {
    if appdir().is_none() {
        return;
    }
    for (key, value) in desktop_env_overrides(|k| std::env::var(k).ok()) {
        match value {
            Some(v) => {
                command.env(&key, v);
            }
            None => {
                command.env_remove(&key);
            }
        }
    }
}

/// [`sanitize_command`] for `tokio::process::Command`.
pub fn sanitize_tokio_command(command: &mut tokio::process::Command) {
    if appdir().is_none() {
        return;
    }
    for (key, value) in desktop_env_overrides(|k| std::env::var(k).ok()) {
        match value {
            Some(v) => {
                command.env(&key, v);
            }
            None => {
                command.env_remove(&key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn not_an_appimage_makes_no_changes() {
        // No APPDIR → the whole thing is a no-op, even with GTK vars present.
        let get = env_from(&[("GTK_PATH", "/tmp/.mount_x/usr/lib64/gtk-3.0")]);
        assert!(desktop_env_overrides(get).is_empty());
    }

    #[test]
    fn set_fresh_appimage_vars_are_removed() {
        let get = env_from(&[
            ("APPDIR", "/tmp/.mount_Lychi_ab12"),
            ("GTK_PATH", "/tmp/.mount_Lychi_ab12/usr/lib64/gtk-3.0"),
            ("GDK_BACKEND", "x11"),
            (
                "GIO_EXTRA_MODULES",
                "/tmp/.mount_Lychi_ab12/usr/lib/gio/modules",
            ),
        ]);
        let overrides = desktop_env_overrides(get);
        // Every set-fresh var must be present with a None (unset) instruction.
        for var in ["APPDIR", "GTK_PATH", "GDK_BACKEND", "GIO_EXTRA_MODULES"] {
            assert!(
                overrides.iter().any(|(k, v)| k == var && v.is_none()),
                "{var} must be unset for the child"
            );
        }
    }

    #[test]
    fn ld_library_path_keeps_only_non_appimage_entries() {
        let appdir = "/tmp/.mount_Lychi_ab12";
        let get = env_from(&[
            ("APPDIR", appdir),
            (
                "LD_LIBRARY_PATH",
                "/tmp/.mount_Lychi_ab12/usr/lib:/tmp/.mount_Lychi_ab12/lib64:/usr/local/lib",
            ),
        ]);
        let overrides = desktop_env_overrides(get);
        let ld = overrides
            .iter()
            .find(|(k, _)| k == "LD_LIBRARY_PATH")
            .expect("LD_LIBRARY_PATH handled");
        assert_eq!(
            ld.1.as_deref(),
            Some("/usr/local/lib"),
            "only the system entry survives"
        );
    }

    #[test]
    fn an_all_appimage_ld_path_is_unset_not_left_empty() {
        // Lychi's real LD_LIBRARY_PATH is entirely mount paths — after stripping,
        // nothing remains, so the var must be UNSET (an empty LD_LIBRARY_PATH has
        // a surprising meaning: search the cwd).
        let get = env_from(&[
            ("APPDIR", "/tmp/.mount_Lychi_ab12"),
            (
                "LD_LIBRARY_PATH",
                "/tmp/.mount_Lychi_ab12/usr/lib/:/tmp/.mount_Lychi_ab12/usr/lib64/:",
            ),
        ]);
        let overrides = desktop_env_overrides(get);
        let ld = overrides
            .iter()
            .find(|(k, _)| k == "LD_LIBRARY_PATH")
            .expect("LD_LIBRARY_PATH handled");
        assert!(ld.1.is_none(), "an all-AppImage LD_LIBRARY_PATH is unset");
    }

    #[test]
    fn xdg_data_dirs_keeps_the_system_and_flatpak_entries() {
        let appdir = "/tmp/.mount_Lychi_ab12";
        let get = env_from(&[
            ("APPDIR", appdir),
            (
                "XDG_DATA_DIRS",
                "/tmp/.mount_Lychi_ab12/usr/share/:/tmp/.mount_Lychi_ab12/usr/share:/usr/share:/var/lib/flatpak/exports/share",
            ),
        ]);
        let overrides = desktop_env_overrides(get);
        let xdg = overrides
            .iter()
            .find(|(k, _)| k == "XDG_DATA_DIRS")
            .expect("XDG_DATA_DIRS handled");
        assert_eq!(
            xdg.1.as_deref(),
            Some("/usr/share:/var/lib/flatpak/exports/share"),
            "the user's real data dirs survive; the mount ones are dropped"
        );
    }
}
