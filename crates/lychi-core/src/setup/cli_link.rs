//! Getting `lychi` onto the user's `PATH`, and keeping it there.
//!
//! An AppImage installs nothing. The `lychi` command every Settings screen
//! documents — `lychi --toggle`, `lychi --ai`, `lychi --screenshot` — does not
//! exist until something links it, and on a portal-less Wayland compositor that
//! command *is* the hotkey. So this is not a convenience; it is the fallback
//! path for the app's primary interaction.
//!
//! ## Why a symlink to the AppImage, and not a copy
//!
//! The AppImage bundles the CLI at `usr/bin/lychi` inside its mount, but that
//! mount is `/tmp/.mount_Lychi.<random>` and is unique per launch — a link there
//! dangles the moment the app exits. Copying the inner binary out survives, but
//! then goes stale on every update and silently drifts from the running app's
//! IPC protocol.
//!
//! Linking to `$APPIMAGE` — the stable path of the `.AppImage` file itself —
//! avoids both: the link keeps working after exit, and it picks up new versions
//! for free when the file is replaced in place. Dispatch then happens on
//! `argv[0]`, which is the documented AppImageKit pattern for bundling command
//! line tools, not an improvisation.
//!
//! ## Why `~/.local/bin`
//!
//! The XDG Base Directory Specification, systemd's `file-hierarchy(7)`, and
//! AppImage's own FAQ all name it as the place for user-specific executables,
//! and Zed installs its CLI exactly there. `/usr/local/bin` would need root —
//! which is precisely what makes the manual step painful enough to automate.

use std::path::{Path, PathBuf};

use super::CliStatus;

/// The command name we put on `PATH`.
const CLI_NAME: &str = "lychi";

/// Where the link lives. Not configurable: one location keeps
/// install/detect/self-heal in agreement, and a second one would be a second
/// decider about where the CLI is.
pub fn link_path() -> Option<PathBuf> {
    local_bin_dir().map(|d| d.join(CLI_NAME))
}

fn local_bin_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().join(".local").join("bin"))
}

/// The line a user would add to their shell profile. Shown, never written —
/// see [`install`].
pub fn export_line() -> String {
    "export PATH=\"$HOME/.local/bin:$PATH\"".to_string()
}

/// Is `path` a Lychi AppImage, by name?
///
/// Used to decide whether a link is ours to repoint. Deliberately conservative:
/// this governs a write, and the failure we must avoid is clobbering a link
/// somebody else created. A file named `Lychi*.AppImage` is ours; anything else
/// is left alone even if it looks plausible.
pub fn looks_like_lychi_appimage(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| {
            let lower = n.to_ascii_lowercase();
            lower.starts_with("lychi") && lower.ends_with(".appimage")
        })
        .unwrap_or(false)
}

/// Decide the CLI's state from already-gathered facts.
///
/// Pure, so every combination is testable without touching a filesystem or a
/// shell — including combinations this machine cannot produce.
///
/// `resolved_on_path` is where the user's *login shell* would find `lychi`, not
/// where this process would. They differ: a GUI app started from a `.desktop`
/// file or by D-Bus activation never sources `.bashrc`, so reading our own
/// `PATH` reports "missing" for a command the user can run perfectly well. That
/// false negative is the bug behind pip's endless "installed in
/// '~/.local/bin' which is not on PATH" complaints, and it would make Lychi
/// offer to install something that is already installed.
pub fn classify(
    install_is_appimage: bool,
    resolved_on_path: Option<PathBuf>,
    link_exists: bool,
    link_dir: &str,
) -> CliStatus {
    if let Some(found) = resolved_on_path {
        return CliStatus::OnPath {
            location: found.display().to_string(),
        };
    }
    // Nothing on PATH, but our link is there: correct, just invisible.
    if link_exists {
        return CliStatus::LinkedButUnreachable {
            dir: link_dir.to_string(),
            export_line: export_line(),
        };
    }
    if install_is_appimage {
        CliStatus::Missing
    } else {
        // A deb/rpm/AUR package puts `lychi` on PATH itself. If it is absent
        // here, that is the package's business — creating a second copy in
        // ~/.local/bin would shadow whatever the package manager later installs.
        CliStatus::ManagedBySystem
    }
}

