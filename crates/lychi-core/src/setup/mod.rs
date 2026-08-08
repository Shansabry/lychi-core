//! What still needs doing on *this* machine — the Setup tab's one decider.
//!
//! This is a **diagnostics surface, not an onboarding checklist**. The
//! distinction decides the design: an onboarding checklist is completed once and
//! then gone, but every fact here can regress. Replacing the AppImage dangles
//! the CLI symlink; moving from X11 to Wayland breaks a working hotkey;
//! uninstalling Ollama strands an AI provider. So nothing here is ever "done
//! forever", nothing is persisted, and every step is re-derived on each read.
//! The closest precedent is a health-check page, not an activation widget.
//!
//! Persisting per-step booleans was considered and rejected: a stored "done"
//! that the environment later contradicts is exactly the drift this module
//! exists to prevent.
//!
//! [`assess`] is **pure**. It probes nothing — no environment, no D-Bus, no
//! filesystem — and instead takes everything it needs in [`SetupInputs`]. That
//! is what makes every branch (Flatpak, a portal-less compositor, an X11 grab a
//! window manager silently kept) testable on any machine, rather than only on
//! whichever desktop the developer happens to run. The same shape as
//! [`crate::install::InstallKind::from_env`] and
//! [`crate::hotkey::HotkeyVerdict::assess`], for the same reason.

pub mod cli_link;

use crate::config::Config;
use crate::context::capabilities::Capabilities;
use crate::hotkey::{Confidence, HotkeyVerdict};
use crate::install::InstallKind;

/// Which step a row is. The frontend switches on this to pick an action
/// handler; it is never parsed out of a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepId {
    Hotkey,
    CliPath,
    Desktop,
    Autostart,
    Ai,
    Terminal,
}

impl StepId {
    /// Stable identifier for the frontend and for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            StepId::Hotkey => "hotkey",
            StepId::CliPath => "cli_path",
            StepId::Desktop => "desktop",
            StepId::Autostart => "autostart",
            StepId::Ai => "ai",
            StepId::Terminal => "terminal",
        }
    }
}

/// What the user should do about a step, decided here rather than inferred by
/// the UI. The frontend is a `match` with no policy of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepAction {
    /// Jump to an existing settings tab that already owns this control. Setup
    /// never duplicates a control that exists elsewhere.
    OpenTab { tab: &'static str },
    /// Create the `lychi` symlink.
    InstallCli,
    /// Nothing can be automated: show the command and let the user run it.
    /// Also covers "we did our part, but your shell cannot see it yet".
    Manual { command: String },
}

/// Where a step stands.
///
/// [`StepState::NotApplicable`] is deliberately a variant rather than a flag on
/// the other two: it carries no completion signal, so it cannot be counted, and
/// a step that cannot apply here is structurally incapable of reading as a
/// failure. Showing a permanent ✗ for something the machine will never need is
/// what trains people to ignore the whole surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepState {
    /// Nothing to do. Includes "the user chose otherwise" — a deliberate "off"
    /// is a finished decision, not an outstanding task.
    Done { detail: String },
    /// Something the user can do here, now.
    Actionable { detail: String, action: StepAction },
    /// Cannot apply on this machine, with the reason. Excluded from the
    /// actionable count entirely.
    NotApplicable { because: String },
}

