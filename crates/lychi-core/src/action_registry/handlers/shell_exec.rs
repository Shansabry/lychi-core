use async_trait::async_trait;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::RwLock;
use std::time::Instant;

use crate::action_registry::{ActionHandler, ActionResult, CommandCategory, OutputType, RiskLevel};
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
    use crate::rules::shell::{ShellDecision, authorize};
    match authorize(cmd) {
        ShellDecision::Allow => Ok(()),
        ShellDecision::Confirm { reason } if clearance == Clearance::UserConfirmed => {
            tracing::debug!(%cmd, %reason, "[shell_exec] confirm cleared by user, running");
            Ok(())
        }
        ShellDecision::Confirm { reason } => {
            // Command text in a warn line ships in the default-level log file —
            // scrub token shapes (the reason only names the matched pattern).
            tracing::warn!(
                cmd = %crate::text::scrub_secrets(cmd),
                %reason,
                "[shell_exec] refused: needs confirmation, none granted"
            );
            Err(LychiError::ExecutionFailed(format!(
                "Command needs confirmation and was not approved: {reason}"
            )))
        }
        ShellDecision::Deny { reason } => {
            tracing::warn!(
                cmd = %crate::text::scrub_secrets(cmd),
                %reason,
                "[shell_exec] hard-deny blocked shell execution"
            );
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

/// How long an inline `run` may hold its result panel before it is killed.
///
/// Generous on purpose: inline is for commands whose output you want in the
/// launcher, and some legitimate ones (a build step, a slow curl) take a
/// minute. What it must never be is *unbounded* — the runtime has four
/// workers, and an inline `tail -f` used to hold one (plus the executor read
/// guard) until app restart. Anything longer-running belongs in a terminal,
/// which is what the timeout message says.
const INLINE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Output cap for inline `run`. The result is one String shipped over IPC into
/// the WebView; uncapped, a chatty command materialised its entire output in
/// memory and then asked WebKitGTK to lay it out. 256KB is far more than the
/// panel can usefully show and far less than what jams the bridge.
const INLINE_MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// What the one capture core produced: the command's capped output, or the fact
/// that it was killed at the timeout. Callers shape this into their own
/// `ActionResult` — presentation differs per surface, safety must not.
enum Capture {
    Completed(CapturedOutput),
    /// The deadline passed. The shell and its whole process group have been
    /// SIGKILLed by the time this is returned — "reported stopped" and
    /// "actually stopped" are the same fact.
    TimedOut,
}

struct CapturedOutput {
    /// Capped at `max_bytes`, char-safe.
    stdout: String,
    /// Capped at `max_bytes`, char-safe.
    stderr: String,
    success: bool,
    duration_ms: u64,
}

/// The ONE shell-capture core: run `cmd` through the login shell, race a
/// timeout, cap output. Every captured execution (`run` inline, Script
/// Commands) goes through here — there used to be a second, bespoke capture in
/// `execute_inline` with no timeout and no cap, which is exactly how a `run
/// tail -f` permanently ate one of the four runtime workers.
///
/// On timeout the child is genuinely killed, not abandoned: `kill_on_drop`
/// takes the shell when the wait future is dropped, and the process group
/// (`process_group(0)` gives the shell its own) is SIGKILLed so a pipeline's
/// children die with it. The previous implementation reported "Timed out"
/// while the child ran on — a user who retried a mutating script then had two
/// instances running.
async fn capture_shell_output(
    shell: &str,
    cmd: &str,
    cwd: Option<&str>,
    env: HashMap<String, String>,
    timeout: std::time::Duration,
    max_bytes: usize,
) -> Result<Capture, LychiError> {
    let start = Instant::now();
    let mut command = tokio::process::Command::new(shell);
    command
        .args(["-ic", cmd])
        .env_clear()
        .envs(&env)
        .env("TERM", "xterm-256color")
        .env("COLUMNS", "120")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        // Its own process group: the timeout must be able to kill the shell
        // AND everything it spawned, not orphan a pipeline's children.
        .process_group(0)
        // If the wait future is dropped (timeout), kill rather than orphan.
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }

    let child = command
        .spawn()
        .map_err(|e| LychiError::ExecutionFailed(format!("shell spawn: {e}")))?;
    let pid = child.id();

    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(LychiError::ExecutionFailed(format!("shell wait: {e}"))),
        Err(_) => {
            // The dropped wait future has SIGKILLed the shell (kill_on_drop).
            // Take the rest of its process group too — the group leader's pid
            // is the child's own, courtesy of process_group(0) above.
            if let Some(pid) = pid {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(-(pid as i32)),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
            return Ok(Capture::TimedOut);
        }
    };

    // Cap output at max_bytes (char-safe) to protect against a chatty command.
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

    Ok(Capture::Completed(CapturedOutput {
        stdout: cap(&output.stdout),
        stderr: cap(&output.stderr),
        success: output.status.success(),
        duration_ms: start.elapsed().as_millis() as u64,
    }))
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
    let start = Instant::now();

    let CapturedOutput {
        stdout,
        stderr,
        success,
        duration_ms,
    } = match capture_shell_output(shell, cmd, cwd, env, timeout, max_bytes).await? {
        Capture::Completed(out) => out,
        Capture::TimedOut => {
            return Ok(ActionResult {
                success: false,
                error: Some(format!(
                    "Timed out after {}s — command killed",
                    timeout.as_secs()
                )),
                duration_ms: start.elapsed().as_millis() as u64,
                ..Default::default()
            });
        }
    };

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

    // Prefer the freedesktop standard when it is installed.
    //
    // `xdg-terminal-exec` implements the Default Terminal Execution
    // Specification: it reads the user's own `xdg-terminals.list`, honours the
    // `X-TerminalArgExec` key each terminal declares, and therefore needs no
    // per-terminal flag knowledge from us. Where present it is strictly better
    // than a hand-maintained table — it reflects what the USER configured.
    //
    // It cannot be the only path: the spec is still a proposal and the tool is
    // not installed by default (verified absent on Fedora 44), so the table
    // below remains the fallback. Preferred-when-present, never depended upon.
    let use_xdg = which::which("xdg-terminal-exec").is_ok();
    let (program, argv): (&str, Vec<String>) = if use_xdg {
        (
            "xdg-terminal-exec",
            vec![shell.to_string(), "-ic".to_string(), wrapped.clone()],
        )
    } else {
        (terminal, terminal_exec_args(term_basename, shell, &wrapped))
    };

    let mut command = Command::new(program);
    for arg in argv {
        command.arg(arg);
    }

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // stderr is CAPTURED, not discarded: when a terminal really does fail
        // to start it prints the reason there ("Failed to parse arguments: …",
        // "Failed to execute child process …"). Throwing it away is what forced
        // the old code to guess "likely wrong launch flags" — a guess that was
        // wrong for the bug that motivated this rewrite.
        .stderr(Stdio::piped());

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

    // Observe the outcome; never block on it.
    //
    // The old code slept 150ms and treated ANY exit within that window as a
    // launch failure. That is wrong for client/server terminals: `gnome-terminal`
    // hands the request to `gnome-terminal-server` over D-Bus and the client
    // exits ~96ms later WITH STATUS 0, while the real window stays open. Measured
    // on GNOME: 96ms, 97ms, 444ms — straddling the 150ms threshold, so the same
    // machine both worked and "failed" depending on load. Berin's `ssh nimbus`
    // hit the fast case and reported failure for a terminal that had opened fine.
    //
    // The exit STATUS answers what the timing cannot:
    //   - exited 0        → success (client handed off, or command already done)
    //   - exited non-zero → real failure; report its stderr
    //   - still running   → success (long-lived terminal)
    //
    // No list of which terminals are client/server. A name list would need
    // editing for every terminal that adopts the pattern; the process already
    // tells us what happened.
    // Name what we actually spawned — under xdg-terminal-exec the failing
    // process is the dispatcher, not the terminal we would have picked, and a
    // message naming the wrong binary sends the next reader to the wrong place.
    let terminal_owned = program.to_string();
    std::thread::spawn(move || {
        let stderr = child.stderr.take();
        match child.wait() {
            Ok(status) if status.success() => {
                tracing::debug!("[shell_exec] '{terminal_owned}' client exited cleanly");
            }
            Ok(status) => {
                // A real launch failure. Read what the terminal actually said
                // instead of guessing, and tell the user — the launcher window
                // is long gone by now, so a toast is the only surface left.
                let detail = stderr
                    .and_then(|mut s| {
                        use std::io::Read;
                        let mut buf = String::new();
                        s.read_to_string(&mut buf).ok().map(|_| buf)
                    })
                    .unwrap_or_default();
                let detail = detail
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("no output")
                    .trim()
                    .to_string();
                tracing::warn!("[shell_exec] '{terminal_owned}' failed ({status}): {detail}");
                crate::notify::show(crate::notify::Toast::new(
                    format!("Could not open {terminal_owned}"),
                    detail,
                ));
            }
            Err(e) => tracing::debug!("[shell_exec] wait failed for {terminal_owned}: {e}"),
        }
    });

    // The command text stays out of the default-level (shareable) log; the
    // scrubbed form is available at debug for local diagnosis.
    tracing::info!("[shell_exec] launched in terminal: {terminal} (pid={pid})");
    tracing::debug!(
        "[shell_exec] terminal cmd: {}",
        crate::text::scrub_secrets(cmd)
    );

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
    candidates
        .into_iter()
        .find(|cand| which::which(cand).is_ok())
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
    ///
    /// Capture goes through the one core (`capture_shell_output`) — this used
    /// to carry its own synchronous `command.output()` with no timeout and no
    /// output cap, blocking a runtime worker for as long as the command ran.
    /// The runtime has four workers; one `run tail -f` ate a quarter of the
    /// app's async capacity forever, while also holding the executor guard.
    async fn execute_inline(
        &self,
        cmd: &str,
        cwd: Option<&str>,
        clearance: Clearance,
    ) -> Result<ActionResult, LychiError> {
        check_shell_authorization(cmd, clearance)?;
        let start = Instant::now();
        let env = self.get_env();

        let CapturedOutput {
            stdout,
            stderr,
            success,
            duration_ms,
        } = match capture_shell_output(
            &self.shell,
            cmd,
            cwd,
            env,
            INLINE_TIMEOUT,
            INLINE_MAX_OUTPUT_BYTES,
        )
        .await?
        {
            Capture::Completed(out) => out,
            Capture::TimedOut => {
                return Ok(ActionResult {
                    success: false,
                    error: Some(format!(
                        "Timed out after {}s — command killed. Long-running \
                         commands belong in a terminal.",
                        INLINE_TIMEOUT.as_secs()
                    )),
                    duration_ms: start.elapsed().as_millis() as u64,
                    ..Default::default()
                });
            }
        };

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
                    "terminal_route: sent wm_class={} pid={}",
                    target.wm_class,
                    target.pid
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
    fn category(&self) -> CommandCategory {
        CommandCategory::Developer
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

    /// Minimal env for capture tests: enough PATH to find `sleep`/`head`,
    /// nothing from the host (the core does `env_clear`).
    fn test_env() -> HashMap<String, String> {
        HashMap::from([(
            "PATH".to_string(),
            "/usr/bin:/bin:/usr/local/bin".to_string(),
        )])
    }

    /// Is any process on the system running with `marker` in its cmdline?
    fn proc_running_with(marker: &str) -> bool {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return false;
        };
        for entry in entries.flatten() {
            if !entry
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
            {
                continue;
            }
            if let Ok(cmdline) = std::fs::read(entry.path().join("cmdline"))
                && String::from_utf8_lossy(&cmdline).contains(marker)
            {
                return true;
            }
        }
        false
    }

    /// EXEC-1/6: "reported stopped" and "actually stopped" must be the same
    /// fact. The old timeout arm returned "Timed out" while the detached child
    /// ran to completion — a user who retried a mutating script then had two
    /// instances running. The kill must take the process GROUP: killing only
    /// the shell orphans a pipeline's children.
    #[tokio::test(flavor = "multi_thread")]
    async fn capture_timeout_kills_the_whole_process_group() {
        // The marker rides in the shell's -c string, so both the shell and the
        // sleep it spawns are findable (group members share the fate).
        let marker = format!("lychi-exec16-test-{}", std::process::id());
        let cmd = format!("sleep 300 # {marker}");

        let started = Instant::now();
        let res = capture_shell_output(
            "/bin/sh",
            &cmd,
            None,
            test_env(),
            std::time::Duration::from_millis(200),
            4096,
        )
        .await
        .expect("capture must not error on timeout");

        assert!(matches!(res, Capture::TimedOut));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "timeout must return promptly, not wait out the child"
        );

        // SIGKILL delivery is immediate but reaping is async — poll briefly.
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while proc_running_with(&marker) && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            !proc_running_with(&marker),
            "the child survived the timeout — the kill contract is broken \
             (docstring promises the child dies with the deadline)"
        );
    }

    /// The cap must hold for both streams, char-safely, with the truncation
    /// marker — the inline path used to materialise unbounded output.
    #[tokio::test(flavor = "multi_thread")]
    async fn capture_caps_output() {
        let res = capture_shell_output(
            "/bin/sh",
            "head -c 100000 /dev/zero | tr '\\0' 'a'",
            None,
            test_env(),
            std::time::Duration::from_secs(30),
            1000,
        )
        .await
        .expect("capture failed");

        let Capture::Completed(out) = res else {
            panic!("command should complete well within the timeout");
        };
        assert!(out.success);
        assert!(
            out.stdout.len() < 1100,
            "cap not applied: {}",
            out.stdout.len()
        );
        assert!(out.stdout.ends_with("(output truncated)"));
    }

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

    /// Terminal launch must be decided by OUTCOME, not by elapsed time.
    ///
    /// The bug (Berin, GNOME Wayland, 2026-08-03): `ssh nimbus` reported
    /// "Terminal 'gnome-terminal' exited immediately (exit status: 4) — likely
    /// wrong launch flags" while the terminal had in fact opened.
    ///
    /// `gnome-terminal` is a CLIENT: it hands the request to
    /// `gnome-terminal-server` over D-Bus and exits, leaving the real window
    /// owned by the server. Measured client lifetimes on GNOME: 96ms, 97ms,
    /// 444ms — straddling the old 150ms grace window, so the same machine both
    /// worked and "failed" depending on load.
    ///
    /// These pin the decision rule so a future "just bump the sleep" cannot
    /// come back — no sleep length is correct, because the premise was wrong.
    mod launch_outcome {
        use std::process::{Command, Stdio};

        /// The rule the observer thread implements.
        fn is_launch_failure(status: std::process::ExitStatus) -> bool {
            !status.success()
        }

        #[test]
        fn a_fast_clean_exit_is_success_not_failure() {
            // Stand-in for the client/server handoff: exits immediately, 0.
            // Under the old rule this was a "launch failure" purely because it
            // finished inside the grace window.
            let status = Command::new("/bin/true")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("spawn /bin/true");
            assert!(status.success());
            assert!(
                !is_launch_failure(status),
                "a client that exits 0 has handed off successfully"
            );
        }

        #[test]
        fn a_non_zero_exit_is_a_real_failure() {
            // The other half: a validator that called everything success would
            // pass the test above while hiding genuine launch failures.
            let status = Command::new("/bin/false")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("spawn /bin/false");
            assert!(is_launch_failure(status), "non-zero exit is a real failure");
        }

        #[test]
        fn the_rule_does_not_consult_timing() {
            // Both processes exit far inside any plausible grace window; only
            // the status distinguishes them. If someone reintroduces a
            // duration-based check, these two become indistinguishable.
            let quick_ok = Command::new("/bin/true").status().unwrap();
            let quick_bad = Command::new("/bin/false").status().unwrap();
            assert_ne!(
                is_launch_failure(quick_ok),
                is_launch_failure(quick_bad),
                "status must separate these; elapsed time cannot"
            );
        }
    }
}
