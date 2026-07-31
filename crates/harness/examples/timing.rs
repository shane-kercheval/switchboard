//! Developer probe: time the login-shell PATH capture and the per-harness
//! binary/version probes on this machine. Run with
//! `cargo run --example timing -p switchboard-harness` to see where install
//! detection latency actually goes.

use std::path::Path;
use std::time::Instant;
use switchboard_harness::subprocess as sp;

fn main() {
    let t = Instant::now();
    let source = sp::await_capture(sp::capture_attempt_budget());
    println!("capture:            {:?}  source={source:?}", t.elapsed());

    for b in ["claude", "codex", "gemini", "agy"] {
        let t = Instant::now();
        let ok = sp::probe_binary(Path::new(b)).is_ok();
        let probe = t.elapsed();
        let t = Instant::now();
        let v = sp::fetch_version(Path::new(b));
        println!(
            "{b:<8} probe={probe:>10.2?}  fetch_version={:>10.2?}  found={ok} v={:?}",
            t.elapsed(),
            v.as_deref().unwrap_or("-")
        );
    }
}
