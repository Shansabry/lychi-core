#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Multi-call dispatch: `Lychi.AppImage --toggle` (or `toggle`) acts as the
    // CLI — poke the running instance over its socket and exit, without
    // starting a second app. Makes the DE-shortcut command work with nothing
    // installed on PATH.
    #[cfg(unix)]
    if std::env::args().any(|a| a == "--toggle" || a == "toggle") {
        use std::io::Write;
        let path = lychi_app::ipc_socket_path();
        match std::os::unix::net::UnixStream::connect(&path) {
            Ok(mut stream) => {
                let _ = stream.write_all(b"toggle\n");
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
