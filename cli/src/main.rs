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
        eprintln!("Usage: lychi --toggle");
        std::process::exit(1);
    }

    match args[1].as_str() {
        "--toggle" | "toggle" => {
            send_command("toggle\n");
        }
        // Fire a screenshot without opening the launcher. Optional mode arg:
        // `lychi --screenshot` (full), `--screenshot area`, `--screenshot window`.
        // Bind this to a key in your desktop's shortcut settings for a global
        // capture hotkey — same idea as `lychi --toggle`.
        "--screenshot" | "screenshot" => {
            let mode = args.get(2).map(|s| s.as_str()).unwrap_or("");
            send_command(&format!("screenshot {mode}\n"));
        }
        "--help" | "-h" => {
            println!("Lychi CLI");
            println!("  toggle, --toggle              Toggle the Lychi launcher window");
            println!("  screenshot, --screenshot      Capture full screen (no window)");
            println!("  --screenshot area|window      Capture a region / the active window");
        }
        other => {
            eprintln!("Unknown argument: {other}");
            eprintln!("Usage: lychi --toggle | --screenshot [area|window]");
            std::process::exit(1);
        }
    }
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
