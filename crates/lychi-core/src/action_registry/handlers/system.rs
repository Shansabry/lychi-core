use async_trait::async_trait;
use std::process::Command;
use std::time::Instant;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem, RiskLevel};
use crate::error::LychiError;

pub struct SystemCommand;

impl SystemCommand {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SystemCommand {
    fn default() -> Self {
        Self::new()
    }
}

struct Action {
    name: &'static str,
    description: &'static str,
    run: fn() -> Result<(), String>,
}

fn actions() -> &'static [Action] {
    &[
        Action {
            name: "shutdown",
            description: "Power off the system",
            run: || run_cmd("systemctl", &["poweroff"]),
        },
        Action {
            name: "reboot",
            description: "Restart the system",
            run: || run_cmd("systemctl", &["reboot"]),
        },
        Action {
            name: "suspend",
            description: "Suspend (sleep) the system",
            run: || run_cmd("systemctl", &["suspend"]),
        },
        Action {
            name: "hibernate",
            description: "Hibernate the system",
            run: || run_cmd("systemctl", &["hibernate"]),
        },
        Action {
            name: "lock",
            description: "Lock the screen",
            run: || run_cmd("loginctl", &["lock-session"]),
        },
        Action {
            name: "logout",
            description: "Log out of the current session",
            run: || {
                // Try loginctl first, fall back to session-specific methods
                if run_cmd("loginctl", &["terminate-user", &whoami()]).is_ok() {
                    return Ok(());
                }
                // Fallback: try gnome-session-quit or similar
                run_cmd("loginctl", &["terminate-session", "self"])
            },
        },
    ]
}

fn run_cmd(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run {program}: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("{program} failed: {}", stderr.trim()))
    }
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "".to_string())
}

#[async_trait]
impl ActionHandler for SystemCommand {
    fn id(&self) -> &str {
        "system"
    }

    fn description(&self) -> &str {
        "System power controls (shutdown, reboot, suspend, lock, logout)"
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let action_name = args.trim().to_lowercase();
        let start = Instant::now();

        if action_name.is_empty() {
            return Ok(ActionResult {
                success: false,
                output: None,
                error: Some(
                    "Usage: system <shutdown|reboot|suspend|hibernate|lock|logout>".to_string(),
                ),
                duration_ms: 0,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            });
        }

        let action = actions().iter().find(|a| a.name == action_name);

        match action {
            Some(a) => {
                let result = (a.run)();
                let duration_ms = start.elapsed().as_millis() as u64;
                match result {
                    Ok(()) => Ok(ActionResult {
                        success: true,
                        output: Some(format!("{} initiated", a.description)),
                        error: None,
                        duration_ms,
                        routed_by: None,
                        open_url: None,
                        needs_confirmation: None,
                        risk_level: None,
                        output_type: None,
                        executed_args: None,
                    }),
                    Err(e) => Ok(ActionResult {
                        success: false,
                        output: None,
                        error: Some(e),
                        duration_ms,
                        routed_by: None,
                        open_url: None,
                        needs_confirmation: None,
                        risk_level: None,
                        output_type: None,
                        executed_args: None,
                    }),
                }
            }
            None => Ok(ActionResult {
                success: false,
                output: None,
                error: Some(format!(
                    "Unknown action '{}'. Available: shutdown, reboot, suspend, hibernate, lock, logout",
                    action_name
                )),
                duration_ms: start.elapsed().as_millis() as u64,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            }),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let lower = partial.to_lowercase();
        actions()
            .iter()
            .filter(|a| a.name.contains(&lower) || lower.is_empty())
            .map(|a| CompletionItem {
                label: a.name.to_string(),
                icon_path: None,
                score: if a.name.starts_with(&lower) { 100 } else { 50 },
            })
            .collect()
    }
}
