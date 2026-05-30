//! Fixture binary used by integration tests in place of the real `agy` CLI.
//!
//! Unlike `fake_claude` / `fake_gemini` (which only drip stdout), the
//! Antigravity producer reads the conversation directory under `$HOME` *while
//! the child runs* — correlating the server-assigned UUID, then tailing
//! `transcript.jsonl`. So `fake_agy` must actually create that directory and
//! write records into it, mimicking the server-side agent's on-disk effects.
//!
//! It can't take its instructions on argv: the adapter builds the argv (it
//! passes the real prompt as `-p <prompt>`, plus `--conversation <uuid>` on
//! resume), so `fake_agy` reads a JSON **script** from a fixed file in its cwd
//! ([`FAKE_AGY_SCRIPT_FILE`]). The adapter sets `current_dir(cwd)` and (under a
//! home override) `HOME`, so the script's cwd and the brain root the producer
//! watches both resolve correctly. The script is parallel-safe because each
//! test uses its own tempdir cwd.
//!
//! Every invocation appends its full argv to [`FAKE_AGY_INVOCATIONS_FILE`] in
//! cwd (one line per spawn) so a test can assert which flags the adapter passed
//! on each dispatch — notably `--conversation <healed-uuid>` on the resume that
//! follows a fork-and-heal.
//!
//! ## Script schema (`.fake_agy.json`)
//!
//! ```json
//! {
//!   "conversation_uuid": "<uuid the brain dir is named for>",
//!   "create_brain_dir": true,        // false → simulate a missing transcript (unresumable)
//!   "warning_not_found": "<stale-uuid>", // optional: emit the fork "conversation not found" signal
//!   "records": [ {"json": "<raw transcript.jsonl line>", "delay_ms": 0} ],
//!   "stdout": [ {"text": "<answer line>", "delay_ms": 0} ],
//!   "exit_code": 0,
//!   "log_file_content": "<text>",    // optional: appended to the --log-file after
//!                                    //   the conversation-id lines. Stages content
//!                                    //   for the no-answer branch's log scan (e.g. a
//!                                    //   RESOURCE_EXHAUSTED line) without the real CLI.
//!   "suppress_conversation_log": false, // omit the `Created conversation <uuid>` log
//!                                    //   lines even when a brain dir is created, so a
//!                                    //   test exercises the brain-dir correlation fallback.
//!   "hang": false                    // true → after writing records/stdout, sleep
//!                                    //   indefinitely instead of exiting, so a
//!                                    //   cancellation test can fire the token while
//!                                    //   the adapter is still polling. Stays killable.
//! }
//! ```
//!
//! ## CLI log (`--log-file`)
//!
//! Real `agy` records the server-assigned conversation id in its `--log-file`,
//! and the adapter reads it back as its **primary** capture signal. So when a
//! brain dir is created, `fake_agy` writes realistic `Created conversation
//! <uuid>` + `Print mode: conversation=<uuid>, sending message` lines to that
//! path by default (suppressible via `suppress_conversation_log`), then appends
//! `log_file_content`.
//!
//! Records are appended (not truncated) so a resume continues the same
//! `transcript.jsonl`, matching real Antigravity's single-file-per-conversation
//! behavior.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use serde::Deserialize;
use switchboard_harness::antigravity::paths;
use switchboard_harness::antigravity::{FAKE_AGY_INVOCATIONS_FILE, FAKE_AGY_SCRIPT_FILE};
use uuid::Uuid;

#[derive(Deserialize)]
struct Script {
    conversation_uuid: String,
    #[serde(default = "default_true")]
    create_brain_dir: bool,
    #[serde(default)]
    warning_not_found: Option<String>,
    #[serde(default)]
    records: Vec<Drip>,
    #[serde(default)]
    stdout: Vec<Drip>,
    #[serde(default)]
    exit_code: i32,
    /// Stage content for the per-dispatch CLI log scan. When non-empty and
    /// the adapter passed `--log-file <path>`, write this text there so the
    /// adapter's no-answer-branch scan reads it. Letting the test express
    /// the entire failure shape (e.g. a `RESOURCE_EXHAUSTED` line) through
    /// the same wiring the production adapter uses.
    #[serde(default)]
    log_file_content: String,
    /// Omit the `Created conversation <uuid>` log lines even when a brain dir
    /// is created, so a test can exercise the adapter's brain-dir
    /// prompt-correlation fallback (the path used only if the real CLI's log
    /// line ever moves).
    #[serde(default)]
    suppress_conversation_log: bool,
    /// When true, park indefinitely after writing records/stdout (never exit
    /// on our own), so a cancellation test can fire the token mid-turn. Stays
    /// killable (a sleeping process dies on SIGTERM/SIGKILL).
    #[serde(default)]
    hang: bool,
}