impl StepState {
    /// Does this step want the user's attention?
    ///
    /// The one place that question is answered. The badge, the summary line and
    /// any future "needs attention" styling all read this rather than each
    /// re-testing the variant and drifting apart.
    pub fn is_actionable(&self) -> bool {
        matches!(self, StepState::Actionable { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupStep {
    pub id: StepId,
    pub title: &'static str,
    pub state: StepState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupChecklist {
    pub steps: Vec<SetupStep>,
}

impl SetupChecklist {
    /// How many rows want attention. Drives the sidebar badge, which is hidden
    /// entirely when this is zero.
    ///
    /// Deliberately **not** a "3 of 6" progress count. Two reasons: with
    /// `NotApplicable` rows the denominator is dishonest — a counter that can
    /// never legitimately reach its maximum is a permanent nag — and progress
    /// indicators are known to backfire when early progress is slower than
    /// expected, which is exactly the fresh-install profile here (the hardest
    /// items are the ones left over).
    pub fn actionable_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| s.state.is_actionable())
            .count()
    }
}

/// Everything [`assess`] needs, gathered by the caller so the rule itself stays
/// pure.
pub struct SetupInputs<'a> {
    pub config: &'a Config,
    pub install: InstallKind,
    /// The hotkey verdict, recorded where the binding actually happened.
    /// Consumed verbatim and never re-derived — deriving it again from weaker
    /// signals (`session_type`, the portal list) is precisely the bug that
    /// reported a stolen X11 grab as working.
    pub hotkey: HotkeyVerdict,
    pub caps: &'a Capabilities,
    /// Whether launch-at-login is on, or `None` when it cannot be determined —
    /// inside a Flatpak sandbox, or on a compositor that ignores
    /// `~/.config/autostart` (Hyprland and most tiling WMs). `None` becomes
    /// `NotApplicable` rather than a confident wrong "off".
    pub autostart: Option<bool>,
    /// Where the `lychi` command currently stands. Probed by the caller because
    /// answering it touches the filesystem and the user's login shell.
    pub cli: CliStatus,
}

/// The state of the `lychi` command-line entry point.
///
/// Four states rather than three: a symlink can be perfectly correct and still
/// invisible, because the directory holding it is not on the user's `PATH`.
/// Collapsing that into "not installed" would make the app offer to fix
/// something it had already done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliStatus {
    /// Resolvable as a command. Carries where it was found, for the detail line.
    OnPath { location: String },
    /// The link exists and is correct, but its directory is not on `PATH`.
    /// Carries the shell line that would fix it.
    LinkedButUnreachable { dir: String, export_line: String },
    /// Nothing found.
    Missing,
    /// A package manager owns it, so this is not ours to install.
    ManagedBySystem,
}

/// Decide every row. Pure: same inputs, same output, on any machine.
pub fn assess(inputs: &SetupInputs) -> SetupChecklist {
    SetupChecklist {
        steps: vec![
            hotkey_step(inputs),
            cli_step(inputs),
            desktop_step(inputs),
            autostart_step(inputs),
            ai_step(inputs),
            terminal_step(inputs),
        ],
    }
}

/// The hotkey row reads [`HotkeyVerdict`] and nothing else.
///
/// When it is broken the advice is to bind `lychi --toggle` in the desktop's own
/// keyboard settings — which is only honest if `lychi` actually resolves. The
/// CLI row is therefore not independent of this one: on a portal-less Wayland
/// compositor the command *is* the hotkey. Recommending a command that does not
/// exist is worse than admitting there is no hotkey.
fn hotkey_step(inputs: &SetupInputs) -> SetupStep {
    let explanation = inputs.hotkey.explain();
    let state = match inputs.hotkey.confidence {
        Confidence::Reliable => StepState::Done {
            detail: format!("{} — {explanation}", inputs.config.general.hotkey),
        },
        Confidence::Unverified => StepState::Actionable {
            detail: format!("{explanation}."),
            action: StepAction::OpenTab { tab: "general" },
        },
        Confidence::Broken => {
            // Only offer the CLI fallback when it will actually run.
            let action = match &inputs.cli {
                CliStatus::OnPath { .. } => StepAction::Manual {
                    command: "lychi --toggle".to_string(),
                },
                _ => StepAction::OpenTab { tab: "general" },
            };
            let detail = match &inputs.cli {
                CliStatus::OnPath { .. } => format!(
                    "{explanation}. Bind this command to a key in your desktop's \
                     keyboard settings instead."
                ),
                _ => format!(
                    "{explanation}. Set up terminal access below to bind it \
                     manually instead."
                ),
            };
            StepState::Actionable { detail, action }
        }
    };
    SetupStep {
        id: StepId::Hotkey,
        title: "Global hotkey",
        state,
    }
}

fn cli_step(inputs: &SetupInputs) -> SetupStep {
    let state = match &inputs.cli {
        CliStatus::ManagedBySystem => StepState::NotApplicable {
            because: "Installed by your package manager.".to_string(),
        },
        // Plain text: these strings are rendered verbatim, so markdown ticks
        // would show up as literal characters.
        CliStatus::OnPath { location } => StepState::Done {
            detail: format!("Available at {location}."),
        },
        CliStatus::LinkedButUnreachable { dir, export_line } => StepState::Actionable {
            detail: format!(
                "Installed in {dir}, but that folder is not on your PATH. \
                 Add this line to your shell profile:"
            ),
            action: StepAction::Manual {
                command: export_line.clone(),
            },
        },
        CliStatus::Missing => StepState::Actionable {
            detail: "Run Lychi from a terminal, a script, or a key bound in your \
                     desktop settings."
                .to_string(),
            action: StepAction::InstallCli,
        },
    };
    SetupStep {
        id: StepId::CliPath,
        title: "Terminal access",
        state,
    }
}

/// Informational: what this desktop can and cannot do. Never actionable —
/// a missing portal is not something the user can install from here, and
/// offering a button that cannot work is worse than saying nothing.
fn desktop_step(inputs: &SetupInputs) -> SetupStep {
    let missing = missing_portals(inputs.caps);
    let state = if missing.is_empty() {
        StepState::Done {
            detail: "All desktop integration features are available.".to_string(),
        }
    } else {
        StepState::NotApplicable {
            because: format!(
                "Your desktop does not provide: {}. Related features fall back \
                 automatically.",
                missing.join(", ")
            ),
        }
    };
    SetupStep {
        id: StepId::Desktop,
        title: "Desktop integration",
        state,
    }
}

/// Portals whose absence changes what Lychi can do, in user-facing words.
///
/// Named by capability rather than by interface: "screenshots" is actionable
/// information, `org.freedesktop.portal.Screenshot` is not.
fn missing_portals(caps: &Capabilities) -> Vec<&'static str> {
    let has = |suffix: &str| caps.portals.iter().any(|p| p.ends_with(suffix));
    let mut missing = Vec::new();
    if !has("Screenshot") {
        missing.push("screenshots");
    }
    if !has("GlobalShortcuts") {
        missing.push("global shortcuts");
    }
    missing
}

