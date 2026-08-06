//! Register the global hotkey in the desktop environment's own settings.
//!
//! This is the fallback for desktops that neither implement the XDG
//! GlobalShortcuts portal (GNOME, KDE do; XFCE, Cinnamon, MATE, Budgie do not)
//! nor let an X11 key-grab win against the window manager.
//!
//! Why a key-grab is not enough on X11: an `XGrabKey` cannot override a
//! combination the desktop environment already owns. The grab *succeeds* — so
//! `tauri-plugin-global-shortcut` returns `Ok(())` and we log "registered" — but
//! the WM keeps delivering the key to itself and the launcher never opens. That
//! is what a tester hit on XFCE with `Super+Space`: silence, and a log line
//! claiming success. Writing the binding into the DE's own config is the only
//! way to be the owner rather than a competing grabber.
//!
//! This is what Ulauncher v6 does, having moved off `libkeybinder` for exactly
//! this reason.
//!
//! ## Deliberate constraints
//!
//! - **Never overwrite a binding that is not ours.** If the combination is
//!   already taken by something else, we report the conflict and leave it —
//!   silently stealing a user's shortcut is worse than not having one.
//! - **Idempotent.** Re-running on every launch must not accumulate duplicate
//!   entries, so an existing Lychi binding is updated in place.
//! - **Never fatal.** Every failure path logs and returns; the launcher still
//!   works via `lychi --toggle`, the tray, and the CLI.

use std::process::Command;

/// The command a desktop shortcut should run. Goes through the CLI so it pokes
/// the already-running instance over its socket rather than starting a second
/// copy.
const TOGGLE_CMD: &str = "lychi --toggle";

/// How the binding was left after an attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// We own the binding now (either freshly written or already correct).
    Registered,
    /// The combination belongs to something else; left untouched.
    Conflict(String),
    /// This desktop has no integration here.
    Unsupported,
    /// The desktop's own tooling failed.
    Failed(String),
}

/// Desktops we can write a shortcut into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desktop {
    Xfce,
    Gnome,
    Kde,
    Cinnamon,
    Mate,
}

/// Identify the running desktop, for the subset we can write a shortcut into.
///
/// Delegates to `context::session`, which is the one detector. This used to be
/// a second implementation that matched substrings of `XDG_CURRENT_DESKTOP`
/// *or* `XDG_SESSION_DESKTOP` — and the latter holds the systemd session **file
/// name**, not a desktop name (`plasma`, not `KDE`), so it could answer for a
/// variable that never carries the answer. It also matched "PLASMA", which no
/// desktop sets. Both traps are documented in `context/session.rs`.
pub fn detect() -> Option<Desktop> {
    from_desktops(&lychi_core::context::session::session().desktops)
}

/// Map a parsed `XDG_CURRENT_DESKTOP` chain onto the desktops we can write to.
///
/// Pure, and separate from [`detect`] on purpose: `session()` caches in a
/// `OnceLock`, so a test that sets the environment and calls `detect` asserts
/// nothing after the first call in the process. Taking the chain as an argument
/// is the only way to test the mapping honestly.
fn from_desktops(chain: &[lychi_core::context::session::Desktop]) -> Option<Desktop> {
    use lychi_core::context::session::Desktop as D;
    // In chain order: `Budgie:GNOME` means "I am Budgie, treat me as GNOME if
    // you must", so the first component we recognise wins.
    for d in chain {
        match d {
            D::Xfce => return Some(Desktop::Xfce),
            D::Cinnamon => return Some(Desktop::Cinnamon),
            D::Mate => return Some(Desktop::Mate),
            D::Kde => return Some(Desktop::Kde),
            // Budgie and Unity are GNOME shells; their settings live in the
            // same GSettings path the GNOME writer uses.
            D::Gnome | D::GnomeClassic | D::GnomeFlashback | D::Budgie | D::Unity => {
                return Some(Desktop::Gnome);
            }
            _ => {}
        }
    }
    None
}

/// Translate a Tauri accelerator (`Super+Space`, `CmdOrCtrl+Alt+K`) into the
/// GTK-style form XFCE/GNOME/Cinnamon/MATE expect (`<Super>space`).
///
/// Kept as a pure function because the whole feature turns on getting this
/// string exactly right: a malformed accelerator is accepted by the config
/// system and simply never fires, which is indistinguishable from the bug this
/// module exists to fix.
pub fn to_gtk_accel(accel: &str) -> Option<String> {
    let mut mods = String::new();
    let mut key = None;
    for part in accel.split('+') {
        let p = part.trim();
        if p.is_empty() {
            return None;
        }
        match p.to_ascii_lowercase().as_str() {
            "super" | "meta" | "cmd" | "command" | "cmdorctrl" | "commandorcontrol" => {
                // CmdOrCtrl means Ctrl on Linux, not Super.
                if p.to_ascii_lowercase().starts_with("cmdorctrl")
                    || p.to_ascii_lowercase().starts_with("commandorcontrol")
                {
                    mods.push_str("<Primary>");
                } else {
                    mods.push_str("<Super>");
                }
            }
            "ctrl" | "control" => mods.push_str("<Primary>"),
            "alt" | "option" => mods.push_str("<Alt>"),
            "shift" => mods.push_str("<Shift>"),
            other => {
                // The last non-modifier token is the key itself. GTK wants
                // lowercase names for named keys (`space`, `return`) and bare
                // letters as-is.
                key = Some(other.to_string());
            }
        }
    }
    let key = key?;
    if mods.is_empty() {
        // A bare key with no modifier is almost never what a user wants for a
        // global shortcut, and several DEs refuse it outright.
        return None;
    }
    Some(format!("{mods}{key}"))
}

