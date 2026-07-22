use async_trait::async_trait;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::RwLock;
use std::time::Instant;

use crate::action_registry::{ActionHandler, ActionResult, OutputType, RiskLevel};
use crate::error::LychiError;

/// Captured environment from the user's interactive login shell. A pure lazy
/// cache keyed by shell path — legitimately process-global (no per-run state,
/// refreshed only when the shell config changes). Not part of `RunEnv`.
static SHELL_ENV: RwLock<Option<(String, HashMap<String, String>)>> = RwLock::new(None);

pub use crate::action_registry::{ExecContext, OutputMode, TerminalTarget};

// ── Command validation ──────────────────────────────────────────────────

/// How much the caller has already cleared this command with the user. Passed
/// into every shell-spawn function so authorization is enforced at the point the
/// shell string is actually assembled — the single choke every handler that
/// shells out (run, script commands, ssh, fan-out) must pass through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Clearance {
    /// The user has NOT been shown/approved this exact command. Only an `Allow`
    /// verdict may run; a `Confirm` is refused (the caller should route through
    /// the Rules Engine confirmation flow instead of spawning directly).
    None,
    /// The user explicitly approved this exact command (e.g. the Rules Engine
    /// confirmation flow returned, or a script opted into `# @lychi.risk`). A
    /// `Confirm` verdict may run; `Deny` is still absolute.
    UserConfirmed,
}

/// Ask the **central decider** (`rules::shell::authorize`) whether this exact
/// shell string may run, and enforce the answer against the caller's clearance.
/// This is the last line before `sh -ic`, so no execution path can define its
/// own weaker rule than the Rules Engine.
///
/// - `Deny`    → always refused (returns `Err`), regardless of clearance.
/// - `Confirm` → refused unless the caller passed `Clearance::UserConfirmed`.
/// - `Allow`   → runs.
///
/// Returns `Err` (surfaced as a failed `ActionResult`) when execution is not
/// authorized.
fn check_shell_authorization(cmd: &str, clearance: Clearance) -> Result<(), LychiError> {
    use crate::rules::shell::{authorize, ShellDecision};
    match authorize(cmd) {
        ShellDecision::Allow => Ok(()),
        ShellDecision::Confirm { reason } if clearance == Clearance::UserConfirmed => {
            tracing::debug!(%cmd, %reason, "[shell_exec] confirm cleared by user, running");
            Ok(())
        }
        ShellDecision::Confirm { reason } => {
            tracing::warn!(%cmd, %reason, "[shell_exec] refused: needs confirmation, none granted");
            Err(LychiError::ExecutionFailed(format!(
                "Command needs confirmation and was not approved: {reason}"
            )))
        }
        ShellDecision::Deny { reason } => {
            tracing::warn!(%cmd, %reason, "[shell_exec] hard-deny blocked shell execution");
            Err(LychiError::ExecutionFailed(reason))
        }
    }
}

/// The command's first word — the executable/builtin/alias being invoked.
/// Skips leading env-var assignments (`FOO=bar cmd`) so those still resolve.
fn command_head(cmd: &str) -> Option<&str> {
    cmd.split_whitespace()
        .find(|word| !word.contains('=') || word.starts_with('='))
}

/// How a `run` command's output is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// Captured subprocess; output shown in Lychi's result panel. Opt-in
    /// (Shift+Enter) — for quick read-only commands.
    Inline,
    /// Opens the user's terminal emulator (routed to an existing one, or a
    /// fresh window). The default — handles interactive/long-running commands.
    Terminal,
}

// ── Terminal launch ─────────────────────────────────────────────────────

/// Open a command in the given terminal emulator (falls back to xterm).
/// Public within the crate so other handlers (e.g. SSH) can launch terminal
/// sessions — they pass the configured terminal from their own run env.
pub(crate) fn open_in_terminal(
    cmd: &str,
    cwd: Option<&str>,
    terminal: Option<&str>,
    clearance: Clearance,
) -> Result<u32, LychiError> {
    let terminal = terminal.unwrap_or("xterm").to_string();
    let shell = SHELL_ENV
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|(s, _)| s.clone()))
        .unwrap_or_else(|| "/bin/sh".to_string());
    launch_in_terminal(&terminal, &shell, cmd, cwd, clearance)
}

