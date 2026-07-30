//! Global hotkey via the XDG GlobalShortcuts portal (Wayland).
//!
//! Wayland forbids client-side key grabs; the sanctioned mechanism is
//! `org.freedesktop.portal.GlobalShortcuts`: the app asks the compositor to
//! bind a trigger, the user confirms once in a DE dialog (KDE Plasma ≥5.27,
//! GNOME ≥48, Hyprland), and the binding persists per app-id across
//! restarts — later launches find it via ListShortcuts and never re-prompt.
//!
//! Compositors without the portal backend (Sway/wlroots, COSMic, GNOME ≤47)
//! fail at `GlobalShortcuts::new()` / bind — we log and leave the existing
//! fallback UX in place (first-run banner + Settings hint pointing at a
//! DE-bound `lychi --toggle`).

use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;
use tauri::Manager;

use crate::state::AppState;

const SHORTCUT_ID: &str = "toggle-launcher";

/// Ensure a desktop file NAMED AFTER THE APP-ID (`app.lychi.desktop`) exists in
/// the user's applications dir. The XDG GlobalShortcuts portal (≥1.21) requires
/// our app-id registration to be backed by an installed `<app-id>.desktop`;
/// without it the hotkey fails on Wayland with "An app id is required".
///
/// The RPM/deb install this system-wide, but an AppImage installs NOTHING — it's
/// a portable blob — so the app self-registers here on startup. Idempotent:
/// writes only if absent, points at the CURRENT executable, refreshes the
/// desktop DB. Best-effort; any failure just means the portal path may not work
/// (the fallback UX still applies).
pub fn ensure_app_desktop_file() {
    let Some(dir) = dirs::data_dir() else { return };
    let apps = dir.join("applications");
    let target = apps.join(format!("{APP_ID}.desktop"));
    if target.exists() {
        return;
    }
    // Point Exec at our own binary (the AppImage path via $APPIMAGE if set, else
    // the resolved exe). `%u` lets the deep-link handler reuse it too.
    let exec = std::env::var("APPIMAGE")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .map(|p| p.display().to_string())
        })
        .unwrap_or_else(|| "lychi-app".to_string());

    let contents = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Lychi\n\
         Comment=Local-first keyboard command launcher\n\
         Exec={exec} %u\n\
         Icon=lychi-app\n\
         Terminal=false\n\
         Categories=Utility;\n\
         StartupWMClass=lychi-app\n\
         MimeType=x-scheme-handler/lychi;\n"
    );

    if std::fs::create_dir_all(&apps).is_err() {
        return;
    }
    match std::fs::write(&target, contents) {
        Ok(()) => {
            tracing::info!("[portal] wrote app-id desktop file: {}", target.display());
            // Refresh the desktop database so the portal sees it immediately.
            let _ = std::process::Command::new("update-desktop-database")
                .arg(&apps)
                .status();
        }
        Err(e) => tracing::debug!("[portal] could not write app-id desktop file ({e})"),
    }
}

/// How long to keep retrying the portal before concluding the compositor has no
/// GlobalShortcuts backend. On AUTOSTART, Lychi often launches before
/// `xdg-desktop-portal` (and its backend) finish coming up, so the first attempts
/// fail transiently — we back off and retry until the portal appears on the bus.
/// Bounded so a compositor that genuinely lacks the backend (Sway/wlroots,
/// GNOME ≤47) eventually gives up and leaves the fallback UX in place.
const RETRY_SCHEDULE_SECS: &[u64] = &[0, 1, 2, 4, 8, 15, 30, 30, 30];

