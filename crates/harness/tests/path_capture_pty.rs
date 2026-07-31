//! The login-shell PATH capture must succeed when the app has a controlling
//! terminal — i.e. when the binary was launched from a shell (`make dev`,
//! `cargo run`) rather than Finder/Dock.
//!
//! The capture spawns the user's shell interactively in its own process group.
//! With a controlling terminal, that group is a *background* group, and an
//! interactive shell with job control enabled suspends itself waiting to be
//! foregrounded — hanging the capture until its timeout and degrading every
//! detection result to the fallback PATH. The fix passes `+m` to disable job
//! control. This test is the only place that reproduces the hostile topology:
//! `script(1)` allocates a real pty and gives the probe binary a controlling
//! terminal, exactly like a terminal launch. A `cargo test` process has no
//! controlling terminal of its own, which is why an in-process test cannot
//! catch this.

#![cfg(target_os = "macos")]

use std::process::Command;

#[test]
fn path_capture_succeeds_under_a_controlling_terminal() {
    let zdotdir = tempfile::tempdir().expect("temp ZDOTDIR should be creatable");
    // An empty .zshrc keeps the capture hermetic: zsh sources it (proving the
    // interactive half runs) without depending on the developer's own config.
    std::fs::write(zdotdir.path().join(".zshrc"), "").expect(".zshrc should be writable");

    let output = Command::new("script")
        .args(["-q", "/dev/null", env!("CARGO_BIN_EXE_path_capture_probe")])
        .env("SHELL", "/bin/zsh")
        .env("ZDOTDIR", zdotdir.path())
        .output()
        .expect("script(1) should run the probe");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("LoginShell"),
        "capture fell back under a controlling terminal (interactive shell \
         suspended in its background process group?): {stdout}"
    );
}
