use async_trait::async_trait;
use std::collections::HashMap;
use std::collections::HashSet;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Instant;

use crate::action_registry::{ActionHandler, ActionResult, OutputType, RiskLevel};
use crate::error::LychiError;

/// Captured environment from the user's interactive login shell.
/// Uses RwLock so it can be refreshed when the shell config changes.
static SHELL_ENV: RwLock<Option<(String, HashMap<String, String>)>> = RwLock::new(None);

/// Context CWD — set by the executor before each shell command.
/// When set, shell commands run in this directory instead of Lychi's process CWD.
static CONTEXT_CWD: Mutex<Option<String>> = Mutex::new(None);

/// Terminal emulator setting — set by the executor from config.
static TERMINAL_SETTING: Mutex<Option<String>> = Mutex::new(None);

/// Terminal routing mode — "auto", "manual", or "off".
static TERMINAL_ROUTING: Mutex<Option<String>> = Mutex::new(None);

/// Target terminal for routing — resolved from the focus ring by the executor.
static CONTEXT_TERMINAL: Mutex<Option<TerminalTarget>> = Mutex::new(None);

#[derive(Clone)]
struct TerminalTarget {
    wm_class: String,
    pid: u32,
    window_id: Option<String>,
}

/// Set the working directory for the next shell command.
pub fn set_context_cwd(cwd: Option<String>) {
    if let Ok(mut guard) = CONTEXT_CWD.lock() {
        *guard = cwd;
    }
}

/// Get the current context CWD.
fn get_context_cwd() -> Option<String> {
    CONTEXT_CWD.lock().ok().and_then(|g| g.clone())
}

/// Set the terminal emulator for the next command.
pub fn set_terminal(terminal: Option<String>) {
    if let Ok(mut guard) = TERMINAL_SETTING.lock() {
        *guard = terminal;
    }
}

/// Get the configured terminal emulator.
fn get_terminal() -> Option<String> {
    TERMINAL_SETTING.lock().ok().and_then(|g| g.clone())
}

/// Set the terminal routing mode for the next command.
pub fn set_terminal_routing(mode: Option<String>) {
    if let Ok(mut guard) = TERMINAL_ROUTING.lock() {
        *guard = mode;
    }
}

/// Get the current terminal routing mode.
pub fn get_terminal_routing() -> String {
    TERMINAL_ROUTING
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| "off".to_string())
}

/// Set the target terminal for routing.
pub fn set_context_terminal(wm_class: Option<String>, pid: u32, window_id: Option<String>) {
    if let Ok(mut guard) = CONTEXT_TERMINAL.lock() {
        *guard = wm_class.map(|wm| TerminalTarget {
            wm_class: wm,
            pid,
            window_id,
        });
    }
}

/// Get the current routing target.
fn get_context_terminal() -> Option<TerminalTarget> {
    CONTEXT_TERMINAL.lock().ok().and_then(|g| g.clone())
}

// ── Inline-safe whitelist ───────────────────────────────────────────────

/// Commands that produce short, read-only output and should stay inline.
fn inline_safe_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "ls", "cat", "head", "tail", "wc", "file", "stat", "du", "df", "free", "uptime",
            "uname", "hostname", "whoami", "id", "date", "cal", "echo", "printf", "pwd", "which",
            "where", "whereis", "type", "env", "printenv", "locale", "lsblk", "lscpu", "lsusb",
            "lspci", "ip", "ss", "dig", "nslookup", "ping", // single ping is quick
        ]
        .into_iter()
        .collect()
    })
}

/// Two-word commands that should stay inline (e.g. "git status", "docker ps").
fn inline_safe_two_words() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "git status",
            "git log",
            "git diff",
            "git branch",
            "git remote",
            "git show",
            "git tag",
            "git stash list",
            "docker ps",
            "docker images",
            "docker inspect",
            "cargo --version",
            "node --version",
            "python --version",
            "go version",
            "rustc --version",
            "npm --version",
        ]
        .into_iter()
        .collect()
    })
}

