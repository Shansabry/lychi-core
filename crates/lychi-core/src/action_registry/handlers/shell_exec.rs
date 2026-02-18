use async_trait::async_trait;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::RwLock;
use std::time::Instant;

use crate::action_registry::{ActionHandler, ActionResult, OutputType, RiskLevel};
use crate::error::LychiError;

/// Captured environment from the user's interactive login shell.
/// Uses RwLock so it can be refreshed when the shell config changes.
static SHELL_ENV: RwLock<Option<(String, HashMap<String, String>)>> = RwLock::new(None);

/// Spawn an interactive login shell and capture its full environment.
fn capture_shell_env(shell: &str) -> HashMap<String, String> {
    let output = Command::new(shell)
        .args(["-ilc", "env -0"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let raw = String::from_utf8_lossy(&out.stdout);
            raw.split('\0')
                .filter_map(|entry| {
                    let (key, val) = entry.split_once('=')?;
                    Some((key.to_string(), val.to_string()))
                })
                .collect()
        }
        _ => {
            tracing::warn!("Failed to capture shell env from {shell}, using process env");
            std::env::vars().collect()
        }
    }
}

/// Invalidate the cached shell env so the next command re-captures it.
pub fn invalidate_shell_env() {
    if let Ok(mut guard) = SHELL_ENV.write() {
        *guard = None;
    }
}

pub struct ShellExec {
    shell: String,
}

impl Default for ShellExec {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellExec {
    pub fn new() -> Self {
        Self {
            shell: "/bin/sh".to_string(),
        }
    }

    pub fn with_shell(shell: String) -> Self {
        Self { shell }
    }

    fn get_env(&self) -> HashMap<String, String> {
        // Check if we have a cached env for this shell
        if let Ok(guard) = SHELL_ENV.read()
            && let Some((cached_shell, env)) = guard.as_ref()
            && cached_shell == &self.shell
        {
            return env.clone();
        }

        // Capture and cache
        let env = capture_shell_env(&self.shell);
        if let Ok(mut guard) = SHELL_ENV.write() {
            *guard = Some((self.shell.clone(), env.clone()));
        }
        env
    }
}

#[async_trait]
impl ActionHandler for ShellExec {
    fn id(&self) -> &str {
        "run"
    }

    fn description(&self) -> &str {
        "Execute a shell command"
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let cmd = args.trim();
        if cmd.is_empty() {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some("Usage: run <shell command>".to_string()),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            });
        }

        let start = Instant::now();
        // Run as interactive shell (-ic) so that rc files are sourced and
        // shell functions like nvm, pyenv, rvm etc. are available.
        let env = self.get_env();
        let output = Command::new(&self.shell)
            .args(["-ic", cmd])
            .env_clear()
            .envs(&env)
            .env("TERM", "xterm-256color")
            .env("COLUMNS", "120")
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .output()?;
        let duration_ms = start.elapsed().as_millis() as u64;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        let success = output.status.success();

        let (out, err) = if success {
            let combined = if stdout.is_empty() && !stderr.is_empty() {
                Some(stderr)
            } else if stdout.is_empty() {
                None
            } else {
                Some(stdout)
            };
            (combined, None)
        } else {
            (
                if stdout.is_empty() {
                    None
                } else {
                    Some(stdout)
                },
                if stderr.is_empty() {
                    None
                } else {
                    Some(stderr)
                },
            )
        };

        Ok(ActionResult {
            success,
            output: out,
            error: err,
            duration_ms,
            routed_by: None,
            open_url: None,
            needs_confirmation: None,
            risk_level: None,
            output_type: Some(OutputType::Terminal),
            executed_args: None,
        })
    }
}
