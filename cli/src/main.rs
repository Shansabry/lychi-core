use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

fn socket_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/tmp/lychi-{}", unsafe { libc::getuid() }));
    PathBuf::from(runtime_dir).join("lychi.sock")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: lychi start | --toggle | --screenshot [area|window] | --ai [preset]");
        std::process::exit(1);
    }

    match parse(&args) {
        Some(Verb::Send(cmd)) => send_command(&cmd),
        Some(Verb::Help) => print_help(),
        // `start` can't be served over the socket — the socket only exists once
        // the app is up. This binary is the CLI half of a packaged install, so
        // it launches the GUI half; the AppImage handles `start` by simply
        // running itself.
        Some(Verb::Start) => {
            if UnixStream::connect(socket_path()).is_ok() {
                return; // already running — nothing to do
            }
            let exe = gui_binary();
            if let Err(e) = spawn_detached(&exe) {
                eprintln!("Could not start Lychi ({exe}): {e}");
                std::process::exit(1);
            }
        }
        None => {
            eprintln!("Unknown argument: {}", args[1]);
            eprintln!("Usage: lychi start | --toggle | --screenshot [area|window] | --ai [preset]");
            std::process::exit(1);
        }
    }
}

/// What a parsed argv means.
enum Verb {
    /// A line to write to the running instance's socket.
    Send(String),
    /// Launch the GUI (can't go over the socket — it may not exist yet).
    Start,
    Help,
}

/// Map argv to a verb.
///
/// Kept as a pure function so the socket strings are testable. `src-tauri`'s
/// `cli_command` must produce byte-identical lines for the shared verbs — the
/// AppImage is the same CLI when it's the only install — and a divergence there
/// would fail silently at runtime, so both sides assert the same literals.
fn parse(args: &[String]) -> Option<Verb> {
    let verb = args.get(1)?.as_str();
    let arg = args.get(2).map(|s| s.as_str()).unwrap_or("");
    match verb {
        "--toggle" | "toggle" => Some(Verb::Send("toggle\n".to_string())),
        // Fire a screenshot without opening the launcher. Optional mode arg:
        // `lychi --screenshot` (full), `--screenshot area`, `--screenshot window`.
        // Bind this to a key in your desktop's shortcut settings for a global
        // capture hotkey — same idea as `lychi --toggle`.
        "--screenshot" | "screenshot" => Some(Verb::Send(format!("screenshot {arg}\n"))),
        // Run an AI action on whatever text is selected in the focused window,
        // without the user first opening the launcher. Optional preset keyword:
        // `lychi --ai` (ask about it) or `--ai summarize`. Bind to a desktop
        // shortcut for "select text anywhere → AI".
        "--ai" | "ai" => Some(Verb::Send(format!("ai {arg}\n"))),
        "--help" | "-h" => Some(Verb::Help),
        "start" | "--start" => Some(Verb::Start),
        _ => None,
    }
}

fn print_help() {
    // Rust ignores SIGPIPE, so `println!` errors instead of exiting when the
    // reader goes away — `lychi --help | head` would print a panic. Standard
    // Unix tools die quietly; restore that.
    //
    // SAFETY: single-threaded — this runs before anything is spawned.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    println!("Lychi CLI");
    println!("  toggle, --toggle              Toggle the Lychi launcher window");
    println!("  screenshot, --screenshot      Capture full screen (no window)");
    println!("  --screenshot area|window      Capture a region / the active window");
    println!("  ai, --ai [preset]             Run AI on the selected text");
    println!("  start, --start                Start Lychi (no-op if running)");
    println!();
    println!("Every verb except start needs Lychi already running.");
}

/// Launch the GUI as a detached background process.
///
/// Two things have to be true for `lychi start` to behave like a service
/// command rather than a foreground app, and plain `spawn()` gives neither:
///
/// - **Its own session.** A child of this CLI shares the terminal's process
///   group, so closing the terminal (or Ctrl-C) takes Lychi down with it.
///   `setsid()` detaches it from the controlling terminal.
/// - **Discarded stdio.** The app logs to stderr; inherited, that spews boot
///   logs over the user's prompt long after `start` has returned. Logs already
///   go to `~/.local/share/lychi/logs`, which is where they belong.
fn spawn_detached(exe: &str) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let mut cmd = std::process::Command::new(exe);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: `setsid` is async-signal-safe and the only pre-exec work done
    // here. Failure means the child keeps the terminal's session — worse, but
    // not fatal — so the result is deliberately ignored.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().map(|_| ())
}