/// Check if a command should run inline (captured output) vs in a terminal window.
fn is_inline_safe(cmd: &str) -> bool {
    let trimmed = cmd.trim();

    // Check two-word match first (e.g. "git status")
    let words: Vec<&str> = trimmed.splitn(3, char::is_whitespace).collect();
    if words.len() >= 2 {
        let two = format!("{} {}", words[0], words[1]);
        if inline_safe_two_words().contains(two.as_str()) {
            return true;
        }
    }

    // Check single-word match
    if let Some(first_word) = words.first()
        && inline_safe_set().contains(first_word)
    {
        return true;
    }

    false
}

// ── Terminal launch ─────────────────────────────────────────────────────

/// Shell-escape a string for use in a shell command.
fn shell_escape(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_alphanumeric() || c == '/' || c == '.' || c == '-' || c == '_')
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Spawn the user's terminal emulator with a command.
///
/// The command runs in an interactive login shell so aliases/functions work.
/// After the command exits, the terminal stays open showing the exit code.
fn launch_in_terminal(
    terminal: &str,
    shell: &str,
    cmd: &str,
    cwd: Option<&str>,
) -> Result<u32, LychiError> {
    let cwd_prefix = cwd
        .map(|d| format!("cd {} && ", shell_escape(d)))
        .unwrap_or_default();

    // Wrap command: run it, then show exit code and wait for Enter
    let wrapped = format!(
        r#"{cwd_prefix}{cmd}; __ec=$?; echo ""; echo "[Process exited with code $__ec] Press Enter to close"; read"#
    );

    let term_basename = std::path::Path::new(terminal)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(terminal);

    let mut command = Command::new(terminal);

    // Terminal-specific flags for "run this command"
    match term_basename {
        "kitty" => {
            command.args(["--", shell, "-ic", &wrapped]);
        }
        "wezterm" => {
            command.args(["start", "--", shell, "-ic", &wrapped]);
        }
        "gnome-terminal" | "gnome-terminal-server" => {
            command.args(["--", shell, "-ic", &wrapped]);
        }
        _ => {
            // Most terminals (alacritty, foot, konsole, xfce4-terminal,
            // ghostty, xterm, etc.) use -e
            command.args(["-e", shell, "-ic", &wrapped]);
        }
    }

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Detach from Lychi's process group
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let child = command.spawn().map_err(|e| {
        LychiError::ExecutionFailed(format!("Failed to launch terminal '{}': {}", terminal, e))
    })?;

    let pid = child.id();

    // Don't drop the Child — that would wait for it or kill it.
    // We want the terminal process to live independently.
    std::mem::forget(child);

    tracing::info!(
        "[shell_exec] launched in terminal: {} (pid={}, cmd={})",
        terminal,
        pid,
        cmd
    );

    Ok(pid)
}

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

    /// Pre-capture the shell environment at startup so first `run` command is instant.
    pub fn warmup(shell: &str) {
        let t0 = Instant::now();
        let env = capture_shell_env(shell);
        if let Ok(mut guard) = SHELL_ENV.write() {
            *guard = Some((shell.to_string(), env));
        }
        tracing::info!(
            "[shell_exec] warmup done: {:.0}ms",
            t0.elapsed().as_secs_f64() * 1000.0
        );
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

    /// Run a command inline (captured output, displayed in Lychi's result panel).
    async fn execute_inline(
        &self,
        cmd: &str,
        cwd: Option<&str>,
    ) -> Result<ActionResult, LychiError> {
        let start = Instant::now();
        let env = self.get_env();
        let mut command = Command::new(&self.shell);
        command
            .args(["-ic", cmd])
            .env_clear()
            .envs(&env)
            .env("TERM", "xterm-256color")
            .env("COLUMNS", "120")
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .stdout(Stdio::piped());

        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }

        let output = command.output()?;
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
            executed_args: Some(cmd.to_string()),
            launch_desktop: None,
            focus_app: None,
        })
    }

    /// Try to route a command to an existing terminal via native protocol.
    ///
    /// Returns `Some(ActionResult)` on success, `None` if routing failed
    /// (caller should fall back to opening a new terminal).
    fn try_route_command(&self, cmd: &str) -> Option<ActionResult> {
        let target = match get_context_terminal() {
            Some(t) => t,
            None => {
                tracing::debug!(
                    "terminal_route: no target terminal in context, fallback=new_terminal"
                );
                return None;
            }
        };

        tracing::debug!(
            "terminal_route: attempt wm_class={} pid={} cmd={}",
            target.wm_class,
            target.pid,
            cmd
        );

        // Busy guard: don't send to a terminal running a foreground process
        #[cfg(target_os = "linux")]
        if super::terminal_send::is_terminal_busy(target.pid) {
            tracing::debug!(
                "terminal_route: busy wm_class={} pid={} fallback=new_terminal",
                target.wm_class,
                target.pid
            );
            crate::context::metrics::inc_terminal_route_busy();
            return None;
        }

        // Send command via terminal protocol
        #[cfg(target_os = "linux")]
        match super::terminal_send::send_command(&target.wm_class, target.pid, cmd) {
            Ok(()) => {
                // Focus the terminal window
                if let Some(ref wid) = target.window_id {
                    let _ = super::kwin_windows::focus_window_by_id(wid);
                } else {
                    let _ = super::kwin_windows::focus_window(&target.wm_class);
                }

                crate::context::metrics::inc_terminal_route_hit();
                tracing::info!(
                    "terminal_route: sent wm_class={} pid={} cmd={}",
                    target.wm_class,
                    target.pid,
                    cmd
                );

                Some(ActionResult {
                    success: true,
                    output: Some(format!(
                        "\u{2192} {} (pid={}): {cmd}",
                        target.wm_class, target.pid
                    )),
                    error: None,
                    duration_ms: 0,
                    routed_by: Some("terminal_routing".to_string()),
                    open_url: None,
                    needs_confirmation: None,
                    risk_level: None,
                    output_type: Some(OutputType::Status),
                    executed_args: Some(cmd.to_string()),
                    launch_desktop: None,
                    focus_app: None,
                })
            }
            Err(e) => {
                if e.contains("no send protocol") {
                    tracing::debug!(
                        "terminal_route: no_protocol wm_class={} fallback=new_terminal",
                        target.wm_class
                    );
                    crate::context::metrics::inc_terminal_route_no_protocol();
                } else {
                    tracing::warn!(
                        "terminal_route: fail wm_class={} pid={} err={} fallback=new_terminal",
                        target.wm_class,
                        target.pid,
                        e
                    );
                    crate::context::metrics::inc_terminal_route_fail();
                }
                None
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = target;
            None
        }
    }

    /// Launch a command in the user's terminal emulator.
    fn execute_in_terminal(
        &self,
        cmd: &str,
        cwd: Option<&str>,
    ) -> Result<ActionResult, LychiError> {
        let terminal = get_terminal().unwrap_or_else(|| "xterm".to_string());

        let pid = launch_in_terminal(&terminal, &self.shell, cmd, cwd)?;

        // Track the spawned process so the user can list/kill it later
        crate::process_tracker::track(pid, cmd, cwd);

        Ok(ActionResult {
            success: true,
            output: Some(format!("Running in {terminal}: {cmd}")),
            error: None,
            duration_ms: 0,
            routed_by: None,
            open_url: None,
            needs_confirmation: None,
            risk_level: None,
            output_type: Some(OutputType::Status),
            executed_args: Some(cmd.to_string()),
            launch_desktop: None,
            focus_app: None,
        })
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
                launch_desktop: None,
                focus_app: None,
            });
        }

        let cwd = get_context_cwd();

        // Smart split: inline-safe commands run in subprocess, everything else
        // opens in the user's terminal emulator (or routes to an existing one).
        if is_inline_safe(cmd) {
            self.execute_inline(cmd, cwd.as_deref()).await
        } else {
            // Try terminal routing before opening a new terminal
            let routing_mode = get_terminal_routing();
            tracing::debug!("shell_exec: routing_mode={routing_mode} for cmd={cmd}");
            if routing_mode != "off" {
                tracing::debug!("shell_exec: attempting try_route_command");
                if let Some(result) = self.try_route_command(cmd) {
                    tracing::info!("shell_exec: routed successfully");
                    return Ok(result);
                }
                tracing::debug!("shell_exec: routing failed, falling back to new terminal");
            }
            self.execute_in_terminal(cmd, cwd.as_deref())
        }
    }
}
