//! Fixture binary used by integration tests in place of the real `codex` CLI.
//!
//! Usage (mirrors how `CodexAdapter` invokes codex):
//!
//!   `fake_codex` [exec [resume <session-id>]] [flags…] <fixture-path-as-prompt>
//!
//! **Asymmetry with `fake_claude`.** `fake_claude` interprets the value after
//! `-p` as the fixture path because the real claude CLI takes the prompt as
//! `-p <prompt>`. Codex's CLI takes the prompt as the **last positional
//! argument** (`codex exec [resume <id>] --json … -C <cwd> "<prompt>"`); the
//! fake binary mirrors that shape by taking the LAST argv entry as the
//! fixture path. Documented asymmetry is fine — both fakes interpret "what
//! the real CLI calls the prompt arg" as the fixture path; the real CLIs
//! just disagree about where that arg lives.
//!
//! Each non-empty, non-comment line of the fixture is written to stdout
//! verbatim.
//!
//! Special comment lines in the fixture (processed, never forwarded to stdout):
//!   `// exit:<N>` — exit with code N instead of 0; stops line processing.
//!   `// stderr:<message>` — write message to stderr before streaming begins.
//!   `// read_stdin` — read stdin to EOF before streaming. The adapter must
//!     spawn the child with `Stdio::null()` for stdin so this returns
//!     immediately; without it, the test would deadlock waiting for input.
//!   `// pgid_to:<path>` — write the child's own process-group id (decimal
//!     ASCII, single line) to the given path before streaming. Tests use
//!     this to assert the adapter put us in our own process group.
//!   `// spawn_child_holding_stderr` (unix only) — fork a child process
//!     that inherits the parent's stderr FD and sleeps indefinitely.
//!     Simulates Codex's two-process tree (Node parent + Rust child) so
//!     tests can verify the adapter uses `killpg` to clean up the whole
//!     group; with plain `kill` on the parent PID, the forked child would
//!     keep the stderr pipe open and the adapter's stderr-drain task would
//!     hang on EOF that never arrives.
//!
//! A fixed "`fake_codex`: done" line is always written to stderr so tests can
//! verify the stderr drain path handles output without deadlocking.

use std::io::{self, BufRead, Read, Write};
use std::process;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // Last argv entry is the prompt slot — interpret as the fixture path.
    // (env::args()[0] is the binary name; we want index >= 1.)
    let fixture_path = match args.last() {
        Some(p) if args.len() >= 2 => p.clone(),
        _ => {
            eprintln!("fake_codex: expected at least one positional arg (fixture path)");
            process::exit(1);
        }
    };

    let file = std::fs::File::open(&fixture_path).unwrap_or_else(|e| {
        eprintln!("fake_codex: cannot open fixture {fixture_path}: {e}");
        process::exit(1);
    });

    let reader = io::BufReader::new(file);
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let stderr = io::stderr();
    let mut err = stderr.lock();

    let mut exit_code: i32 = 0;

    for line in reader.lines() {
        let line = line.unwrap_or_default();

        if let Some(rest) = line.strip_prefix("// exit:") {
            exit_code = rest.trim().parse().unwrap_or(1);
            break;
        }

        if let Some(msg) = line.strip_prefix("// stderr:") {
            writeln!(err, "fake_codex: {}", msg.trim()).ok();
            continue;
        }

        if line.trim() == "// read_stdin" {
            let mut sink = String::new();
            io::stdin().lock().read_to_string(&mut sink).ok();
            continue;
        }

        if line.trim() == "// spawn_child_holding_stderr" {
            #[cfg(unix)]
            {
                // Spawn `sleep` as a long-running child. By default
                // std::process::Command inherits the parent's stdio, so
                // the sleep child holds an FD to fake_codex's stderr
                // pipe — exactly the "Rust child keeps the pipe alive
                // after the Node parent dies" pattern in real Codex.
                // The child inherits fake_codex's process group (set by
                // the adapter's `process_group(0)` at spawn), so
                // killpg(fake_codex_pid) reaches both. Plain
                // kill(fake_codex_pid) would only reach the parent and
                // leave the sleep child holding the pipe — exactly the
                // hang we're testing against.
                //
                // We don't track the child handle: if the adapter's
                // killpg works, the kernel reaps it; if it doesn't, the
                // test fails loudly with a timeout. No reason to clean
                // up explicitly.
                let _ = std::process::Command::new("sleep").arg("3600").spawn();
            }
            #[cfg(not(unix))]
            {
                // No-op on non-unix; process groups aren't a thing.
            }
            continue;
        }

        if let Some(path) = line.strip_prefix("// pgid_to:") {
            #[cfg(unix)]
            {
                let pgid = nix::unistd::getpgrp();
                if let Ok(mut f) = std::fs::File::create(path.trim()) {
                    let _ = writeln!(f, "{pgid}");
                }
            }
            #[cfg(not(unix))]
            {
                let _ = path;
            }
            continue;
        }

        if line.trim().is_empty() {
            continue;
        }

        writeln!(out, "{line}").ok();
        out.flush().ok();
    }

    writeln!(err, "fake_codex: done").ok();

    process::exit(exit_code);
}