/// Return the cached login-shell env for `shell`, capturing (and caching) it on
/// first use. Shared with `ShellExec::get_env` via the same `SHELL_ENV` cache so
/// other handlers (Script Commands) run with the same environment as `run`.
pub(crate) fn cached_shell_env(shell: &str) -> HashMap<String, String> {
    if let Ok(guard) = SHELL_ENV.read()
        && let Some((cached_shell, env)) = guard.as_ref()
        && cached_shell == shell
    {
        return env.clone();
    }
    let env = capture_shell_env(shell);
    if let Ok(mut guard) = SHELL_ENV.write() {
        *guard = Some((shell.to_string(), env.clone()));
    }
    env
}

/// Run `cmd` through the login shell and capture its output, with a timeout and
/// an output-size cap — the safe reusable capture path for handlers other than
/// `run` (e.g. Script Commands). `sh -ic "<cmd>"` honors shebangs and the login
/// env. On timeout the child is killed and a truncated/error result returned.
///
/// Returns an `ActionResult` with `OutputType::Terminal` (like `execute_inline`).
pub(crate) async fn run_captured(
    shell: &str,
    cmd: &str,
    cwd: Option<&str>,
    timeout: std::time::Duration,
    max_bytes: usize,
    clearance: Clearance,
) -> Result<ActionResult, LychiError> {
    check_shell_authorization(cmd, clearance)?;
    let env = cached_shell_env(shell);
    let shell = shell.to_string();
    let cmd = cmd.to_string();
    let cwd = cwd.map(|s| s.to_string());

    // The blocking child runs on a spawn_blocking thread; the timeout races it.
    let start = Instant::now();
    let handle = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(&shell);
        command
            .args(["-ic", &cmd])
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
        command.output()
    });

    let output = match tokio::time::timeout(timeout, handle).await {
        Ok(Ok(Ok(out))) => out,
        Ok(Ok(Err(e))) => return Err(LychiError::ExecutionFailed(format!("script spawn: {e}"))),
        Ok(Err(e)) => return Err(LychiError::ExecutionFailed(format!("script task: {e}"))),
        Err(_) => {
            // Timed out — the spawn_blocking thread is detached; the child will be
            // reaped when it eventually exits. Report the timeout to the user.
            return Ok(ActionResult {
                success: false,
                error: Some(format!("Timed out after {}s", timeout.as_secs())),
                duration_ms: start.elapsed().as_millis() as u64,
                ..Default::default()
            });
        }
    };

    let duration_ms = start.elapsed().as_millis() as u64;
    let success = output.status.success();

    // Cap output at max_bytes (char-safe) to protect against a chatty script.
    let cap = |bytes: &[u8]| -> String {
        let s = String::from_utf8_lossy(bytes);
        if s.len() <= max_bytes {
            return s.into_owned();
        }
        let mut end = max_bytes;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}\n… (output truncated)", &s[..end])
    };
    let stdout = cap(&output.stdout);
    let stderr = cap(&output.stderr);

    let mut result = if success {
        // Prefer stdout; fall back to stderr (some tools print to stderr).
        if !stdout.is_empty() {
            ActionResult::ok(stdout, OutputType::Terminal)
        } else if !stderr.is_empty() {
            ActionResult::ok(stderr, OutputType::Terminal)
        } else {
            ActionResult::empty_ok()
        }
    } else {
        // Failure — surface stderr (else stdout) as the error message.
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        ActionResult {
            success: false,
            error: (!msg.is_empty()).then_some(msg),
            ..Default::default()
        }
    };
    result.duration_ms = duration_ms;
    Ok(result)
}

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

/// Terminals that CANNOT be launched with a command (no `-e`/URI/config that
/// runs an arbitrary command — confirmed for Warp; Wave/Tabby share the trait).
/// Matched on binary basename. We route around these to a working terminal.
const NO_COMMAND_LAUNCH: &[&str] = &["warp-terminal", "warp", "waveterm", "tabby"];

/// Preference order for a fallback terminal that DOES support command launch,
/// when the resolved terminal can't run a command. First one on PATH wins.
const FALLBACK_TERMINALS: &[&str] = &[
    "konsole",
    "gnome-terminal",
    "kitty",
    "alacritty",
    "wezterm",
    "foot",
    "tilix",
    "xfce4-terminal",
    "kgx",
    "ptyxis",
    "xterm",
];

