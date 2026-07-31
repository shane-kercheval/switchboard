//! Test binary: runs one full login-shell PATH capture attempt and prints
//! where the resolved PATH came from (`LoginShell` or `Fallback`). Exists so
//! the pty regression test (`tests/path_capture_pty.rs`) can exercise the
//! capture from a process that has a controlling terminal — the context that
//! makes an interactive shell suspend itself — which a `cargo test` process
//! doesn't have.

use switchboard_harness::subprocess;

fn main() {
    let source = subprocess::await_capture(subprocess::capture_attempt_budget());
    println!("{source:?}");
}