/// Write the hotkey into the desktop's settings, if we know how.
///
/// Called only when the portal path is unavailable or a key-grab cannot be
/// trusted. Returns what it did so the caller can log one honest line rather
/// than claiming success on every path.
pub fn register(accel: &str) -> Outcome {
    match detect() {
        Some(Desktop::Xfce) => xfce(accel),
        // GNOME/Cinnamon/MATE all use the same gsettings custom-keybinding
        // shape under different schema prefixes.
        Some(Desktop::Gnome) => gsettings_based(
            accel,
            "org.gnome.settings-daemon.plugins.media-keys",
            "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/",
        ),
        Some(Desktop::Cinnamon) => gsettings_based(
            accel,
            "org.cinnamon.desktop.keybindings",
            "/org/cinnamon/desktop/keybindings/custom-keybindings/",
        ),
        Some(Desktop::Mate) => Outcome::Unsupported,
        // KDE implements the portal, which is a better path (it asks the user).
        // Only reached if the portal failed, and rewriting kglobalshortcutsrc
        // behind kglobalaccel's back needs a daemon reload to take effect, so
        // we do not attempt it.
        Some(Desktop::Kde) => Outcome::Unsupported,
        None => Outcome::Unsupported,
    }
}

/// Remove a binding we previously wrote for `accel`.
///
/// Must be called before writing a new one when the user changes their hotkey in
/// Settings. Without it, the OLD combination stays in the desktop's config and
/// keeps launching Lychi — so a user who rebinds `Super+Space` to `Ctrl+Space`
/// ends up with both working and no way to tell why.
///
/// Only ever removes a binding whose command is ours; a shortcut that has since
/// been repointed at something else is left alone.
pub fn unregister(accel: &str) {
    let Some(desktop) = detect() else { return };
    let Some(gtk) = to_gtk_accel(accel) else {
        return;
    };
    match desktop {
        Desktop::Xfce => {
            let prop = format!("/commands/custom/{gtk}");
            // Confirm it is still ours before deleting.
            let owned = Command::new("xfconf-query")
                .args(["-c", "xfce4-keyboard-shortcuts", "-p", &prop])
                .output()
                .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("lychi"))
                .unwrap_or(false);
            if owned {
                let _ = Command::new("xfconf-query")
                    .args(["-c", "xfce4-keyboard-shortcuts", "-p", &prop, "-r"])
                    .output();
            }
        }
        // GNOME/Cinnamon reuse one fixed path (`.../lychi/`) rather than a
        // per-accelerator one, so changing the hotkey overwrites the `binding`
        // key in place — there is no stale entry to clean up.
        Desktop::Gnome | Desktop::Cinnamon => {}
        Desktop::Kde | Desktop::Mate => {}
    }
}

