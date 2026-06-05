//! Fixture binary used by integration tests in place of the real `gemini` CLI.
//!
//! Usage (mirrors how `GeminiAdapter` invokes gemini):
//!
//!   `fake_gemini` --prompt=<fixture-path> [other gemini flags ignored]
//!
//! The adapter passes the prompt in the attached `--prompt=<value>` form (so a
//! leading-dash prompt isn't rejected by yargs). The value is treated as a path
//! to a JSONL fixture file. Each non-empty, non-comment line is written to stdout
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
//!   `// hang` — flush lines emitted so far, then sleep indefinitely (never
//!     exit on our own), leaving the adapter's read parked so a cancellation
//!     test can fire the token. Stays killable (dies on SIGTERM/SIGKILL).

use std::io::{self, BufRead, Read, Write};
use std::process;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let fixture_path = args
        .iter()
        .find_map(|a| a.strip_prefix("--prompt=").map(str::to_owned))
        .unwrap_or_else(|| {
            eprintln!("fake_gemini: missing --prompt=<fixture-path>");
            process::exit(1);
        });

    let file = std::fs::File::open(&fixture_path).unwrap_or_else(|e| {
        eprintln!("fake_gemini: cannot open fixture {fixture_path}: {e}");
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
            writeln!(err, "{}", msg.trim()).ok();
            continue;
        }

        if line.trim() == "// read_stdin" {
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

        if line.trim() == "// hang" {
            out.flush().ok();
            loop {
                std::thread::park();
            }
        }

        if line.trim().is_empty() {
            continue;
        }

        writeln!(out, "{line}").ok();
        out.flush().ok();
    }

    writeln!(err, "fake_gemini: done").ok();

    process::exit(exit_code);
}