/// Bind the toggle hotkey through the portal and service its activations.
/// Runs for the app's lifetime; all failures are non-fatal (fallback UX).
///
/// Retries with backoff so an autostart race against `xdg-desktop-portal` (the
/// backend not yet ready when we boot) self-heals: we keep trying until the
/// portal is available, then bind + service activations for the process lifetime.
pub async fn setup(app: tauri::AppHandle, configured_hotkey: String) {
    for (attempt, &delay) in RETRY_SCHEDULE_SECS.iter().enumerate() {
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        }
        // If a prior attempt already bound + is servicing (its `run` returned Ok
        // only when the session ends), we won't re-enter. `run` blocks for the
        // app lifetime on success, so reaching here again means it failed.
        match run(&app, &configured_hotkey).await {
            Ok(()) => {
                // The activation stream ended cleanly (session closed) — done.
                return;
            }
            Err(e) => {
                let last = attempt + 1 == RETRY_SCHEDULE_SECS.len();
                if last {
                    tracing::warn!(
                        "[portal] GlobalShortcuts unavailable after {} attempts ({e}) — bind `lychi --toggle` in your desktop's shortcut settings",
                        RETRY_SCHEDULE_SECS.len()
                    );
                } else {
                    // WARN, not DEBUG. A retry logs "[portal] registered host
                    // app-id" on the way in, so at default level the user sees
                    // that line repeat on a 1/2/4/8/15s backoff with no reason
                    // attached — which reads as the app malfunctioning when the
                    // usual cause is mundane: another Lychi (autostart) already
                    // holds the shortcut, so this instance's CreateSession is
                    // refused. The symptom was visible and the cause was not.
                    tracing::warn!(
                        "[portal] attempt {}/{} failed ({e}) — retrying. If another \
                         Lychi is already running (autostart), it owns the hotkey and \
                         this one does not need it.",
                        attempt + 1,
                        RETRY_SCHEDULE_SECS.len()
                    );
                }
            }
        }
    }
}

/// The reverse-DNS app-id, matching the bundle identifier + the installed
/// `.desktop`. Required by the host-registry handshake (below).
const APP_ID: &str = "app.lychi";

/// Announce our app-id to the portal via the host-registry handshake, returning
/// whether the interface is present (true) or genuinely absent (false).
///
/// xdg-desktop-portal ≥1.21 REMOVED the old "derive the app-id from the systemd
/// scope" path and now HARD-REJECTS GlobalShortcuts `CreateSession` from a
/// non-sandboxed app that hasn't identified itself — the exact
/// `NotAllowed: An app id is required` we hit after a reboot. The fix is to call
/// `org.freedesktop.host.portal.Registry.Register(app_id, {})` on the session
/// bus BEFORE creating the shortcuts session. ashpd 0.13 doesn't wrap this new
/// method, so we call it directly over zbus.
///
/// CRITICAL for AUTOSTART: on a fresh boot the Registry interface may not be up
/// yet when we first try. If the *interface* is missing (ServiceUnknown /
/// UnknownObject) we return an Err so the caller RETRIES — proceeding to
/// CreateSession without a registered app-id would be a guaranteed reject.
/// A genuine `UnknownMethod` (older portal that never had Register) returns Ok:
/// there's nothing to register and the legacy path still works.
async fn register_host_app_id() -> Result<(), String> {
    use ashpd::zbus;
    let attempt = async {
        let conn = zbus::Connection::session().await?;
        let proxy = zbus::Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.host.portal.Registry",
        )
        .await?;
        // Register(in s app_id, in a{sv} options)
        let options: std::collections::HashMap<&str, zbus::zvariant::Value> =
            std::collections::HashMap::new();
        proxy.call_method("Register", &(APP_ID, options)).await?;
        Ok::<(), zbus::Error>(())
    }
    .await;

    match attempt {
        Ok(()) => {
            tracing::info!("[portal] registered host app-id '{APP_ID}'");
            Ok(())
        }
        // The Register METHOD doesn't exist → old portal, nothing to do, proceed.
        Err(zbus::Error::MethodError(name, _, _))
            if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod" =>
        {
            tracing::info!("[portal] host Registry.Register absent (older portal) — proceeding");
            Ok(())
        }
        // The portal/interface isn't up YET (fresh boot) → tell caller to retry.
        Err(e) => {
            tracing::warn!("[portal] host app-id registration failed ({e}) — will retry");
            Err(e.to_string())
        }
    }
}