fn autostart_step(inputs: &SetupInputs) -> SetupStep {
    let state = match inputs.autostart {
        None => StepState::NotApplicable {
            because: "Your desktop manages startup applications itself.".to_string(),
        },
        Some(true) => StepState::Done {
            detail: "Lychi starts hidden when you log in.".to_string(),
        },
        // Not actionable: choosing not to autostart is a legitimate preference,
        // and nagging about it is how a diagnostics page turns into an advert.
        Some(false) => StepState::Done {
            detail: "Off — summon Lychi with the hotkey or `lychi --toggle`.".to_string(),
        },
    };
    SetupStep {
        id: StepId::Autostart,
        title: "Start at login",
        state,
    }
}

/// AI off is a **completed decision**, not an outstanding task.
///
/// Lychi is local-first and AI is opt-in, so a user who wants none of it has
/// finished configuring it. A row that reads as incomplete forever because
/// someone declined an optional feature is exactly the nagging that teaches
/// people to stop reading the page.
fn ai_step(inputs: &SetupInputs) -> SetupStep {
    let mode = inputs.config.ai.mode.as_str();
    let state = if mode == "disabled" {
        StepState::Done {
            detail: "Off — Lychi works fully without it.".to_string(),
        }
    } else {
        StepState::Done {
            detail: format!(
                "{} — check the AI tab to test the connection.",
                label_for(mode)
            ),
        }
    };
    SetupStep {
        id: StepId::Ai,
        title: "AI",
        state,
    }
}

fn label_for(mode: &str) -> &str {
    match mode {
        "byo" => "Your own API key",
        "ollama" => "Ollama",
        "local" => "On-device model",
        other => other,
    }
}

