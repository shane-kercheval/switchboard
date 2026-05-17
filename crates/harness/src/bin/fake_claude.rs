//! Fixture binary used by integration tests in place of the real `claude` CLI.
//!
//! Usage (mirrors how `ClaudeCodeAdapter` invokes claude):
//!
//!   `fake_claude` -p <fixture-path> [other claude flags ignored]
//!
//! The value after `-p` is treated as a path to a JSONL fixture file. Each
//! non-empty, non-comment line is written to stdout verbatim.
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
//!
//! A fixed "`fake_claude`: done" line is always written to stderr so tests can
//! verify the stderr drain path handles output without deadlocking.

use std::io::{self, BufRead, Read, Write};
use std::process;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Find the value immediately after `-p`.
    let fixture_path = args.windows(2).find(|w| w[0] == "-p").map_or_else(
        || {
            eprintln!("fake_claude: missing -p <fixture-path>");
            process::exit(1);
        },
        |w| w[1].clone(),
    );

    let file = std::fs::File::open(&fixture_path).unwrap_or_else(|e| {
        eprintln!("fake_claude: cannot open fixture {fixture_path}: {e}");
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
            writeln!(err, "fake_claude: {}", msg.trim()).ok();
            continue;
        }

        if line.trim() == "// read_stdin" {
            // Drain stdin to EOF before continuing. If the adapter forgot to
            // close the child's stdin (i.e., didn't set Stdio::null()), this
            // blocks forever — exactly the deadlock the stdin-EOF test guards.
            let mut sink = String::new();
            io::stdin().lock().read_to_string(&mut sink).ok();
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

    // Always write at least one stderr line so tests can verify the drain path.
    writeln!(err, "fake_claude: done").ok();

    process::exit(exit_code);
}