/// XFCE: `xfce4-keyboard-shortcuts` channel, `/commands/custom/<accel>`.
///
/// xfconf applies changes live — `xfsettingsd` watches the channel — so the
/// binding works without a logout.
///
/// Note the accelerator is part of the property PATH here, unlike the gsettings
/// desktops: each combination is its own key, so rebinding leaves the old one
/// behind unless [`unregister`] removes it.
fn xfce(accel: &str) -> Outcome {
    let Some(gtk) = to_gtk_accel(accel) else {
        return Outcome::Failed(format!("could not translate accelerator {accel:?}"));
    };
    let prop = format!("/commands/custom/{gtk}");

    // Is anything already bound here?
    let existing = Command::new("xfconf-query")
        .args(["-c", "xfce4-keyboard-shortcuts", "-p", &prop])
        .output();
    if let Ok(out) = existing
        && out.status.success()
    {
        let current = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if current.contains("lychi") {
            return Outcome::Registered; // already ours, nothing to do
        }
        if !current.is_empty() {
            return Outcome::Conflict(current);
        }
    }

    let res = Command::new("xfconf-query")
        .args([
            "-c",
            "xfce4-keyboard-shortcuts",
            "-p",
            &prop,
            "--create",
            "-t",
            "string",
            "-s",
            TOGGLE_CMD,
        ])
        .output();
    match res {
        Ok(o) if o.status.success() => Outcome::Registered,
        Ok(o) => Outcome::Failed(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// GNOME/Cinnamon: a relocatable `custom-keybinding` schema, plus the path
/// appended to the `custom-keybindings` list.
///
/// The two-part shape is why this cannot be a single `gsettings set`: the
/// binding lives at a relocatable path, and the desktop only reads paths that
/// are also listed in the parent array.
fn gsettings_based(accel: &str, schema: &str, path_prefix: &str) -> Outcome {
    let Some(gtk) = to_gtk_accel(accel) else {
        return Outcome::Failed(format!("could not translate accelerator {accel:?}"));
    };

    let list = match Command::new("gsettings")
        .args(["get", schema, "custom-keybindings"])
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Ok(o) => return Outcome::Failed(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => return Outcome::Failed(e.to_string()),
    };

    // Reuse our own slot if we already have one, so repeated launches do not
    // append `lychi0`, `lychi1`, ... forever.
    let our_path = format!("{path_prefix}lychi/");
    let child = format!("{schema}.custom-keybinding:{our_path}");

    let set = |key: &str, val: &str| {
        Command::new("gsettings")
            .args(["set", &child, key, val])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !set("name", "Lychi") || !set("command", TOGGLE_CMD) || !set("binding", &gtk) {
        return Outcome::Failed("gsettings set failed".into());
    }

    if !list.contains(&our_path) {
        // `@as []` is the empty-array form gsettings prints; treat it as empty.
        let mut paths: Vec<String> = if list.starts_with("@as") || list == "[]" {
            Vec::new()
        } else {
            list.trim_matches(['[', ']'].as_ref())
                .split(',')
                .map(|s| s.trim().trim_matches('\'').to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        paths.push(our_path);
        let joined = format!(
            "[{}]",
            paths
                .iter()
                .map(|p| format!("'{p}'"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        match Command::new("gsettings")
            .args(["set", schema, "custom-keybindings", &joined])
            .output()
        {
            Ok(o) if o.status.success() => {}
            Ok(o) => return Outcome::Failed(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => return Outcome::Failed(e.to_string()),
        }
    }
    Outcome::Registered
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The accelerator translation is the whole feature: a malformed string is
    /// accepted by every config system here and then simply never fires, which
    /// looks exactly like the bug this module fixes.
    #[test]
    fn gtk_accelerators_translate() {
        assert_eq!(to_gtk_accel("Super+Space").as_deref(), Some("<Super>space"));
        assert_eq!(
            to_gtk_accel("Ctrl+Space").as_deref(),
            Some("<Primary>space")
        );
        assert_eq!(
            to_gtk_accel("Super+Shift+K").as_deref(),
            Some("<Super><Shift>k")
        );
        assert_eq!(to_gtk_accel("Alt+F2").as_deref(), Some("<Alt>f2"));
    }

    /// `CmdOrCtrl` is Tauri's cross-platform token. On Linux it means Ctrl —
    /// mapping it to Super would bind a different key than the user asked for.
    #[test]
    fn cmd_or_ctrl_is_control_not_super() {
        assert_eq!(
            to_gtk_accel("CmdOrCtrl+Space").as_deref(),
            Some("<Primary>space")
        );
    }

    /// A modifier-less accelerator is refused rather than written: several DEs
    /// reject it, and a bare letter as a global shortcut would swallow typing.
    #[test]
    fn a_bare_key_is_refused() {
        assert_eq!(to_gtk_accel("Space"), None);
    }

    #[test]
    fn malformed_accelerators_are_refused() {
        assert_eq!(to_gtk_accel(""), None);
        assert_eq!(to_gtk_accel("Super+"), None);
        assert_eq!(to_gtk_accel("Super++Space"), None);
        // Modifiers only, no key.
        assert_eq!(to_gtk_accel("Super+Shift"), None);
    }

    /// The desktop chain maps onto the writers we have.
    ///
    /// Tests `from_desktops`, not `detect`: `session()` caches in a `OnceLock`,
    /// so the previous version of this test set `XDG_CURRENT_DESKTOP` in a loop
    /// and would have asserted the same cached answer six times.
    #[test]
    fn desktop_chains_map_to_the_right_writer() {
        use lychi_core::context::session::Desktop as D;
        let cases: [(&[D], Option<Desktop>); 7] = [
            (&[D::Xfce], Some(Desktop::Xfce)),
            (&[D::Gnome], Some(Desktop::Gnome)),
            (&[D::Cinnamon], Some(Desktop::Cinnamon)),
            (&[D::Kde], Some(Desktop::Kde)),
            // "I am Budgie, treat me as GNOME if you must" — either component
            // reaches the GNOME writer.
            (&[D::Budgie, D::Gnome], Some(Desktop::Gnome)),
            // A GNOME variant that does not literally say GNOME first.
            (&[D::GnomeFlashback, D::Gnome], Some(Desktop::Gnome)),
            // wlroots compositors have no settings store we can write to.
            (&[D::Sway], None),
        ];
        for (chain, want) in cases {
            assert_eq!(from_desktops(chain), want, "for {chain:?}");
        }
    }

    #[test]
    fn the_first_recognised_component_wins() {
        use lychi_core::context::session::Desktop as D;
        // The chain is ordered specific→generic, so a session claiming both
        // must be written where it primarily is.
        assert_eq!(
            from_desktops(&[D::Cinnamon, D::Gnome]),
            Some(Desktop::Cinnamon)
        );
        // An unrecognised leading component must not stop the search.
        assert_eq!(from_desktops(&[D::Other, D::Kde]), Some(Desktop::Kde));
    }
}
