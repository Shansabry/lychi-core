//! The Setup tab's bridge: gather the machine's state, ask `lychi_core::setup`
//! what it means, hand the answer to the frontend.
//!
//! Thin by design. Every judgement here belongs to `lychi_core::setup::assess`,
//! which is pure and tested; this module only collects the inputs that require
//! touching the world — D-Bus, the filesystem, the user's login shell, the
//! autostart plugin — and converts the verdict into wire types.
//!
//! The wire types are declared here rather than in `lychi-core` because deriving
//! `specta::Type` there would drag Tauri into a crate that is deliberately
//! testable without it. `HotkeyStatus` follows the same convention.

use lychi_core::context::doctor;
use lychi_core::install::InstallKind;
use lychi_core::setup::{self, CliStatus, StepAction, StepState};
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::state::AppState;

/// A checklist row, flattened for the frontend.
///
/// `state` is a plain string rather than a tagged union because the UI switches
/// on it directly; `detail` is whatever the backend decided to say, rendered
/// verbatim. The frontend owns no copy for these — a second copy table is a
/// place for the two sides to disagree, and this is exactly the text where
/// "not applicable" turns into shaming if it drifts.
#[derive(Serialize, specta::Type)]
pub struct SetupStepDto {
    pub id: String,
    pub title: String,
    /// "done" | "actionable" | "not_applicable"
    pub state: String,
    pub detail: String,
    /// "open_tab" | "install_cli" | "manual", absent when there is nothing to do.
    pub action: Option<String>,
    /// Which settings tab `open_tab` should open.
    pub tab: Option<String>,
    /// The command `manual` should display.
    pub command: Option<String>,
}

#[derive(Serialize, specta::Type)]
pub struct SetupChecklistDto {
    pub steps: Vec<SetupStepDto>,
    /// How many rows want attention. Drives the sidebar badge, which is hidden
    /// entirely at zero — deliberately not an "x of y" progress count.
    pub actionable: u32,
}

fn to_dto(step: &setup::SetupStep) -> SetupStepDto {
    let (state, detail, action) = match &step.state {
        StepState::Done { detail } => ("done", detail.clone(), None),
        StepState::Actionable { detail, action } => {
            ("actionable", detail.clone(), Some(action.clone()))
        }
        StepState::NotApplicable { because } => ("not_applicable", because.clone(), None),
    };
    let (action_kind, tab, command) = match action {
        Some(StepAction::OpenTab { tab }) => (Some("open_tab"), Some(tab.to_string()), None),
        Some(StepAction::InstallCli) => (Some("install_cli"), None, None),
        Some(StepAction::Manual { command }) => (Some("manual"), None, Some(command)),
        None => (None, None, None),
    };
    SetupStepDto {
        id: step.id.as_str().to_string(),
        title: step.title.to_string(),
        state: state.to_string(),
        detail,
        action: action_kind.map(str::to_string),
        tab,
        command,
    }
}