async fn run(app: &tauri::AppHandle, configured_hotkey: &str) -> ashpd::Result<()> {
    // Identify ourselves to the portal FIRST (required on xdg-desktop-portal
    // ≥1.21, otherwise CreateSession is rejected with "An app id is required").
    // If this fails (portal not up yet at autostart), bail so the retry loop
    // tries again — rather than proceeding to a guaranteed CreateSession reject.
    if let Err(e) = register_host_app_id().await {
        return Err(ashpd::Error::Portal(ashpd::PortalError::Failed(format!(
            "host app-id registration not ready: {e}"
        ))));
    }

    let shortcuts = GlobalShortcuts::new().await?;
    let session = shortcuts.create_session(Default::default()).await?;

    // Bindings persist per app-id. Bind only if absent: BindShortcuts may
    // only be attempted once per session, and skipping it avoids any dialog
    // on subsequent launches.
    let existing = shortcuts
        .list_shortcuts(&session, Default::default())
        .await?
        .response()
        .map(|r| r.shortcuts().to_vec())
        .unwrap_or_default();

    let trigger = if let Some(known) = existing.iter().find(|s| s.id() == SHORTCUT_ID) {
        known.trigger_description().to_string()
    } else {
        let preferred = to_portal_trigger(configured_hotkey);
        tracing::info!(
            "[portal] binding '{SHORTCUT_ID}' (preferred trigger: {preferred}) — the desktop may show a confirmation dialog"
        );
        let new_shortcut = NewShortcut::new(SHORTCUT_ID, "Show or hide the Lychi launcher")
            .preferred_trigger(preferred.as_str());
        let bound = shortcuts
            .bind_shortcuts(&session, &[new_shortcut], None, Default::default())
            .await?
            .response()?;
        bound
            .shortcuts()
            .iter()
            .find(|s| s.id() == SHORTCUT_ID)
            .map(|s| s.trigger_description().to_string())
            .unwrap_or_default()
    };

    if trigger.is_empty() {
        tracing::warn!("[portal] shortcut not bound (user declined or compositor refused)");
        return Ok(());
    }

    {
        let state = app.state::<AppState>();
        state
            .hotkey_registered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        state
            .portal_bound
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    tracing::info!("[portal] global shortcut active: {trigger}");

    // Service activations for the app's lifetime. The session must stay
    // alive — dropping it unbinds the shortcut for this run.
    let mut activated = shortcuts.receive_activated().await?;
    let _session = session;
    while let Some(activation) = activated.next().await {
        if activation.shortcut_id() == SHORTCUT_ID
            && let Some(win) = app.get_webview_window("main")
        {
            tracing::debug!("[portal] activation → toggle");
            // toggle_window is thread-safe: it debounces and posts the
            // decision to the GTK main thread.
            crate::window::toggle_window(&win);
        }
    }
    tracing::warn!("[portal] activation stream ended");
    Ok(())
}

/// Map Lychi's hotkey config format ("Super+Space", "Ctrl+Alt+K") to the
/// portal trigger format from the freedesktop shortcuts spec
/// ("LOGO+space", "CTRL+ALT+k"): modifiers uppercase (Super→LOGO), key as an
/// xkbcommon keysym name.
fn to_portal_trigger(hotkey: &str) -> String {
    hotkey
        .split('+')
        .map(|part| match part.trim() {
            "Super" | "Meta" | "Cmd" => "LOGO".to_string(),
            "Ctrl" | "Control" => "CTRL".to_string(),
            "Alt" => "ALT".to_string(),
            "Shift" => "SHIFT".to_string(),
            "Space" => "space".to_string(),
            "Enter" | "Return" => "Return".to_string(),
            "Tab" => "Tab".to_string(),
            "Backspace" => "BackSpace".to_string(),
            "Delete" => "Delete".to_string(),
            "Home" => "Home".to_string(),
            "End" => "End".to_string(),
            "PageUp" => "Prior".to_string(),
            "PageDown" => "Next".to_string(),
            "Up" => "Up".to_string(),
            "Down" => "Down".to_string(),
            "Left" => "Left".to_string(),
            "Right" => "Right".to_string(),
            key if key.len() == 1 => key.to_lowercase(),
            // F1-F12 and other keysym-named keys pass through unchanged
            key => key.to_string(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

#[cfg(test)]
mod tests {
    use super::to_portal_trigger;

    #[test]
    fn maps_common_hotkeys() {
        assert_eq!(to_portal_trigger("Super+Space"), "LOGO+space");
        assert_eq!(to_portal_trigger("Ctrl+Alt+K"), "CTRL+ALT+k");
        assert_eq!(to_portal_trigger("Ctrl+Shift+F5"), "CTRL+SHIFT+F5");
        assert_eq!(to_portal_trigger("Super+Enter"), "LOGO+Return");
    }
}