fn terminal_step(inputs: &SetupInputs) -> SetupStep {
    let terminal = inputs.config.commands.terminal.trim();
    let state = if terminal.is_empty() {
        StepState::Done {
            detail: "Using the first terminal found on this system.".to_string(),
        }
    } else {
        StepState::Done {
            detail: format!("Commands open in {terminal}."),
        }
    };
    SetupStep {
        id: StepId::Terminal,
        title: "Terminal",
        state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::Binding;

    /// A machine where everything works, as the baseline each test perturbs.
    fn caps_all() -> Capabilities {
        Capabilities {
            portals: vec![
                "org.freedesktop.portal.Screenshot".to_string(),
                "org.freedesktop.portal.GlobalShortcuts".to_string(),
            ],
            kwin_scripting: true,
            gnome_shell: false,
        }
    }

    fn caps_none() -> Capabilities {
        Capabilities {
            portals: vec![],
            kwin_scripting: false,
            gnome_shell: false,
        }
    }

    fn on_path() -> CliStatus {
        CliStatus::OnPath {
            location: "/home/u/.local/bin/lychi".to_string(),
        }
    }

    /// Everything healthy. Individual tests override one field at a time so a
    /// failure names the input that caused it.
    fn inputs<'a>(config: &'a Config, caps: &'a Capabilities) -> SetupInputs<'a> {
        SetupInputs {
            config,
            install: InstallKind::AppImage,
            hotkey: HotkeyVerdict::assess(Binding::Portal, true),
            caps,
            autostart: Some(true),
            cli: on_path(),
        }
    }

    fn step<'a>(list: &'a SetupChecklist, id: StepId) -> &'a SetupStep {
        list.steps
            .iter()
            .find(|s| s.id == id)
            .expect("every step is always present")
    }

    #[test]
    fn every_step_appears_exactly_once() {
        let config = Config::default();
        let caps = caps_all();
        let list = assess(&inputs(&config, &caps));
        let mut ids: Vec<&str> = list.steps.iter().map(|s| s.id.as_str()).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "a step id is duplicated");
        assert_eq!(count, 6);
    }

    // ---- the hotkey row tracks the verdict, and only the verdict ----

    /// The one rule this module must never break: the verdict is consumed as
    /// given. Re-deriving it from the session type or the portal list is the
    /// bug that reported a window-manager-stolen X11 grab as working.
    #[test]
    fn hotkey_tracks_every_confidence() {
        let config = Config::default();
        let caps = caps_all();

        for (binding, wayland, actionable) in [
            (Binding::Portal, true, false),
            (Binding::DesktopSettings, false, false),
            (Binding::X11Grab, false, true), // Unverified — ask, don't warn
            (Binding::X11Grab, true, true),  // Broken on Wayland
            (Binding::Conflict, false, true),
            (Binding::None, false, true),
        ] {
            let mut i = inputs(&config, &caps);
            i.hotkey = HotkeyVerdict::assess(binding, wayland);
            let list = assess(&i);
            assert_eq!(
                step(&list, StepId::Hotkey).state.is_actionable(),
                actionable,
                "{binding:?} (wayland={wayland}) produced the wrong hotkey row"
            );
        }
    }

    /// A capable desktop must not launder a broken hotkey into a working one.
    /// This is the specific shape of the shipped bug.
    #[test]
    fn a_broken_hotkey_stays_broken_on_a_fully_capable_desktop() {
        let config = Config::default();
        let caps = caps_all(); // GlobalShortcuts portal present
        let mut i = inputs(&config, &caps);
        i.hotkey = HotkeyVerdict::assess(Binding::None, true);
        let list = assess(&i);
        assert!(step(&list, StepId::Hotkey).state.is_actionable());
    }

    /// Conversely: a reliable hotkey stays reliable even where every portal is
    /// missing, because the verdict already accounts for how it was bound.
    #[test]
    fn a_reliable_hotkey_survives_a_capability_less_desktop() {
        let config = Config::default();
        let caps = caps_none();
        let mut i = inputs(&config, &caps);
        i.hotkey = HotkeyVerdict::assess(Binding::DesktopSettings, true);
        let list = assess(&i);
        assert!(!step(&list, StepId::Hotkey).state.is_actionable());
    }

    /// The hotkey and CLI rows are coupled: `lychi --toggle` is the fallback on
    /// a portal-less compositor, so it may only be recommended when it runs.
    #[test]
    fn a_broken_hotkey_only_suggests_the_command_when_it_exists() {
        let config = Config::default();
        let caps = caps_none();

        let mut with_cli = inputs(&config, &caps);
        with_cli.hotkey = HotkeyVerdict::assess(Binding::None, true);
        let list = assess(&with_cli);
        match &step(&list, StepId::Hotkey).state {
            StepState::Actionable { action, .. } => assert_eq!(
                *action,
                StepAction::Manual {
                    command: "lychi --toggle".to_string()
                }
            ),
            other => panic!("expected an actionable hotkey row, got {other:?}"),
        }

        let mut without_cli = inputs(&config, &caps);
        without_cli.hotkey = HotkeyVerdict::assess(Binding::None, true);
        without_cli.cli = CliStatus::Missing;
        let list = assess(&without_cli);
        match &step(&list, StepId::Hotkey).state {
            StepState::Actionable { action, .. } => assert!(
                !matches!(action, StepAction::Manual { .. }),
                "recommended a command that is not installed"
            ),
            other => panic!("expected an actionable hotkey row, got {other:?}"),
        }
    }

    // ---- the CLI row: four states, and what each install kind implies ----

    #[test]
    fn a_package_managed_cli_is_not_ours_to_install() {
        let config = Config::default();
        let caps = caps_all();
        let mut i = inputs(&config, &caps);
        i.install = InstallKind::System;
        i.cli = CliStatus::ManagedBySystem;
        let list = assess(&i);
        assert!(matches!(
            step(&list, StepId::CliPath).state,
            StepState::NotApplicable { .. }
        ));
    }

    #[test]
    fn a_missing_cli_offers_to_install_itself() {
        let config = Config::default();
        let caps = caps_all();
        let mut i = inputs(&config, &caps);
        i.cli = CliStatus::Missing;
        let list = assess(&i);
        match &step(&list, StepId::CliPath).state {
            StepState::Actionable { action, .. } => {
                assert_eq!(*action, StepAction::InstallCli);
            }
            other => panic!("expected an install offer, got {other:?}"),
        }
    }

    /// The state that a three-state model cannot express. Getting this wrong
    /// makes the app offer to install something it already installed.
    #[test]
    fn a_linked_but_unreachable_cli_shows_the_path_line_not_an_install_button() {
        let config = Config::default();
        let caps = caps_all();
        let mut i = inputs(&config, &caps);
        i.cli = CliStatus::LinkedButUnreachable {
            dir: "/home/u/.local/bin".to_string(),
            export_line: "export PATH=\"$HOME/.local/bin:$PATH\"".to_string(),
        };
        let list = assess(&i);
        match &step(&list, StepId::CliPath).state {
            StepState::Actionable { action, .. } => match action {
                StepAction::Manual { command } => assert!(command.contains(".local/bin")),
                other => panic!("expected the PATH line, got {other:?}"),
            },
            other => panic!("expected an actionable row, got {other:?}"),
        }
    }

    #[test]
    fn a_resolvable_cli_is_done_and_names_where_it_found_it() {
        let config = Config::default();
        let caps = caps_all();
        let list = assess(&inputs(&config, &caps));
        match &step(&list, StepId::CliPath).state {
            StepState::Done { detail } => assert!(detail.contains("/home/u/.local/bin/lychi")),
            other => panic!("expected done, got {other:?}"),
        }
    }

    // ---- "off" is a decision, not an outstanding task ----

    /// Lychi is local-first and AI is opt-in, so declining it is finished
    /// configuration. A row that nags forever teaches people to ignore the page.
    #[test]
    fn disabled_ai_is_done_not_outstanding() {
        let mut config = Config::default();
        config.ai.mode = "disabled".to_string();
        let caps = caps_all();
        let list = assess(&inputs(&config, &caps));
        let ai = step(&list, StepId::Ai);
        assert!(!ai.state.is_actionable());
        assert!(matches!(ai.state, StepState::Done { .. }));
    }

    #[test]
    fn autostart_off_is_a_preference_not_a_problem() {
        let config = Config::default();
        let caps = caps_all();
        let mut i = inputs(&config, &caps);
        i.autostart = Some(false);
        let list = assess(&i);
        assert!(!step(&list, StepId::Autostart).state.is_actionable());
    }

    /// `None` means "we cannot tell" — inside a Flatpak sandbox, or on a
    /// compositor that ignores `~/.config/autostart`. It must not be reported
    /// as a confident "off".
    #[test]
    fn undeterminable_autostart_is_not_applicable_rather_than_off() {
        let config = Config::default();
        let caps = caps_all();
        let mut i = inputs(&config, &caps);
        i.autostart = None;
        let list = assess(&i);
        let state = &step(&list, StepId::Autostart).state;
        assert!(
            matches!(state, StepState::NotApplicable { .. }),
            "got {state:?}"
        );
        assert!(!state.is_actionable());
    }

    // ---- the badge count ----

    /// `NotApplicable` carries no completion signal, so it can never inflate
    /// the number of things demanding attention.
    #[test]
    fn not_applicable_rows_are_never_counted_as_actionable() {
        let config = Config::default();
        let caps = caps_none(); // desktop row -> NotApplicable
        let mut i = inputs(&config, &caps);
        i.install = InstallKind::System;
        i.cli = CliStatus::ManagedBySystem; // cli row -> NotApplicable
        i.autostart = None; // autostart row -> NotApplicable
        let list = assess(&i);

        let not_applicable = list
            .steps
            .iter()
            .filter(|s| matches!(s.state, StepState::NotApplicable { .. }))
            .count();
        assert_eq!(not_applicable, 3, "expected three inapplicable rows");
        assert_eq!(
            list.actionable_count(),
            0,
            "inapplicable rows must not demand attention"
        );
    }

    /// The badge is hidden when this is zero, so a fully healthy machine must
    /// produce exactly zero.
    #[test]
    fn a_healthy_machine_has_nothing_to_report() {
        let config = Config::default();
        let caps = caps_all();
        assert_eq!(assess(&inputs(&config, &caps)).actionable_count(), 0);
    }

    #[test]
    fn the_count_tracks_the_number_of_actionable_rows() {
        let config = Config::default();
        let caps = caps_all();
        let mut i = inputs(&config, &caps);
        i.hotkey = HotkeyVerdict::assess(Binding::None, true);
        i.cli = CliStatus::Missing;
        let list = assess(&i);
        assert_eq!(list.actionable_count(), 2);
    }

    // ---- the desktop row ----

    #[test]
    fn a_missing_portal_is_reported_but_never_actionable() {
        let config = Config::default();
        let caps = caps_none();
        let list = assess(&inputs(&config, &caps));
        let state = &step(&list, StepId::Desktop).state;
        assert!(!state.is_actionable(), "the user cannot install a portal");
        match state {
            StepState::NotApplicable { because } => {
                assert!(because.contains("screenshots"));
                assert!(because.contains("global shortcuts"));
            }
            other => panic!("expected an explanation, got {other:?}"),
        }
    }

    /// Portals are matched by interface suffix, so an unrelated portal must not
    /// satisfy the check.
    #[test]
    fn an_unrelated_portal_does_not_satisfy_the_desktop_row() {
        let config = Config::default();
        let caps = Capabilities {
            portals: vec!["org.freedesktop.portal.Notification".to_string()],
            kwin_scripting: false,
            gnome_shell: false,
        };
        let list = assess(&inputs(&config, &caps));
        assert!(matches!(
            step(&list, StepId::Desktop).state,
            StepState::NotApplicable { .. }
        ));
    }

    // ---- purity ----

    /// The whole point of `SetupInputs`: identical inputs give identical output,
    /// with nothing read from the machine running the test.
    #[test]
    fn assess_is_pure() {
        let config = Config::default();
        let caps = caps_all();
        let a = assess(&inputs(&config, &caps));
        let b = assess(&inputs(&config, &caps));
        assert_eq!(a, b);
    }
}