/// Whether a terminal binary can't be launched with a command.
fn is_no_command_launch(term_basename: &str) -> bool {
    let lower = term_basename.to_lowercase();
    NO_COMMAND_LAUNCH.iter().any(|t| lower == *t)
}

/// Given a requested terminal that can't run a command, pick a command-capable
/// one that's actually installed. Honors `$TERMINAL` first, then the preference
/// order. Returns `None` if nothing suitable is found.
fn pick_fallback_terminal() -> Option<String> {
    if let Ok(t) = std::env::var("TERMINAL")
        && !t.is_empty()
    {
        let base = std::path::Path::new(&t)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&t);
        if !is_no_command_launch(base) && which::which(&t).is_ok() {
            return Some(t);
        }
    }
    FALLBACK_TERMINALS
        .iter()
        .find(|t| which::which(t).is_ok())
        .map(|t| t.to_string())
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
    clearance: Clearance,
) -> Result<u32, LychiError> {
    check_shell_authorization(cmd, clearance)?;
    let cwd_prefix = cwd
        .map(|d| format!("cd {} && ", shell_escape(d)))
        .unwrap_or_default();

    // Wrap command: run it, then show exit code and wait for Enter
    let wrapped = format!(
        r#"{cwd_prefix}{cmd}; __ec=$?; echo ""; echo "[Process exited with code $__ec] Press Enter to close"; read"#
    );

    let requested_basename = std::path::Path::new(terminal)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(terminal);

    // If the resolved terminal can't run a command (e.g. Warp), route to a
    // command-capable terminal that's installed instead — so `run top` opens a
    // usable terminal rather than failing.
    let terminal = if is_no_command_launch(requested_basename) {
        match pick_fallback_terminal() {
            Some(fallback) => {
                tracing::info!(
                    "[shell_exec] '{requested_basename}' can't run a command — falling back to '{fallback}'"
                );
                fallback
            }
            None => {
                return Err(LychiError::ExecutionFailed(format!(
                    "'{requested_basename}' can't run commands and no fallback terminal is installed"
                )));
            }
        }
    } else {
        terminal.to_string()
    };
    let terminal = terminal.as_str();

    let term_basename = std::path::Path::new(terminal)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(terminal);

    let mut command = Command::new(terminal);
    // Build the terminal-specific argv to run `<shell> -ic "<wrapped>"`.
    for arg in terminal_exec_args(term_basename, shell, &wrapped) {
        command.arg(arg);
    }

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Detach from Lychi's process group so it survives independently.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command.spawn().map_err(|e| {
        LychiError::ExecutionFailed(format!("Failed to launch terminal '{terminal}': {e}"))
    })?;

    let pid = child.id();

    // Grace check: `spawn()` only fails if the binary can't be exec'd at all — a
    // terminal launched with the wrong flags starts then dies in milliseconds,
    // which would otherwise be reported as success (and leak a zombie). Wait
    // briefly; if it already exited, that's a launch failure.
    std::thread::sleep(std::time::Duration::from_millis(150));
    match child.try_wait() {
        Ok(Some(status)) => {
            // Died instantly — reaped here, so no zombie. Surface a real error.
            return Err(LychiError::ExecutionFailed(format!(
                "Terminal '{terminal}' exited immediately ({status}) — likely wrong launch flags"
            )));
        }
        Ok(None) => {
            // Still alive: reap it in a detached thread when it eventually
            // exits (user closes the window), so it never becomes a zombie.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => {
            // Couldn't poll — don't leak; best-effort reap in a thread.
            tracing::debug!("[shell_exec] try_wait failed for {terminal}: {e}");
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }

    tracing::info!("[shell_exec] launched in terminal: {terminal} (pid={pid}, cmd={cmd})");

    Ok(pid)
}

