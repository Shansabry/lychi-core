//! Live end-to-end check of the clipboard sensitivity hint against a real
//! compositor selection.
//!
//! `#[ignore]`d because it needs a Wayland session with `wl-clipboard`, and it
//! clobbers the user's clipboard. Run deliberately:
//!
//! ```text
//! cargo test -p lychi-core --test clipboard_sensitive_live -- --ignored --nocapture
//! ```
//!
//! This exists because the unit tests assert against a *captured* MIME list.
//! That proves the rule, not that the list is what a real `--sensitive` copy
//! actually produces on this stack — and the whole feature rests on that.

use std::process::{Command, Stdio};

fn wl_copy(text: &str, sensitive: bool) {
    use std::io::Write;
    let mut cmd = Command::new("wl-copy");
    if sensitive {
        cmd.arg("--sensitive");
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .spawn()
        .expect("wl-copy must be installed for this test");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(text.as_bytes())
        .expect("write to wl-copy");
    child.wait().expect("wl-copy exit");
    // wl-copy forks a server to own the selection; give it a moment to claim it.
    std::thread::sleep(std::time::Duration::from_millis(300));
}

#[test]
#[ignore = "needs a live Wayland session; clobbers the clipboard"]
fn a_real_sensitive_copy_is_detected_and_an_ordinary_one_is_not() {
    use lychi_core::clipboard::sensitive::{offered_types, types_are_sensitive};

    wl_copy("ordinary-text", false);
    let ordinary = offered_types(true).expect("could not enumerate clipboard types");
    println!("ordinary copy offers: {ordinary:?}");
    assert!(
        !types_are_sensitive(&ordinary),
        "an ordinary copy must be recordable"
    );

    wl_copy("hunter2-secret", true);
    let secret = offered_types(true).expect("could not enumerate clipboard types");
    println!("sensitive copy offers: {secret:?}");
    assert!(
        types_are_sensitive(&secret),
        "wl-copy --sensitive must be detected as a password-manager copy"
    );

    // The text is still readable — which is exactly why the type list has to be
    // consulted. If this ever stops being true the hint check is redundant, and
    // that is worth knowing.
    let readable = Command::new("wl-paste")
        .args(["--no-newline", "--type", "text/plain"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    println!("secret still readable as text/plain: {readable}");

    wl_copy("cleanup", false);
}
