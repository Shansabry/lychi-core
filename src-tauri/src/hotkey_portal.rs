//! Global hotkey via the XDG GlobalShortcuts portal (Wayland).
//!
//! Wayland forbids client-side key grabs; the sanctioned mechanism is
//! `org.freedesktop.portal.GlobalShortcuts`: the app asks the compositor to
//! bind a trigger, the user confirms once in a DE dialog (KDE Plasma ≥5.27,
//! GNOME ≥48, Hyprland), and the approval persists per app-id across
//! restarts. Every launch still calls BindShortcuts — the binding attaches to
//! the session, not the app-id (see `run`) — but the stored approval makes
//! every bind after the first silent.
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

/// How a servicing `run` came to an end. `run` blocks for the app's lifetime
/// while healthy, so every variant is a reason it stopped — a typed outcome
/// rather than a bare `Ok(())`, because a bare Ok is exactly what the old
/// `setup` misread as "session closed cleanly — done": a portal crash/restart
/// (routine over a long session — package upgrades, OOM, `systemctl --user
/// restart`) ended the stream, `setup` returned, and the hotkey stayed dead
/// until app restart while the stored verdict kept saying Reliable.
enum RunOutcome {
    /// The user (or compositor) declined the bind. Retrying would re-prompt —
    /// respect the answer and leave the fallback UX in place.
    Declined,
    /// The activation stream ended after a successful bind: the portal side
    /// went away. Re-registration is warranted (and prompt-free — the stored
    /// approval carries).
    StreamEnded,
}

/// Delay before re-registering after a stream death, so we bind to the portal
/// that replaces the dying one rather than racing its shutdown.
const REREGISTER_DELAY_SECS: u64 = 2;

/// Bind the toggle hotkey through the portal and service its activations.
/// Runs for the app's lifetime; all failures are non-fatal (fallback UX).
///
/// Two nested loops, two different failure shapes:
/// - the INNER schedule retries a registration that has not succeeded yet (an
///   autostart race against `xdg-desktop-portal` coming up self-heals; a
///   compositor with no backend at all exhausts the schedule and gives up);
/// - the OUTER loop re-enters registration when a previously WORKING session
///   dies (portal crash/restart). Each re-entry gets a fresh schedule: a
///   session that bound once proves the backend exists, so the give-up
///   reasoning of the inner loop does not transfer. `run` records the
///   downgraded verdict before returning, so diagnostics tell the truth
///   during the gap.
pub async fn setup(app: tauri::AppHandle, configured_hotkey: String) {
    loop {
        match register_cycle(&app, &configured_hotkey).await {
            CycleOutcome::Done => return,
            CycleOutcome::StreamEnded => {
                tokio::time::sleep(std::time::Duration::from_secs(REREGISTER_DELAY_SECS)).await;
            }
        }
    }
}

enum CycleOutcome {
    /// Nothing left to do: declined, or the schedule exhausted with no bind.
    Done,
    /// A working session died — re-register from scratch.
    StreamEnded,
}

