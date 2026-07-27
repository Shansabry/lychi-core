//! Script Commands handler — runs user scripts from `~/.config/lychi/scripts/`
//! as named launcher commands. Discovery, metadata parsing, and the fs-watcher
//! live in `crate::script_commands`; this handler is the execution + completion
//! surface.
//!
//! Routing mirrors the bang handler: a single handler (id `"script"`) plus a
//! keyword side-channel on the Executor (`set_script_keywords`). The handler
//! receives `"<keyword> <args>"` and dispatches to the matching script.

use std::collections::HashMap;

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, RiskAssessment,
    RiskLevel,
};
use crate::error::LychiError;
use crate::script_commands::{ScriptCommand, ScriptMode};

use super::shell_exec::Clearance;

/// Timeout for an inline (captured) script — a hanging script must not wedge the
/// launcher. Terminal-mode scripts detach and aren't bounded here.
const SCRIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Cap on captured output (bytes) so a chatty script can't flood the UI.
const MAX_OUTPUT_BYTES: usize = 32 * 1024;

pub struct ScriptCommandsHandler {
    /// keyword → command. Rebuilt (re-registered) by the scripts watcher.
    scripts: HashMap<String, ScriptCommand>,
    /// Login shell used to run scripts (honors shebangs; same env as `run`).
    shell: String,
}

impl ScriptCommandsHandler {
    pub fn new(scripts: Vec<ScriptCommand>, shell: String) -> Self {
        let scripts = scripts
            .into_iter()
            .map(|s| (s.keyword.clone(), s))
            .collect();
        Self { scripts, shell }
    }

    /// The discovered keywords (already lowercased), for the router to recognise.
    pub fn keywords(&self) -> Vec<String> {
        self.scripts.keys().cloned().collect()
    }

    /// Split `"<keyword> <args>"` into the matched script + the remaining args.
    fn resolve<'a>(&'a self, input: &str) -> Option<(&'a ScriptCommand, String)> {
        let trimmed = input.trim();
        let (first, rest) = match trimmed.split_once(char::is_whitespace) {
            Some((f, r)) => (f, r.trim().to_string()),
            None => (trimmed, String::new()),
        };
        self.scripts.get(&first.to_lowercase()).map(|s| (s, rest))
    }
}

/// Build the shell command line: `<script_path> <args>`. The path is
/// shell-escaped; args are passed through as the user typed them (same contract
/// as the `run` handler — args go through the shell).
fn build_command(cmd: &ScriptCommand, args: &str) -> String {
    let path = cmd.path.to_string_lossy();
    // Minimal escaping of the path (spaces in the scripts dir): wrap in quotes,
    // escaping any embedded quote.
    let escaped = format!("'{}'", path.replace('\'', "'\\''"));
    if args.is_empty() {
        escaped
    } else {
        format!("{escaped} {args}")
    }
}

#[async_trait]
impl ActionHandler for ScriptCommandsHandler {
    fn id(&self) -> &str {
        "script"
    }

    fn description(&self) -> &str {
        "Run a Script Command from ~/.config/lychi/scripts/"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Developer
    }

    // User-authored scripts in the user's own scripts dir auto-run by default
    // (an authored-intent boundary like a shell alias). A script can opt into a
    // confirmation with `# @lychi.risk medium`.
    fn assess_risk(
        &self,
        args: &str,
        _ctx: &crate::action_registry::RiskContext<'_>,
    ) -> RiskAssessment {
        match self.resolve(args) {
            Some((cmd, _)) if cmd.confirm => RiskAssessment {
                level: RiskLevel::Medium,
                reason: Some(format!("Run script “{}”?", cmd.title)),
            },
            _ => RiskAssessment::level(RiskLevel::Low),
        }
    }