/// Resolve the GUI binary to spawn for `start`.
///
/// PATH first: a distro package, a `cargo install`, or a user-local build all
/// put `lychi-app` on PATH, and any of those is a more correct answer than a
/// path baked in at compile time. `/opt/lychi/Lychi.AppImage` is the fallback
/// for the AppImage install layout, which puts nothing on PATH but the CLI
/// symlink. Bare `lychi-app` is the last resort so the spawn error names
/// something meaningful when neither exists.
fn gui_binary() -> String {
    if let Some(found) = which_on_path("lychi-app") {
        return found;
    }
    const APPIMAGE: &str = "/opt/lychi/Lychi.AppImage";
    if std::path::Path::new(APPIMAGE).exists() {
        return APPIMAGE.to_string();
    }
    "lychi-app".to_string()
}

/// Find an executable on `PATH`. Avoids depending on the `which` crate for one
/// lookup in a binary whose whole point is being tiny.
fn which_on_path(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
        .map(|p| p.to_string_lossy().into_owned())
}

fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Send a newline-terminated command to the running Lychi over its Unix socket.
fn send_command(cmd: &str) {
    let path = socket_path();
    match UnixStream::connect(&path) {
        Ok(mut stream) => {
            let _ = stream.write_all(cmd.as_bytes());
        }
        Err(e) => {
            eprintln!("Lychi is not running ({})", e);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        std::iter::once("lychi")
            .chain(parts.iter().copied())
            .map(String::from)
            .collect()
    }

    fn sent(parts: &[&str]) -> Option<String> {
        match parse(&argv(parts)) {
            Some(Verb::Send(s)) => Some(s),
            _ => None,
        }
    }

    /// These literals are the wire contract with `ipc_server.rs` AND with
    /// `src-tauri/src/main.rs`'s `cli_command`. Written out by hand rather than
    /// derived, so a change to either side has to change this test too.
    #[test]
    fn socket_lines_are_exact() {
        assert_eq!(sent(&["--toggle"]).as_deref(), Some("toggle\n"));
        assert_eq!(sent(&["toggle"]).as_deref(), Some("toggle\n"));
        assert_eq!(sent(&["--screenshot"]).as_deref(), Some("screenshot \n"));
        assert_eq!(
            sent(&["--screenshot", "area"]).as_deref(),
            Some("screenshot area\n")
        );
        assert_eq!(
            sent(&["screenshot", "window"]).as_deref(),
            Some("screenshot window\n")
        );
        assert_eq!(sent(&["--ai"]).as_deref(), Some("ai \n"));
        assert_eq!(
            sent(&["--ai", "summarize"]).as_deref(),
            Some("ai summarize\n")
        );
    }

    /// `start` must never become a socket send: the socket doesn't exist yet
    /// when it's the verb that matters.
    #[test]
    fn start_is_not_a_socket_command() {
        for v in ["start", "--start"] {
            assert!(matches!(parse(&argv(&[v])), Some(Verb::Start)), "{v}");
        }
    }

    #[test]
    fn help_needs_no_socket() {
        for v in ["--help", "-h"] {
            assert!(matches!(parse(&argv(&[v])), Some(Verb::Help)), "{v}");
        }
    }

    /// Unknown verbs and a bare invocation are errors here — unlike the
    /// AppImage, where the same argv means "just launch the GUI".
    #[test]
    fn unknown_and_empty_are_rejected() {
        assert!(parse(&argv(&[])).is_none());
        assert!(parse(&argv(&["--nope"])).is_none());
        assert!(parse(&argv(&["lychi://open/x"])).is_none());
    }

    #[test]
    fn extra_args_beyond_the_mode_are_ignored() {
        assert_eq!(
            sent(&["--screenshot", "area", "junk"]).as_deref(),
            Some("screenshot area\n")
        );
    }

    #[test]
    fn falls_back_when_nothing_is_on_path() {
        // SAFETY: single-threaded test, restored immediately.
        let saved = std::env::var_os("PATH");
        unsafe { std::env::set_var("PATH", "") };
        let resolved = gui_binary();
        if let Some(p) = saved {
            unsafe { std::env::set_var("PATH", p) };
        }
        // With an empty PATH the only candidates are the AppImage location or
        // the bare name; either way it must never return an empty string.
        assert!(!resolved.is_empty());
        assert!(resolved.ends_with("lychi-app") || resolved.ends_with("Lychi.AppImage"));
    }
}