/// Resolve a window's WM class to a launchable terminal binary on PATH.
///
/// WM classes often differ from the binary name (`org.gnome.terminal` →
/// `gnome-terminal`, `dev.warp.warp` → `warp`). We try known aliases and a few
/// mechanical transforms, returning the first that exists on PATH — so "run in
/// the terminal I'm already using" actually works instead of silently falling
/// back to the configured default.
pub fn terminal_binary_for_class(wm_class: &str) -> Option<String> {
    let lower = wm_class.to_lowercase();

    // Known WM-class → binary aliases (only where they genuinely differ).
    let alias = match lower.as_str() {
        "org.gnome.terminal" | "gnome-terminal-server" => Some("gnome-terminal"),
        "org.gnome.console" => Some("kgx"),
        "org.kde.konsole" | "konsole" => Some("konsole"),
        "xfce4-terminal" | "xfce4-terminal.wrapper" => Some("xfce4-terminal"),
        "org.wezfurlong.wezterm" => Some("wezterm"),
        "dev.warp.warp" => Some("warp-terminal"),
        _ => None,
    };
    if let Some(bin) = alias
        && which::which(bin).is_ok()
    {
        return Some(bin.to_string());
    }

    // Mechanical fallbacks: the class as-is, and its last dotted segment
    // (reverse-DNS classes like "com.foo.Bar" → "bar").
    let candidates = [
        lower.clone(),
        lower.rsplit('.').next().unwrap_or(&lower).to_string(),
    ];
    for cand in candidates {
        if which::which(&cand).is_ok() {
            return Some(cand);
        }
    }
    None
}

/// Argument style a terminal uses to run a command with arguments. The two
/// conventions are genuinely incompatible (research-confirmed) — a table is
/// required. `xterm -e prog arg1 arg2` (execvp remainder) is the golden
/// standard; the GTK/VTE `-e "single string"` family is the exception.
enum ArgStyle {
    /// Flag (if any) then the command + args as SEPARATE argv (execvp-style).
    Execvp(&'static [&'static str]),
    /// Flag then ONE shell-quoted string the terminal re-parses itself.
    SingleString(&'static str),
}