/// Read the machine and decide every row.
///
/// **Never call this from the startup preload path.** It performs a D-Bus
/// introspect plus two name lookups, and may run the user's login shell. It
/// belongs behind a tab the user opened on purpose.
#[tauri::command]
#[specta::specta]
pub async fn get_setup_checklist(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<SetupChecklistDto, String> {
    let config = state.config.read().await.clone();
    let verdict = *state
        .hotkey_verdict
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let install = InstallKind::detect();
    let autostart = autostart_state(&app);

    // Probing touches D-Bus and possibly spawns a shell, so keep it off the
    // async runtime's core threads.
    let (caps, cli) = tauri::async_runtime::spawn_blocking(move || {
        (
            lychi_core::context::capabilities::probe_all(),
            probe_cli(install),
        )
    })
    .await
    .map_err(|e| format!("setup probe failed: {e}"))?;

    let checklist = setup::assess(&setup::SetupInputs {
        config: &config,
        install,
        hotkey: verdict,
        caps: &caps,
        autostart,
        cli,
    });

    Ok(SetupChecklistDto {
        steps: checklist.steps.iter().map(to_dto).collect(),
        actionable: checklist.actionable_count() as u32,
    })
}

/// Whether launch-at-login is on, or `None` when the answer cannot be trusted.
///
/// The plugin writes `~/.config/autostart/*.desktop`, which a Flatpak sandbox
/// ignores and some compositors (Hyprland, most tiling WMs) never read. Rather
/// than report a confident "off" that the desktop will contradict, an
/// undeterminable answer becomes a `NotApplicable` row.
fn autostart_state(app: &AppHandle) -> Option<bool> {
    if InstallKind::detect() == InstallKind::Flatpak {
        return None;
    }
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().ok()
}

/// Where `lychi` currently stands, from the user's point of view.
fn probe_cli(install: InstallKind) -> CliStatus {
    let link = setup::cli_link::link_path();
    let link_exists = link
        .as_ref()
        .map(|p| std::fs::symlink_metadata(p).is_ok())
        .unwrap_or(false);
    let dir = link
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    setup::cli_link::classify(
        install == InstallKind::AppImage,
        resolve_on_login_path("lychi"),
        link_exists,
        &dir,
    )
}

/// Find a command the way the *user's shell* would, not the way this process
/// would.
///
/// A GUI app launched from a `.desktop` file or by D-Bus activation never
/// sources `.bashrc` or `.zshenv`, so its own `PATH` routinely omits
/// `~/.local/bin`. Trusting it would report "not installed" for a command the
/// user runs every day — and then offer to install it again. This is the same
/// mismatch behind pip's perennial "installed in '~/.local/bin' which is not on
/// PATH" warnings.
///
/// Falls back to our own `PATH` if the shell cannot be run, since a slightly
/// pessimistic answer beats no answer.
fn resolve_on_login_path(name: &str) -> Option<std::path::PathBuf> {
    if let Some(path) = login_shell_path() {
        if let Some(found) = which_in(name, &path) {
            return Some(found);
        }
        // The login shell answered and did not have it. Trust that over our own
        // environment, which is a strict subset in the cases that matter.
        return None;
    }
    std::env::var_os("PATH").and_then(|p| which_in(name, &p.to_string_lossy()))
}

/// Ask the user's login shell what its `PATH` is.
fn login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").ok()?;
    let out = std::process::Command::new(&shell)
        .args(["-lc", "printf %s \"$PATH\""])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!path.is_empty()).then_some(path)
}

fn which_in(name: &str, path: &str) -> Option<std::path::PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|c| {
            std::fs::metadata(c)
                .map(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    m.is_file() && m.permissions().mode() & 0o111 != 0
                })
                .unwrap_or(false)
        })
}

/// Put `lychi` on the user's `PATH`.
///
/// Refuses when the command already resolves — including a hand-made
/// `/usr/local/bin/lychi` — rather than shadowing it. Returns a sentence for the
/// UI to show as-is.
#[tauri::command]
#[specta::specta]
pub async fn install_cli_link() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let install = InstallKind::detect();
        if install != InstallKind::AppImage {
            return Err("The lychi command is provided by your package manager here.".to_string());
        }
        let appimage = std::env::var("APPIMAGE").ok();
        let on_path = resolve_on_login_path("lychi");
        let dir_on_path = login_bin_dir_on_path();

        match setup::cli_link::install(appimage.as_deref(), on_path, dir_on_path) {
            Ok(setup::cli_link::InstallOutcome::Linked { path }) => {
                Ok(format!("Ready — linked at {}.", path.display()))
            }
            // Deliberately not reported as success: a link the shell cannot see
            // is not a working command.
            Ok(setup::cli_link::InstallOutcome::LinkedButUnreachable { path, export_line }) => {
                Err(format!(
                    "Linked at {}, but that folder is not on your PATH yet. \
                     Add this to your shell profile:\n{export_line}",
                    path.display()
                ))
            }
            Ok(setup::cli_link::InstallOutcome::AlreadyPresent { location }) => {
                Ok(format!("Already available at {location}."))
            }
            Err(e) => Err(e.to_string()),
        }
    })
    .await
    .map_err(|e| format!("install failed: {e}"))?
}

/// Is `~/.local/bin` on the login shell's `PATH`?
fn login_bin_dir_on_path() -> bool {
    let Some(link) = setup::cli_link::link_path() else {
        return false;
    };
    let Some(dir) = link.parent() else {
        return false;
    };
    let path = login_shell_path()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    std::env::split_paths(&path).any(|p| p == dir)
}

/// The full `lychi doctor` report, for the copy button in About.
///
/// Turns "please run `lychi doctor` in a terminal and paste the output" into one
/// click — which matters because the people who most need to send it are the
/// ones whose launcher will not open.
#[tauri::command]
#[specta::specta]
pub async fn get_diagnostics() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(doctor::report)
        .await
        .map_err(|e| format!("diagnostics failed: {e}"))
}