#[derive(Deserialize)]
struct Drip {
    #[serde(default)]
    text: String,
    #[serde(default)]
    json: String,
    #[serde(default)]
    delay_ms: u64,
}

fn default_true() -> bool {
    true
}

/// Return the value of `--flag <value>` from argv, or `None` if absent or
/// trailing without a value.
fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Read the script first. Argv logging happens only *after* a script loads,
    // so a scriptless invocation in the crate dir (the adapter's cwd-agnostic
    // `agy --version` probe) doesn't drop a `.fake_agy.invocations` artifact
    // into the source tree. Real dispatches run in a per-test tempdir that has
    // a script, and log there.
    let script_text = std::fs::read_to_string(FAKE_AGY_SCRIPT_FILE).unwrap_or_else(|e| {
        eprintln!("fake_agy: cannot read {FAKE_AGY_SCRIPT_FILE}: {e}");
        std::process::exit(1);
    });
    let script: Script = serde_json::from_str(&script_text).unwrap_or_else(|e| {
        eprintln!("fake_agy: malformed {FAKE_AGY_SCRIPT_FILE}: {e}");
        std::process::exit(1);
    });

    // Record this invocation's argv so tests can assert passed flags (e.g.
    // `--conversation <healed-uuid>` on the post-fork resume).
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(FAKE_AGY_INVOCATIONS_FILE)
    {
        let _ = writeln!(f, "{}", args[1..].join(" "));
    }

    // Stage this dispatch's CLI log at the `--log-file` path. Real `agy`
    // records the conversation id here and the adapter reads it back as its
    // primary capture signal, so emit realistic id lines by default when a
    // conversation is created. The adapter only reads them on first-turn
    // capture / fork recapture (a normal resume ignores the log), so emitting
    // them unconditionally is harmless. `log_file_content` (e.g. a staged
    // RESOURCE_EXHAUSTED line) is appended for the no-answer-branch scan.
    if let Some(log_path) = arg_value(&args, "--log-file") {
        let mut log = String::new();
        if script.create_brain_dir && !script.suppress_conversation_log {
            let uuid = &script.conversation_uuid;
            let _ = writeln!(log, "server.go:755] Created conversation {uuid}");
            let _ = writeln!(
                log,
                "printmode.go:130] Print mode: conversation={uuid}, sending message"
            );
        }
        log.push_str(&script.log_file_content);
        if !log.is_empty() {
            let _ = std::fs::write(&log_path, &log);
        }
    }

    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| {
        eprintln!("fake_agy: HOME unset");
        std::process::exit(1);
    }));

    let stderr = io::stderr();
    let mut err = stderr.lock();

    // The fork-and-heal signal: a resume whose conversation expired server-side
    // prints this and forks a fresh conversation. Observed on stderr.
    if let Some(stale) = &script.warning_not_found {
        let _ = writeln!(err, "Warning: conversation \"{stale}\" not found.");
        let _ = err.flush();
    }

    // Write transcript records into the (possibly fresh) conversation dir.
    // Skipped entirely when `create_brain_dir` is false — that models a broken
    // transcript path, leaving the producer unable to locate the conversation.
    if script.create_brain_dir {
        let uuid = Uuid::parse_str(&script.conversation_uuid).unwrap_or_else(|e| {
            eprintln!("fake_agy: bad conversation_uuid: {e}");
            std::process::exit(1);
        });
        let transcript = paths::transcript_path(&home, uuid);
        if let Some(parent) = transcript.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript)
            .unwrap_or_else(|e| {
                eprintln!(
                    "fake_agy: cannot open transcript {}: {e}",
                    transcript.display()
                );
                std::process::exit(1);
            });
        for rec in &script.records {
            if rec.delay_ms > 0 {
                sleep(Duration::from_millis(rec.delay_ms));
            }
            let _ = writeln!(file, "{}", rec.json);
            let _ = file.flush();
        }
    }

    // Drip the model's answer to stdout (the live-text source).
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for line in &script.stdout {
        if line.delay_ms > 0 {
            sleep(Duration::from_millis(line.delay_ms));
        }
        let _ = writeln!(out, "{}", line.text);
        let _ = out.flush();
    }

    if script.hang {
        // Records + stdout are written (so the adapter correlates the UUID and
        // tails the transcript); now stay alive so the turn is still in flight
        // when the cancellation test fires the token. Only the adapter's
        // cancel kill ends us.
        loop {
            std::thread::park();
        }
    }

    std::process::exit(script.exit_code);
}
