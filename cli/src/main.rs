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
        "--toggle" => {
            let path = socket_path();
            match UnixStream::connect(&path) {
                Ok(mut stream) => {
                    let _ = stream.write_all(b"toggle\n");
                }
                Err(e) => {
                    eprintln!("Lychi is not running ({})", e);
                    std::process::exit(1);
                }
            }
        }
        "--help" | "-h" => {
            println!("Lychi CLI");
            println!("  --toggle    Toggle the Lychi launcher window");
        }
        other => {
            eprintln!("Unknown argument: {other}");
            eprintln!("Usage: lychi --toggle");
            std::process::exit(1);
        }
    }
}