    async fn execute(&self, ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let Some((cmd, script_args)) = self.resolve(args) else {
            return Ok(ActionResult::err("No such script command"));
        };
        let command = build_command(cmd, &script_args);
        let cwd = ctx.cwd.as_deref();

        // Clearance for the assembled command line. A script that opted into a
        // confirmation (`# @lychi.risk medium`) has already been approved by the
        // Rules Engine before we get here, so it carries user clearance for a
        // `Confirm` verdict. A default (Low-risk) script carries none — so the
        // central decider independently authorizes its command line: a benign
        // `'<path>' <args>` is `Allow` and runs, but if the resolved line hits a
        // dangerous/denylisted pattern it is refused rather than silently run.
        // This is the fix for script commands bypassing the shell gate (C1).
        let clearance = if cmd.confirm {
            Clearance::UserConfirmed
        } else {
            Clearance::None
        };

        match cmd.mode {
            ScriptMode::Inline => {
                // Surface a refusal (denied / needs-confirmation) as a failed
                // result rather than a hard error, so it reads cleanly in the UI
                // — matching the terminal arm below.
                match super::shell_exec::run_captured(
                    &self.shell,
                    &command,
                    cwd,
                    SCRIPT_TIMEOUT,
                    MAX_OUTPUT_BYTES,
                    clearance,
                )
                .await
                {
                    Ok(r) => Ok(r),
                    Err(e) => Ok(ActionResult::err(e.to_string())),
                }
            }
            ScriptMode::Terminal => {
                let terminal = ctx.terminal.as_deref();
                match super::shell_exec::open_in_terminal(&command, cwd, terminal, clearance) {
                    Ok(_) => Ok(ActionResult::ok(
                        format!("Launched “{}” in a terminal", cmd.title),
                        crate::action_registry::OutputType::Status,
                    )),
                    Err(e) => Ok(ActionResult::err(format!("Failed to launch: {e}"))),
                }
            }
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let p = partial.trim().to_lowercase();
        let mut items: Vec<CompletionItem> = self
            .scripts
            .values()
            .filter(|s| {
                p.is_empty() || s.keyword.starts_with(&p) || s.title.to_lowercase().contains(&p)
            })
            .map(|s| CompletionItem {
                label: s.keyword.clone(),
                icon_path: Some("__custom__".into()),
                score: 100,
                description: if s.description.is_empty() {
                    Some(s.title.clone())
                } else {
                    Some(s.description.clone())
                },
                // Fill the keyword so the user can add args, matching the
                // tab-to-complete pattern for arg-taking commands.
                fill: Some(format!("{} ", s.keyword)),
                ..Default::default()
            })
            .collect();
        items.sort_by(|a, b| a.label.cmp(&b.label));
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cmd(keyword: &str, mode: ScriptMode) -> ScriptCommand {
        ScriptCommand {
            keyword: keyword.into(),
            path: PathBuf::from(format!("/tmp/{keyword}.sh")),
            title: keyword.into(),
            description: String::new(),
            mode,
            confirm: false,
        }
    }

    #[test]
    fn resolve_splits_keyword_and_args() {
        let h =
            ScriptCommandsHandler::new(vec![cmd("deploy", ScriptMode::Inline)], "/bin/sh".into());
        let (c, args) = h.resolve("deploy prod --force").unwrap();
        assert_eq!(c.keyword, "deploy");
        assert_eq!(args, "prod --force");
    }

    #[test]
    fn resolve_bare_keyword_no_args() {
        let h =
            ScriptCommandsHandler::new(vec![cmd("backup", ScriptMode::Inline)], "/bin/sh".into());
        let (c, args) = h.resolve("backup").unwrap();
        assert_eq!(c.keyword, "backup");
        assert_eq!(args, "");
    }

    #[test]
    fn resolve_unknown_is_none() {
        let h = ScriptCommandsHandler::new(vec![cmd("a", ScriptMode::Inline)], "/bin/sh".into());
        assert!(h.resolve("nonexistent x").is_none());
    }

    #[test]
    fn build_command_quotes_path_and_appends_args() {
        let c = cmd("deploy", ScriptMode::Inline);
        assert_eq!(build_command(&c, ""), "'/tmp/deploy.sh'");
        assert_eq!(build_command(&c, "prod"), "'/tmp/deploy.sh' prod");
    }

    #[test]
    fn confirm_metadata_elevates_risk() {
        let mut c = cmd("danger", ScriptMode::Inline);
        c.confirm = true;
        let h = ScriptCommandsHandler::new(vec![c], "/bin/sh".into());
        let risk = h.assess_risk("danger", &Default::default());
        assert_eq!(risk.level, RiskLevel::Medium);
        assert!(risk.reason.is_some());
    }

    #[test]
    fn no_confirm_is_low_risk() {
        let h = ScriptCommandsHandler::new(vec![cmd("safe", ScriptMode::Inline)], "/bin/sh".into());
        assert_eq!(
            h.assess_risk("safe", &Default::default()).level,
            RiskLevel::Low
        );
    }

    // Build a real script file on disk and a handler wrapping it, so `execute`
    // resolves + assembles a genuine command line. Returns (handler, cleanup).
    //
    // The dir is made unique per call with an atomic counter (not just pid+keyword)
    // so tests reusing the same keyword don't share a directory and race on
    // `remove_dir_all` when run in parallel (CLAUDE.md temp-path rule).
    fn script_on_disk(keyword: &str, confirm: bool) -> (ScriptCommandsHandler, PathBuf) {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lychi-scripttest-{}-{keyword}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join(format!("{keyword}.sh"));
        let mut f = std::fs::File::create(&script).unwrap();
        writeln!(f, "#!/bin/sh\ntrue").unwrap();
        let c = ScriptCommand {
            keyword: keyword.into(),
            path: script,
            title: keyword.into(),
            description: String::new(),
            mode: ScriptMode::Inline,
            confirm,
        };
        (ScriptCommandsHandler::new(vec![c], "/bin/sh".into()), dir)
    }

    // A script whose resolved command line is denylisted must be blocked at the
    // shell-assembly chokepoint, even though the script handler itself is Low
    // risk (it never went through the Rules Engine `run` path). Guards the C1
    // fix: script commands used to reach `sh -ic` with no denylist check.
    #[tokio::test]
    async fn denylisted_script_command_is_rejected() {
        let (h, dir) = script_on_disk("noop", false);
        let ctx = ExecContext::default();
        // Assembled line: `'<path>' rm -rf /` → hard Deny.
        let res = h.execute(&ctx, "noop rm -rf /").await.unwrap();
        assert!(!res.success, "denylisted command line must not execute");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A Low-risk (no-confirm) script whose command line hits a *Confirm* pattern
    // (not a hard deny) must be refused — it carries no user clearance, so the
    // decider won't silently run a mutating command. This is the other half of
    // the C1/C2 fix: not just catastrophes, but un-vetted dangerous commands.
    #[tokio::test]
    async fn unconfirmed_script_with_dangerous_line_is_refused() {
        let (h, dir) = script_on_disk("noop", false);
        let ctx = ExecContext::default();
        // Assembled line contains `sudo ` → Confirm; clearance None → refused.
        let res = h.execute(&ctx, "noop && sudo rm somefile").await.unwrap();
        assert!(!res.success, "unconfirmed dangerous line must be refused");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
