#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Multi-call dispatch: the app binary doubles as the CLI. A verb pokes the
    // running instance over its socket and exits, without starting a second
    // app — so a DE shortcut (or `/usr/local/bin/lychi -> Lychi.AppImage`)
    // works with nothing else installed on PATH.
    //
    // Covers EVERY verb the standalone `lychi-cli` accepts, not just `toggle`.
    // When the AppImage is the only install, `lychi --help` and
    // `lychi --screenshot area` have to work too; previously they fell through
    // and launched a second GUI, which is the opposite of what was asked.
    #[cfg(unix)]
    if let Some(cmd) = cli_command(&std::env::args().collect::<Vec<_>>()) {
        // `doctor` is answered HERE, not forwarded over the socket: it exists
        // to diagnose a launcher that will not start or will not bind a
        // hotkey, so requiring a running instance would make it useless in
        // exactly the case it is for.
        if cmd == "--doctor" {
            unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
            print!("{}", lychi_core::context::doctor::report());
            return;
        }
        if cmd == "--help" {
            // Rust ignores SIGPIPE so `println!` returns an error instead of
            // exiting — which makes `lychi --help | head` print a panic. Every
            // standard Unix tool just dies quietly when the reader goes away.
            // Restored only on this CLI path; the GUI keeps Rust's default.
            //
            // SAFETY: single-threaded here — nothing has spawned yet.
            unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
            print_cli_help();
            return;
        }
        use std::io::Write;
        let path = lychi_app::ipc_socket_path();
        match std::os::unix::net::UnixStream::connect(&path) {
            Ok(mut stream) => {
                let _ = stream.write_all(cmd.as_bytes());
                return;
            }
            Err(e) => {
                eprintln!("Lychi is not running ({e})");
                std::process::exit(1);
            }
        }
    }

    // The AppImage's AppRun exports GDK_BACKEND=x11 unconditionally, which
    // silently forces XWayland and disables the layer-shell window path.
    // Reclaim native Wayland before GTK initializes; x11 stays as fallback.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        // SAFETY: single-threaded here — no other threads exist before run().
        unsafe { std::env::set_var("GDK_BACKEND", "wayland,x11") };
    }

    // NOTE: deliberately NOT auto-setting WEBKIT_DISABLE_DMABUF_RENDERER on
    // NVIDIA. Detection by /proc/driver/nvidia/version misfires on hybrid
    // AMD+NVIDIA laptops (iGPU renders, workaround not needed) and forcing
    // modern WebKitGTK (2.46+) onto the legacy path causes stale-frame
    // ghosting. Users with genuine NVIDIA blank-window issues can set the
    // env var themselves — we never override an explicit value.

    lychi_app::run();
}

/// Map argv to the socket line the running instance expects, or `None` when
/// this is a normal GUI launch.
///
/// Mirrors `cli/src/main.rs` — the standalone binary is still what a packaged
/// install puts on PATH; this exists so an AppImage-only install behaves the
/// same. `--help` is returned as a sentinel and handled without a socket,
/// since printing usage must work whether or not the app is running.
#[cfg(unix)]
fn cli_command(args: &[String]) -> Option<String> {
    let verb = args.get(1)?.as_str();
    let arg = args.get(2).map(|s| s.as_str()).unwrap_or("");
    match verb {
        "--toggle" | "toggle" => Some("toggle\n".to_string()),
        "--screenshot" | "screenshot" => Some(format!("screenshot {arg}\n")),
        "--ai" | "ai" => Some(format!("ai {arg}\n")),
        "--help" | "-h" => Some("--help".to_string()),
        "doctor" | "--doctor" => Some("--doctor".to_string()),
        // `start` / `--hidden` are NOT socket commands — they mean "run the
        // app". Listed explicitly so the intent is obvious at the call site and
        // so `start` can't be mistaken for a missing verb.
        "start" | "--start" | "--hidden" => None,
        // Anything else (including no args, and the `%u` deep-link URLs the
        // desktop entry passes) is a normal launch.
        _ => None,
    }
}

#[cfg(unix)]
fn print_cli_help() {
    println!("Lychi CLI");
    println!("  toggle, --toggle              Toggle the Lychi launcher window");
    println!("  screenshot, --screenshot      Capture full screen (no window)");
    println!("  --screenshot area|window      Capture a region / the active window");
    println!("  ai, --ai [preset]             Run AI on the selected text");
    println!("  start, --start                Start Lychi (no-op if running)");
    println!("  doctor, --doctor              Report desktop + capabilities (no app needed)");
    println!("  --hidden                      Start in the background (autostart)");
    println!();
    println!("Every verb except start/doctor/--hidden needs Lychi already running.");
}

#[cfg(all(unix, test))]
mod tests {
    use super::cli_command;

    fn argv(parts: &[&str]) -> Vec<String> {
        std::iter::once("Lychi.AppImage")
            .chain(parts.iter().copied())
            .map(String::from)
            .collect()
    }

    /// Byte-identical to `cli/src/main.rs`'s `socket_lines_are_exact`. Both are
    /// hand-written rather than shared: a helper crate would let a single edit
    /// change both sides at once, which is the exact drift this guards against.
    #[test]
    fn socket_lines_match_the_standalone_cli() {
        assert_eq!(
            cli_command(&argv(&["--toggle"])).as_deref(),
            Some("toggle\n")
        );
        assert_eq!(cli_command(&argv(&["toggle"])).as_deref(), Some("toggle\n"));
        assert_eq!(
            cli_command(&argv(&["--screenshot"])).as_deref(),
            Some("screenshot \n")
        );
        assert_eq!(
            cli_command(&argv(&["--screenshot", "area"])).as_deref(),
            Some("screenshot area\n")
        );
        assert_eq!(
            cli_command(&argv(&["screenshot", "window"])).as_deref(),
            Some("screenshot window\n")
        );
        assert_eq!(cli_command(&argv(&["--ai"])).as_deref(), Some("ai \n"));
        assert_eq!(
            cli_command(&argv(&["--ai", "summarize"])).as_deref(),
            Some("ai summarize\n")
        );
    }

    /// The bug this whole change exists to fix: `--help` used to fall through
    /// and launch a GUI. It must be intercepted, and without a socket.
    #[test]
    fn help_is_intercepted_not_launched() {
        for v in ["--help", "-h"] {
            assert_eq!(cli_command(&argv(&[v])).as_deref(), Some("--help"), "{v}");
        }
    }

    /// `None` means "launch the GUI". A bare launch, the desktop entry's `%u`
    /// deep links, and the start/autostart verbs all have to land here — if any
    /// became a socket command, double-clicking the icon would silently do
    /// nothing when the app wasn't already running.
    #[test]
    fn launch_verbs_and_unknown_args_start_the_gui() {
        for a in [
            vec![],
            vec!["start"],
            vec!["--start"],
            vec!["--hidden"],
            vec!["lychi://open/thing"],
            vec!["--some-tauri-flag"],
        ] {
            assert!(cli_command(&argv(&a)).is_none(), "{a:?}");
        }
    }
}