/// What [`install`] did, so the caller can report it honestly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// Link created and the directory is on `PATH`.
    Linked { path: PathBuf },
    /// Link created, but the shell cannot see the directory yet.
    LinkedButUnreachable { path: PathBuf, export_line: String },
    /// Already present; nothing was written.
    AlreadyPresent { location: String },
}

/// Errors worth telling the user about verbatim.
#[derive(Debug)]
pub enum InstallError {
    /// Not running from an AppImage, so there is no stable file to link to.
    NotAnAppImage,
    /// `$HOME` could not be determined.
    NoHome,
    /// Something already occupies the link path and is not ours to replace.
    Occupied(PathBuf),
    Io(std::io::Error),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::NotAnAppImage => write!(
                f,
                "the `lychi` command is provided by your package manager here"
            ),
            InstallError::NoHome => write!(f, "could not determine your home directory"),
            InstallError::Occupied(p) => write!(
                f,
                "{} already exists and was not created by Lychi — remove it first",
                p.display()
            ),
            InstallError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for InstallError {
    fn from(e: std::io::Error) -> Self {
        InstallError::Io(e)
    }
}

/// Create `~/.local/bin/lychi` pointing at this AppImage.
///
/// `appimage` is `$APPIMAGE`. `on_path` is the login shell's answer, so that a
/// `lychi` already installed anywhere — including the root-owned
/// `/usr/local/bin/lychi` some users create by hand — is reported rather than
/// shadowed.
///
/// Refuses rather than overwrites. Replacing a working link the user made
/// themselves is the one genuinely destructive thing available here, and it
/// would be invisible until their own setup stopped behaving as they expected.
pub fn install(
    appimage: Option<&str>,
    on_path: Option<PathBuf>,
    dir_on_path: bool,
) -> Result<InstallOutcome, InstallError> {
    let link = link_path().ok_or(InstallError::NoHome)?;
    install_at(&link, appimage, on_path, dir_on_path)
}

/// [`install`], with the destination given rather than derived.
///
/// Split out so the filesystem behaviour — refusing a foreign file, replacing a
/// dangling link, creating a missing directory — is exercised against a real
/// scratch directory instead of the developer's actual `~/.local/bin`. Testing
/// a destructive write by performing it on the machine running the tests is not
/// a test worth having.
pub fn install_at(
    link: &Path,
    appimage: Option<&str>,
    on_path: Option<PathBuf>,
    dir_on_path: bool,
) -> Result<InstallOutcome, InstallError> {
    if let Some(found) = on_path {
        return Ok(InstallOutcome::AlreadyPresent {
            location: found.display().to_string(),
        });
    }

    let source = appimage
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(InstallError::NotAnAppImage)?;

    // symlink_metadata, not metadata: a dangling symlink must be seen as an
    // existing link rather than an absent file, or we would try to create over
    // it and fail with a confusing EEXIST.
    if let Ok(meta) = std::fs::symlink_metadata(link) {
        if !meta.file_type().is_symlink() {
            return Err(InstallError::Occupied(link.to_path_buf()));
        }
        // Ours to repoint only if it already points at a Lychi AppImage (or at
        // nothing at all, which is the stale case this exists to fix).
        match std::fs::read_link(link) {
            Ok(target) if looks_like_lychi_appimage(&target) || !target.exists() => {
                std::fs::remove_file(link)?;
            }
            _ => return Err(InstallError::Occupied(link.to_path_buf())),
        }
    }

    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::os::unix::fs::symlink(source, link)?;

    // Never report success straight from a successful syscall: a link the shell
    // cannot see is not a working command, and saying otherwise is worse than
    // the manual instructions this replaces.
    if dir_on_path {
        Ok(InstallOutcome::Linked {
            path: link.to_path_buf(),
        })
    } else {
        Ok(InstallOutcome::LinkedButUnreachable {
            path: link.to_path_buf(),
            export_line: export_line(),
        })
    }
}

/// Repoint a stale link at the current AppImage.
///
/// Runs at startup, silently. The symlink hardcodes one `.AppImage` path, and
/// users rename (`Lychi-0.1.0` → `Lychi-0.2.0`), move the file to
/// `~/Applications`, or delete it after updating — after which `lychi` dies with
/// "No such file or directory" and no hint why. That is the failure users
/// actually hit, on every update, and it is invisible until they try to use the
/// command.
///
/// Bounded so it can only ever repair its own work: only a **symlink**, only at
/// our own path, and only when the current target is absent or is itself a Lychi
/// AppImage. A regular file, or a link aimed at anything unrecognised, is left
/// untouched.
///
/// Returns the new target when something was repaired.
pub fn heal_stale_link(appimage: Option<&str>) -> Option<PathBuf> {
    let link = link_path()?;
    heal_stale_link_at(&link, appimage)
}

/// [`heal_stale_link`], with the link path given rather than derived, so the
/// repair rules can be tested against a scratch directory.
pub fn heal_stale_link_at(link: &Path, appimage: Option<&str>) -> Option<PathBuf> {
    let source = appimage.map(str::trim).filter(|s| !s.is_empty())?;

    let meta = std::fs::symlink_metadata(link).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }

    let target = std::fs::read_link(link).ok()?;
    if target == Path::new(source) {
        return None; // already correct
    }
    // Absent target = the stale case this exists for. Present-but-different is
    // an upgrade, but only when the thing being replaced is recognisably ours.
    if target.exists() && !looks_like_lychi_appimage(&target) {
        return None;
    }

    std::fs::remove_file(link).ok()?;
    std::os::unix::fs::symlink(source, link).ok()?;
    tracing::info!("repointed stale CLI link {} -> {source}", link.display());
    Some(PathBuf::from(source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_already_on_path_is_reported_where_it_was_found() {
        let status = classify(
            true,
            Some(PathBuf::from("/usr/local/bin/lychi")),
            false,
            "/home/u/.local/bin",
        );
        match status {
            CliStatus::OnPath { location } => assert_eq!(location, "/usr/local/bin/lychi"),
            other => panic!("expected OnPath, got {other:?}"),
        }
    }

    /// The state a three-state model cannot express: we did our part, the shell
    /// still cannot see it. Reporting this as "missing" would offer to redo work
    /// that is already done.
    #[test]
    fn a_link_that_exists_but_is_not_on_path_is_distinguished_from_missing() {
        let status = classify(true, None, true, "/home/u/.local/bin");
        match status {
            CliStatus::LinkedButUnreachable { dir, export_line } => {
                assert_eq!(dir, "/home/u/.local/bin");
                assert!(export_line.contains(".local/bin"));
            }
            other => panic!("expected LinkedButUnreachable, got {other:?}"),
        }
    }

    #[test]
    fn an_appimage_with_no_link_offers_to_install() {
        assert_eq!(
            classify(true, None, false, "/home/u/.local/bin"),
            CliStatus::Missing
        );
    }

    /// On deb/rpm the package owns the command. Linking our own copy would
    /// shadow whatever the package manager installs next.
    #[test]
    fn a_package_install_is_never_ours_to_fix() {
        assert_eq!(
            classify(false, None, false, "/home/u/.local/bin"),
            CliStatus::ManagedBySystem
        );
    }

    /// PATH wins over the link check: a user with a hand-made
    /// `/usr/local/bin/lychi` must read as done, not as needing our link.
    #[test]
    fn a_hand_made_link_elsewhere_still_counts_as_installed() {
        let status = classify(
            true,
            Some(PathBuf::from("/usr/local/bin/lychi")),
            true,
            "/home/u/.local/bin",
        );
        assert!(matches!(status, CliStatus::OnPath { .. }));
    }

    #[test]
    fn install_is_refused_when_the_command_already_resolves() {
        let outcome = install(
            Some("/opt/lychi/Lychi.AppImage"),
            Some(PathBuf::from("/usr/local/bin/lychi")),
            true,
        )
        .expect("already-present is not an error");
        match outcome {
            InstallOutcome::AlreadyPresent { location } => {
                assert_eq!(location, "/usr/local/bin/lychi");
            }
            other => panic!("expected AlreadyPresent, got {other:?}"),
        }
    }

    #[test]
    fn install_refuses_outside_an_appimage() {
        assert!(matches!(
            install(None, None, true),
            Err(InstallError::NotAnAppImage)
        ));
        assert!(matches!(
            install(Some("   "), None, true),
            Err(InstallError::NotAnAppImage)
        ));
    }

    /// Only files named like a Lychi AppImage are ours to replace. This governs
    /// a destructive write, so it is deliberately narrow.
    #[test]
    fn only_lychi_appimages_are_recognised_as_ours() {
        for yes in [
            "/opt/lychi/Lychi.AppImage",
            "/home/u/Applications/lychi-0.2.0.AppImage",
            "/tmp/LYCHI.APPIMAGE",
        ] {
            assert!(
                looks_like_lychi_appimage(Path::new(yes)),
                "{yes} should be recognised"
            );
        }
        for no in [
            "/usr/bin/zsh",
            "/opt/other/Krita.AppImage",
            "/opt/lychi/Lychi.AppImage.bak",
            "/home/u/lychi-notes.txt",
        ] {
            assert!(
                !looks_like_lychi_appimage(Path::new(no)),
                "{no} must not be treated as ours"
            );
        }
    }

    /// `.bak` is the specific near-miss worth pinning: the release process
    /// leaves `Lychi.AppImage.bak` next to the real one, and linking to a backup
    /// would be a silent downgrade.
    #[test]
    fn a_backup_appimage_is_not_a_link_target() {
        assert!(!looks_like_lychi_appimage(Path::new(
            "/opt/lychi/Lychi.AppImage.bak"
        )));
    }

    // ---- the writes, against a real scratch directory ----
    //
    // These exercise the destructive paths. A rule about when NOT to overwrite
    // is only worth having if something proves it holds.

    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Unique scratch dir per test, so the suite stays parallel-safe.
    fn scratch(name: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("lychi-clilink-{name}-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn touch(path: &Path) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, b"x").unwrap();
    }

    #[test]
    fn install_creates_the_link_and_the_missing_directory() {
        let dir = scratch("create");
        let img = dir.join("Lychi.AppImage");
        touch(&img);
        // Deliberately a directory that does not exist yet.
        let link = dir.join("nested").join("bin").join("lychi");

        let outcome = install_at(&link, Some(img.to_str().unwrap()), None, true).unwrap();
        assert!(matches!(outcome, InstallOutcome::Linked { .. }));
        assert_eq!(std::fs::read_link(&link).unwrap(), img);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The reachability rule: a link the shell cannot see is not a working
    /// command, so the outcome must say so even though the syscall succeeded.
    #[test]
    fn install_reports_unreachable_when_the_directory_is_not_on_path() {
        let dir = scratch("unreachable");
        let img = dir.join("Lychi.AppImage");
        touch(&img);
        let link = dir.join("bin").join("lychi");

        let outcome = install_at(&link, Some(img.to_str().unwrap()), None, false).unwrap();
        match outcome {
            InstallOutcome::LinkedButUnreachable { export_line, .. } => {
                assert!(export_line.contains(".local/bin"));
            }
            other => panic!("expected LinkedButUnreachable, got {other:?}"),
        }
        // The link is still created — we did our half of the job.
        assert!(std::fs::symlink_metadata(&link).is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The destructive case. Somebody else's file at our path is never ours to
    /// remove, and the error must name it so they can decide.
    #[test]
    fn install_refuses_to_replace_a_regular_file() {
        let dir = scratch("occupied");
        let img = dir.join("Lychi.AppImage");
        touch(&img);
        let link = dir.join("lychi");
        touch(&link); // a real file, not a symlink

        let err =
            install_at(&link, Some(img.to_str().unwrap()), None, true).expect_err("must refuse");
        assert!(matches!(err, InstallError::Occupied(_)));
        // Untouched.
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_file()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn install_refuses_to_replace_a_link_to_something_unrecognised() {
        let dir = scratch("foreign");
        let img = dir.join("Lychi.AppImage");
        touch(&img);
        let other = dir.join("Krita.AppImage");
        touch(&other);
        let link = dir.join("lychi");
        std::os::unix::fs::symlink(&other, &link).unwrap();

        let err =
            install_at(&link, Some(img.to_str().unwrap()), None, true).expect_err("must refuse");
        assert!(matches!(err, InstallError::Occupied(_)));
        assert_eq!(std::fs::read_link(&link).unwrap(), other);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A dangling link IS ours to replace — that is the stale case the whole
    /// self-heal path exists for.
    #[test]
    fn install_replaces_a_dangling_link() {
        let dir = scratch("dangling");
        let img = dir.join("Lychi.AppImage");
        touch(&img);
        let link = dir.join("lychi");
        std::os::unix::fs::symlink(dir.join("gone.AppImage"), &link).unwrap();

        let outcome = install_at(&link, Some(img.to_str().unwrap()), None, true).unwrap();
        assert!(matches!(outcome, InstallOutcome::Linked { .. }));
        assert_eq!(std::fs::read_link(&link).unwrap(), img);

        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- self-healing ----

    /// The everyday case: the AppImage was renamed by an update, so `lychi`
    /// silently stopped working until now.
    #[test]
    fn heal_repoints_a_link_whose_target_vanished() {
        let dir = scratch("heal");
        let new_img = dir.join("Lychi-0.2.0.AppImage");
        touch(&new_img);
        let link = dir.join("lychi");
        std::os::unix::fs::symlink(dir.join("Lychi-0.1.0.AppImage"), &link).unwrap();

        let healed = heal_stale_link_at(&link, Some(new_img.to_str().unwrap()));
        assert_eq!(healed, Some(new_img.clone()));
        assert_eq!(std::fs::read_link(&link).unwrap(), new_img);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Self-healing runs unattended at every launch, so its restraint matters
    /// more than its reach: a link pointing at somebody else's working file is
    /// never repointed.
    #[test]
    fn heal_leaves_a_link_to_a_foreign_target_alone() {
        let dir = scratch("heal-foreign");
        let img = dir.join("Lychi.AppImage");
        touch(&img);
        let other = dir.join("Krita.AppImage");
        touch(&other);
        let link = dir.join("lychi");
        std::os::unix::fs::symlink(&other, &link).unwrap();

        assert_eq!(heal_stale_link_at(&link, Some(img.to_str().unwrap())), None);
        assert_eq!(std::fs::read_link(&link).unwrap(), other);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn heal_never_touches_a_regular_file() {
        let dir = scratch("heal-file");
        let img = dir.join("Lychi.AppImage");
        touch(&img);
        let link = dir.join("lychi");
        touch(&link);

        assert_eq!(heal_stale_link_at(&link, Some(img.to_str().unwrap())), None);
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_file()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// No write when nothing is wrong — this runs on every launch.
    #[test]
    fn heal_does_nothing_when_the_link_is_already_correct() {
        let dir = scratch("heal-noop");
        let img = dir.join("Lychi.AppImage");
        touch(&img);
        let link = dir.join("lychi");
        std::os::unix::fs::symlink(&img, &link).unwrap();

        assert_eq!(heal_stale_link_at(&link, Some(img.to_str().unwrap())), None);
        assert_eq!(std::fs::read_link(&link).unwrap(), img);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An upgrade in place: both old and new are Lychi AppImages, so repointing
    /// is correct even though the old target still exists.
    #[test]
    fn heal_repoints_between_two_lychi_appimages() {
        let dir = scratch("heal-upgrade");
        let old_img = dir.join("Lychi-0.1.0.AppImage");
        let new_img = dir.join("Lychi-0.2.0.AppImage");
        touch(&old_img);
        touch(&new_img);
        let link = dir.join("lychi");
        std::os::unix::fs::symlink(&old_img, &link).unwrap();

        assert_eq!(
            heal_stale_link_at(&link, Some(new_img.to_str().unwrap())),
            Some(new_img.clone())
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), new_img);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn heal_is_a_no_op_outside_an_appimage() {
        let dir = scratch("heal-noimg");
        let link = dir.join("lychi");
        std::os::unix::fs::symlink(dir.join("gone"), &link).unwrap();

        assert_eq!(heal_stale_link_at(&link, None), None);
        assert_eq!(heal_stale_link_at(&link, Some("")), None);

        std::fs::remove_dir_all(&dir).ok();
    }
}