/// Map a terminal to how it wants a command passed, then build the argv to run
/// `<shell> -ic "<wrapped>"`. Basename-keyed; unknown terminals default to the
/// xterm golden standard (`-e` execvp).
fn terminal_exec_args(term_basename: &str, shell: &str, wrapped: &str) -> Vec<String> {
    let style = match term_basename {
        // Positional command, no flag.
        "kitty" | "foot" => ArgStyle::Execvp(&[]),
        // execvp remainder after a prefix flag.
        "wezterm" => ArgStyle::Execvp(&["start", "--"]),
        "gnome-terminal" | "gnome-terminal-server" | "kgx" | "gnome-console" | "ptyxis" => {
            ArgStyle::Execvp(&["--"])
        }
        "xfce4-terminal" | "terminator" => ArgStyle::Execvp(&["-x"]),
        // execvp remainder after `-e` (xterm golden standard + compatibles).
        "xterm" | "urxvt" | "rxvt" | "konsole" | "alacritty" | "ghostty" | "qterminal"
        | "deepin-terminal" | "rio" | "contour" | "blackbox" => ArgStyle::Execvp(&["-e"]),
        // GTK/VTE single-string `-e` (terminal re-splits the string itself).
        "mate-terminal" | "tilix" => ArgStyle::SingleString("-e"),
        // Unknown: xterm's `-e prog args` is the documented golden standard.
        _ => ArgStyle::Execvp(&["-e"]),
    };

    match style {
        ArgStyle::Execvp(flags) => {
            let mut args: Vec<String> = flags.iter().map(|s| s.to_string()).collect();
            args.push(shell.to_string());
            args.push("-ic".to_string());
            args.push(wrapped.to_string());
            args
        }
        ArgStyle::SingleString(flag) => {
            // One argument the terminal shell-splits: `<shell> -ic '<wrapped>'`.
            let single = format!("{} -ic {}", shell, shell_escape(wrapped));
            vec![flag.to_string(), single]
        }
    }
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

    /// Whether the command's first word resolves in the user's login shell —
    /// covers PATH binaries, shell builtins, aliases, and functions (exactly
    /// what execution sees, since commands run via `sh -ic`). Lets us reject an
    /// unknown command (e.g. `xyx`) *before* launching a terminal that would
    /// just flash "command not found" and vanish.
    fn command_exists(&self, head: &str) -> bool {
        let env = self.get_env();
        let probe = format!("command -v -- {} >/dev/null 2>&1", shell_escape(head));
        Command::new(&self.shell)
            .args(["-ic", &probe])
            .env_clear()
            .envs(&env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(true) // Probe failed to run → fail open, don't block.
    }

    /// Run a command inline (captured output, displayed in Lychi's result panel).
    async fn execute_inline(
        &self,
        cmd: &str,
        cwd: Option<&str>,
        clearance: Clearance,
    ) -> Result<ActionResult, LychiError> {
        check_shell_authorization(cmd, clearance)?;
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

        let mut result = match out {
            Some(body) => ActionResult::ok(body, OutputType::Terminal),
            None => ActionResult::empty_ok(),
        };
        result.success = success;
        result.error = err;
        result.duration_ms = duration_ms;
        Ok(result)
    }

    /// Try to route a command to an existing terminal via native protocol.
    ///
    /// Returns `Some(ActionResult)` on success, `None` if routing failed
    /// (caller should fall back to opening a new terminal).
    fn try_route_command(&self, ctx: &ExecContext, cmd: &str) -> Option<ActionResult> {
        let target = match ctx.terminal_target.clone() {
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

                Some(ActionResult::ok(
                    format!("\u{2192} {} (pid={}): {cmd}", target.wm_class, target.pid),
                    OutputType::Status,
                ))
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
        ctx: &ExecContext,
        cmd: &str,
        cwd: Option<&str>,
        clearance: Clearance,
    ) -> Result<ActionResult, LychiError> {
        let terminal = ctx.terminal.clone().unwrap_or_else(|| "xterm".to_string());

        let pid = launch_in_terminal(&terminal, &self.shell, cmd, cwd, clearance)?;

        // Track the spawned process so the user can list/kill it later
        crate::process_tracker::track(pid, cmd, cwd);

        Ok(ActionResult::ok(
            format!("Running in {terminal}: {cmd}"),
            OutputType::Status,
        ))
    }
}

#[async_trait]
impl ActionHandler for ShellExec {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["run"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "run"
    }

    fn description(&self) -> &str {
        "Execute a shell command"
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // The `run` pipeline is three clear steps: validate → pick mode →
        // dispatch. Everything it needs (cwd, terminal, output mode, routing
        // target) comes from the immutable per-run `ctx`.
        let cmd = args.trim();

        // 1. Validate. Empty or an unresolvable first word never launches
        //    anything — we fail in Lychi with a clear message (and the caller
        //    won't record a failed command in history).
        if cmd.is_empty() {
            return Ok(error_result("Usage: run <shell command>"));
        }
        match command_head(cmd) {
            Some(head) if self.command_exists(head) => {}
            Some(head) => {
                return Ok(error_result(&format!("{head}: command not found")));
            }
            None => return Ok(error_result("Usage: run <shell command>")),
        }

        let cwd = ctx.cwd.as_deref();

        // 2. Pick mode from the context: inline (Shift+Enter) vs terminal (default).
        let mode = match ctx.output_mode {
            OutputMode::Inline => RunMode::Inline,
            OutputMode::Terminal => RunMode::Terminal,
        };
        tracing::debug!("shell_exec: mode={mode:?} for cmd={cmd}");

        // 3. Dispatch. Reaching `execute()` means the Rules Engine already
        //    authorized this command (the executor only calls the handler on
        //    Allow, or on a Confirm the user approved), so the spawn-point
        //    decider runs with `UserConfirmed` — it re-checks the absolute `Deny`
        //    set (defense-in-depth) while honoring the already-granted confirm.
        let clearance = Clearance::UserConfirmed;
        match mode {
            RunMode::Inline => self.execute_inline(cmd, cwd, clearance).await,
            RunMode::Terminal => {
                // Prefer routing into an already-open terminal; else a fresh one.
                if ctx.routing_mode() != "off"
                    && let Some(result) = self.try_route_command(ctx, cmd)
                {
                    return Ok(result);
                }
                self.execute_in_terminal(ctx, cmd, cwd, clearance)
            }
        }
    }
}

/// A failed `ActionResult` carrying a user-facing error message. Keeps the
/// `run` failure paths to one line instead of a full struct literal.
fn error_result(message: &str) -> ActionResult {
    ActionResult::err(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_head_skips_env_assignments() {
        assert_eq!(command_head("ls -la"), Some("ls"));
        assert_eq!(command_head("FOO=bar mycmd arg"), Some("mycmd"));
        assert_eq!(command_head("A=1 B=2 git status"), Some("git"));
        assert_eq!(command_head("   "), None);
    }

    #[test]
    fn no_command_launch_detection() {
        // Warp (both binary and short form) cannot run a command.
        assert!(is_no_command_launch("warp-terminal"));
        assert!(is_no_command_launch("warp"));
        assert!(is_no_command_launch("Warp")); // case-insensitive
        assert!(is_no_command_launch("waveterm"));
        assert!(is_no_command_launch("tabby"));
        // Normal terminals are fine.
        assert!(!is_no_command_launch("konsole"));
        assert!(!is_no_command_launch("gnome-terminal"));
        assert!(!is_no_command_launch("kitty"));
        assert!(!is_no_command_launch("xterm"));
    }

    #[test]
    fn fallback_terminal_is_command_capable() {
        // Whatever we pick as a fallback must NOT itself be a no-command-launch
        // terminal (never fall back Warp→Warp). On CI/dev at least xterm exists.
        if let Some(fallback) = pick_fallback_terminal() {
            let base = std::path::Path::new(&fallback)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&fallback);
            assert!(!is_no_command_launch(base), "fallback must be launchable");
        }
    }

    #[test]
    fn terminal_exec_args_per_terminal() {
        let sh = "/bin/zsh";
        let w = "echo hi; read";

        // xfce4-terminal: -x (execvp remainder), NOT -e — the reported bug.
        assert_eq!(
            terminal_exec_args("xfce4-terminal", sh, w),
            vec!["-x", sh, "-ic", w]
        );
        // terminator also uses -x.
        assert_eq!(terminal_exec_args("terminator", sh, w)[0], "-x");
        // gnome-terminal / kgx: -- (execvp), never deprecated -e.
        assert_eq!(
            terminal_exec_args("gnome-terminal", sh, w),
            vec!["--", sh, "-ic", w]
        );
        assert_eq!(terminal_exec_args("kgx", sh, w)[0], "--");
        // wezterm: start -- prefix.
        assert_eq!(
            terminal_exec_args("wezterm", sh, w),
            vec!["start", "--", sh, "-ic", w]
        );
        // kitty / foot: positional, no flag.
        assert_eq!(terminal_exec_args("kitty", sh, w), vec![sh, "-ic", w]);
        assert_eq!(terminal_exec_args("foot", sh, w), vec![sh, "-ic", w]);
        // xterm golden standard + compatibles: -e execvp.
        assert_eq!(
            terminal_exec_args("konsole", sh, w),
            vec!["-e", sh, "-ic", w]
        );
        assert_eq!(terminal_exec_args("alacritty", sh, w)[0], "-e");
        assert_eq!(terminal_exec_args("ghostty", sh, w)[0], "-e");
        // Unknown terminal: default to the xterm golden standard (-e execvp).
        assert_eq!(
            terminal_exec_args("some-new-term", sh, w),
            vec!["-e", sh, "-ic", w]
        );
    }

    #[test]
    fn terminal_exec_args_single_string_family() {
        let sh = "/bin/zsh";
        let w = "echo hi; read";
        // mate-terminal / tilix: ONE shell-quoted string after -e.
        let args = terminal_exec_args("mate-terminal", sh, w);
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "-e");
        // The single string embeds the shell invocation, quoting the wrapped cmd.
        assert!(args[1].starts_with("/bin/zsh -ic "));
        assert!(args[1].contains("echo hi"));
    }

    #[test]
    fn exec_context_output_mode_and_routing() {
        // Output mode + routing come from the immutable per-call context — no
        // global flag, so no cross-test interference (this used to need a lock).
        let inline = ExecContext {
            output_mode: OutputMode::Inline,
            ..Default::default()
        };
        assert_eq!(inline.output_mode, OutputMode::Inline);

        let default = ExecContext::default();
        assert_eq!(default.output_mode, OutputMode::Terminal);
        assert_eq!(default.routing_mode(), "off"); // empty → off

        let routed = ExecContext {
            terminal_routing: "auto".to_string(),
            ..Default::default()
        };
        assert_eq!(routed.routing_mode(), "auto");
    }
}