/// One pass through the retry schedule. Extracted from `setup` so the loop
/// there states the actual lifecycle (register → serve → maybe re-register)
/// instead of interleaving it with backoff bookkeeping.
async fn register_cycle(app: &tauri::AppHandle, configured_hotkey: &str) -> CycleOutcome {
    for (attempt, &delay) in RETRY_SCHEDULE_SECS.iter().enumerate() {
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        }
        match run(app, configured_hotkey).await {
            Ok(RunOutcome::Declined) => return CycleOutcome::Done,
            Ok(RunOutcome::StreamEnded) => return CycleOutcome::StreamEnded,
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
                    // that line repeat on a 1/2/4/8/15s backoff, and without a
                    // reason attached that reads as the app malfunctioning.
                    //
                    // This message used to assert the cause was another running
                    // Lychi holding the shortcut. That guess was wrong and it
                    // cost real debugging time: the actual failure was our own
                    // connection bug (see `register_host_app_id`), and the
                    // confident-but-false explanation in the log actively
                    // pointed away from it. Report the error; don't speculate.
                    tracing::warn!(
                        "[portal] attempt {}/{} failed ({e}) — retrying",
                        attempt + 1,
                        RETRY_SCHEDULE_SECS.len()
                    );
                }
            }
        }
    }
    CycleOutcome::Done
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
///
/// CRITICAL — the connection is the identity. The interface XML is explicit:
/// "lets unsandboxed applications register their **D-Bus connections**" and
/// "Register a **D-Bus peer**". The app-id is bound to the peer that called
/// `Register`, NOT to the process. Registering on a throwaway connection and
/// then creating the session on a different one leaves the session's peer
/// anonymous, and the portal rejects it with the exact
/// `NotAllowed: An app id is required` this function exists to prevent.
/// That was a real bug: we opened `Connection::session()` here, registered,
/// dropped it at the end of the scope, and let ashpd open its own — so every
/// attempt logged a successful registration followed by a rejected
/// CreateSession, nine times, and the hotkey silently never bound.
///
/// So the caller owns ONE connection, registers it here, and hands that same
/// connection to `GlobalShortcuts::with_connection`. Also note the spec's
/// "Registering can only [be] done at most once; any subsequent call will
/// result in an error" — a fresh connection per retry keeps that true.
async fn register_host_app_id(conn: &ashpd::zbus::Connection) -> Result<(), String> {
    use ashpd::zbus;
    let attempt = async {
        let proxy = zbus::Proxy::new(
            conn,
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

async fn run(app: &tauri::AppHandle, configured_hotkey: &str) -> ashpd::Result<RunOutcome> {
    // ONE connection, used for both the handshake and the session — the portal
    // binds the app-id to the calling peer, so these cannot be separate
    // connections. See `register_host_app_id` for what happens when they are.
    //
    // Built per attempt rather than cached process-wide: `Register` is
    // once-per-peer, so a retry after a failure needs a peer that has not
    // already registered.
    let conn = ashpd::zbus::Connection::session()
        .await
        .map_err(ashpd::Error::from)?;

    // Identify ourselves to the portal FIRST (required on xdg-desktop-portal
    // ≥1.21, otherwise CreateSession is rejected with "An app id is required").
    // If this fails (portal not up yet at autostart), bail so the retry loop
    // tries again — rather than proceeding to a guaranteed CreateSession reject.
    if let Err(e) = register_host_app_id(&conn).await {
        return Err(ashpd::Error::Portal(ashpd::PortalError::Failed(format!(
            "host app-id registration not ready: {e}"
        ))));
    }

    let shortcuts = GlobalShortcuts::with_connection(conn).await?;
    let session = shortcuts.create_session(Default::default()).await?;

    // BindShortcuts EVERY launch — this is load-bearing, not politeness.
    // Shortcuts are bound to the SESSION: on Plasma ≥6.7 (kglobalaccel lives
    // inside KWin) a fresh session is inert until it binds, while
    // ListShortcuts happily reports the binding stored for our app-id. An
    // earlier version skipped the bind when ListShortcuts found one, which
    // left the hotkey dead after every app restart — with this very function
    // logging "bound and works". The stored user approval makes the re-bind
    // silent; only the genuinely first bind may show a dialog.
    let existing = shortcuts
        .list_shortcuts(&session, Default::default())
        .await?
        .response()
        .map(|r| r.shortcuts().to_vec())
        .unwrap_or_default();
    let first_bind = !existing.iter().any(|s| s.id() == SHORTCUT_ID);

    let preferred = to_portal_trigger(configured_hotkey);
    if first_bind {
        tracing::info!(
            "[portal] binding '{SHORTCUT_ID}' (preferred trigger: {preferred}) — the desktop may show a confirmation dialog"
        );
    } else {
        tracing::info!(
            "[portal] re-binding stored '{SHORTCUT_ID}' to activate it for this session"
        );
    }
    let new_shortcut = NewShortcut::new(SHORTCUT_ID, "Show or hide the Lychi launcher")
        .preferred_trigger(preferred.as_str());
    let trigger = shortcuts
        .bind_shortcuts(&session, &[new_shortcut], None, Default::default())
        .await?
        .response()?
        .shortcuts()
        .iter()
        .find(|s| s.id() == SHORTCUT_ID)
        .map(|s| s.trigger_description().to_string())
        .unwrap_or_default();

    if trigger.is_empty() {
        tracing::warn!("[portal] shortcut not bound (user declined or compositor refused)");
        return Ok(RunOutcome::Declined);
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
    // The compositor routes the key to us; nothing further needs proving.
    crate::record_hotkey_binding(app, lychi_core::hotkey::Binding::Portal);
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

    // The stream only ends because the portal side went away. The hotkey is
    // dead from this instant until re-registration succeeds, and everything
    // that answers "does the hotkey work?" must say so NOW — the caller will
    // retry, but a Setup tab read during the gap (or after a failed retry
    // cycle) must not repeat the stored "Reliable" from a session that no
    // longer exists.
    {
        use std::sync::atomic::Ordering;
        let state = app.state::<AppState>();
        state.portal_bound.store(false, Ordering::SeqCst);
        state.hotkey_registered.store(false, Ordering::SeqCst);
    }
    crate::record_hotkey_binding(app, lychi_core::hotkey::Binding::PortalLost);
    tracing::warn!("[portal] activation stream ended — the portal side went away; re-registering");
    Ok(RunOutcome::StreamEnded)
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
