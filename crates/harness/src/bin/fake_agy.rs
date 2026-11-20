//! Fixture binary used by integration tests in place of the real `agy` CLI.
//!
//! Unlike `fake_claude` (which only drips stdout), the
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
//!   "stream": [ {"json": "<one stream-json NDJSON line>", "delay_ms": 0} ],
//!                                    //   optional: written to stdout before the
//!                                    //   `stdout` drip. Empty models a pre-1.1.8
//!                                    //   text-mode agy (exercises the fallbacks).
//!   "stdout": [ {"text": "<answer line>", "delay_ms": 0} ],
//!   "stderr": [ {"text": "<diagnostic line>", "delay_ms": 0} ],
//!                                    //   optional: written to stderr after stdout and
//!                                    //   before any `hang`. agy 1.1.x emits its
//!                                    //   `Error:` diagnostics here rather than on
//!                                    //   stdout; pair with `hang` to model the
//!                                    //   auth-line-then-block case.
//!   "exit_code": 0,
//!   "log_file_content": "<text>",    // optional: appended to the --log-file after
//!                                    //   the conversation-id lines. Stages content
//!                                    //   for the no-answer branch's log scan (e.g. a
//!                                    //   RESOURCE_EXHAUSTED line) without the real CLI.
//!   "suppress_conversation_log": false, // omit the `Created conversation <uuid>` log
//!                                    //   lines even when a brain dir is created, so a
//!                                    //   test exercises the brain-dir correlation fallback.
//!   "hang": false,                   // true → after writing records/stdout, sleep
//!                                    //   indefinitely instead of exiting, so a
//!                                    //   cancellation test can fire the token while
//!                                    //   the adapter is still polling. Stays killable.
//!   "exit_delay_ms": 0,              // optional: wait this long after writing, then exit
//!   "pid_file_self": "<path>",       // optional: this process's own pid is written here
//!   "leftover": {                    // optional: first start a `sleep` that outlives
//!     "seconds": 8,                  //   this process holding its outputs, as a
//!     "holds": "both",               //   background command agy started might:
//!     "own_group": false,            //   "both", "stdout" or "stderr"; own_group puts
//!     "own_session": false,          //   it in a process group of its own, own_session
//!     "ignore_term": false,          //   in a session of its own with no terminal, like
//!     "pid_file": "<path>"           //   agy's auto-updater; ignore_term makes it ignore
//!   },                               //   SIGTERM; its pid is written to pid_file once set up
//!   "command_shell": {               // optional (macOS): then start a shell the way agy
//!     "seconds": 30,                 //   runs a command: leading its own session on a
//!     "shell_pid_file": "<path>",    //   terminal of its own, ignoring SIGHUP. It starts
//!     "worker_pid_file": "<path>",   //   `sleep <seconds>` in a process group of its own
//!     "ready_file": "<path>",        //   and waits for it; each writes its pid. This
//!     "after_term": false            //   process waits for both, checks the worker's
//!   }                                //   group, then creates ready_file. after_term starts
//! }                                  //   it only once SIGTERM arrives, which is then held
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
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::Stdio;
use std::thread::sleep;
use std::time::Duration;

#[cfg(unix)]
use nix::sys::signal::{SigSet, Signal};
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
    /// Diagnostic lines for stderr, written after `stdout` and before `hang`.
    /// `agy` 1.1.x moved its `Error:` output off stdout, so the adapter now
    /// scans both streams; this lets a test stage that shape. Combined with
    /// `hang` it models the auth fast-fail: emit the auth line, then block, so
    /// the test proves the adapter kills the child instead of waiting out the
    /// real CLI's ~30s OAuth window.
    #[serde(default)]
    stderr: Vec<Drip>,
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
    /// Emitted verbatim on stdout **before** the `stdout` drip, one JSON line
    /// each. Models `--output-format stream-json`, which the adapter now
    /// passes on every dispatch: the `init` event is its primary
    /// conversation-id capture, a tool `ERROR` step is the only signal for a
    /// tool failure agy writes no transcript record for, and `result` is the
    /// terminal verdict. Left empty, the fixture behaves like a pre-1.1.8
    /// text-mode `agy`, which is how the fallback chain is exercised.
    #[serde(default)]
    stream: Vec<Drip>,
    /// When true, park indefinitely after writing records/stdout (never exit
    /// on our own), so a cancellation test can fire the token mid-turn. Stays
    /// killable (a sleeping process dies on SIGTERM/SIGKILL).
    #[serde(default)]
    hang: bool,
    /// Wait this long after writing everything, then exit.
    #[serde(default)]
    exit_delay_ms: u64,
    /// Where this process writes its own pid, so a test can tell when the
    /// adapter has reaped it.
    #[serde(default)]
    pid_file_self: Option<String>,
    /// A process to start before anything is written, which outlives this one
    /// and keeps its output pipes open.
    #[serde(default)]
    leftover: Option<Leftover>,
    /// A command shell to start once the leftover is running, before anything
    /// is written.
    #[serde(default)]
    command_shell: Option<CommandShell>,
}

#[derive(Deserialize)]
struct CommandShell {
    /// How long the command's worker sleeps; the shell exits once it has.
    seconds: u64,
    shell_pid_file: String,
    worker_pid_file: String,
    /// Created once both pids are written and the worker is known to lead a
    /// process group of its own.
    ready_file: String,
    /// Block SIGTERM, and start the shell only once SIGTERM arrives, so the
    /// shell exists only while this process exits. Not for a script with a
    /// `leftover`: it would start with SIGTERM blocked too.
    #[serde(default)]
    after_term: bool,
}

#[derive(Deserialize)]
struct Leftover {
    seconds: u64,
    /// Which of this process's outputs it inherits: "both", "stdout" or
    /// "stderr". The other goes to /dev/null.
    holds: String,
    /// Start it in a process group of its own, out of reach of a signal to
    /// this process's group.
    #[serde(default)]
    own_group: bool,
    /// Start it in a session of its own with no terminal, as `agy` starts its
    /// auto-updater. It writes its pid itself once the session is set up.
    #[serde(default)]
    own_session: bool,
    /// Ignore SIGTERM, so only a SIGKILL ends it.
    #[serde(default)]
    ignore_term: bool,
    /// Where its pid is written, so a test can check on it and clean it up.
    /// With `ignore_term` the leftover writes it itself once SIGTERM is
    /// ignored, so a test can wait for that.
    pid_file: String,
}

/// Start `leftover`'s `sleep`, holding the outputs it names, and record its
/// pid.
#[expect(
    clippy::zombie_processes,
    reason = "the leftover is meant to outlive this process, which exits without waiting for it"
)]
fn start_leftover(leftover: &Leftover) {
    let seconds = leftover.seconds.to_string();
    let mut command = if leftover.own_session {
        let exe = std::env::current_exe().unwrap_or_else(|e| {
            eprintln!("fake_agy: cannot find its own binary: {e}");
            std::process::exit(1);
        });
        let mut command = std::process::Command::new(exe);
        command.args([LEAD_A_SESSION, leftover.pid_file.as_str(), seconds.as_str()]);
        command
    } else if leftover.ignore_term {
        // An ignored signal stays ignored across `exec`.
        let mut command = std::process::Command::new("sh");
        command.args([
            "-c",
            r#"trap "" TERM; echo $$ > "$0.tmp" && mv "$0.tmp" "$0"; exec sleep "$1""#,
            leftover.pid_file.as_str(),
            seconds.as_str(),
        ]);
        command
    } else {
        let mut command = std::process::Command::new("sleep");
        command.arg(&seconds);
        command
    };
    command.stdin(Stdio::null());
    if leftover.holds == "stderr" {
        command.stdout(Stdio::null());
    }
    if leftover.holds == "stdout" {
        command.stderr(Stdio::null());
    }
    #[cfg(unix)]
    if leftover.own_group {
        command.process_group(0);
    }
    let child = command.spawn().unwrap_or_else(|e| {
        eprintln!("fake_agy: cannot start the leftover: {e}");
        std::process::exit(1);
    });
    if !leftover.ignore_term && !leftover.own_session {
        write_pid(&leftover.pid_file, child.id());
    }
}

/// The argument that runs this binary as a leftover leading a session of its
/// own: `fake_agy --lead-a-session <pid_file> <seconds>`.
const LEAD_A_SESSION: &str = "--lead-a-session";

/// When run as `fake_agy --lead-a-session <pid_file> <seconds>`, start a
/// session with no terminal, write this process's pid to `pid_file`, and
/// become `sleep <seconds>`.
#[cfg(unix)]
fn lead_a_session_if_asked(args: &[String]) {
    let [_, flag, pid_file, seconds] = args else {
        return;
    };
    if flag != LEAD_A_SESSION {
        return;
    }
    if let Err(e) = nix::unistd::setsid() {
        eprintln!("fake_agy: cannot start a session: {e}");
        std::process::exit(1);
    }
    write_pid(pid_file, std::process::id());
    let e = std::process::Command::new("sleep").arg(seconds).exec();
    eprintln!("fake_agy: cannot run sleep: {e}");
    std::process::exit(1);
}

/// For a command shell started only once SIGTERM arrives, block SIGTERM and
/// return the set to wait on. Called while this is the only thread, so no
/// other thread can take the SIGTERM instead.
#[cfg(unix)]
fn block_sigterm_for(shell: Option<&CommandShell>) -> Option<SigSet> {
    if !shell.is_some_and(|shell| shell.after_term) {
        return None;
    }
    let mut set = SigSet::empty();
    set.add(Signal::SIGTERM);
    if let Err(e) = set.thread_block() {
        eprintln!("fake_agy: cannot block SIGTERM: {e}");
        std::process::exit(1);
    }
    Some(set)
}

/// Wait for the SIGTERM `held` blocks, then start `shell`.
#[cfg(unix)]
fn start_command_shell_on_sigterm(held: SigSet, shell: &CommandShell) {
    if let Err(e) = held.wait() {
        eprintln!("fake_agy: cannot wait for SIGTERM: {e}");
        std::process::exit(1);
    }
    // `std::process::Command` passes the signal mask on to a child, so the
    // shell would otherwise start with SIGTERM blocked too.
    if let Err(e) = held.thread_unblock() {
        eprintln!("fake_agy: cannot unblock SIGTERM: {e}");
        std::process::exit(1);
    }
    start_command_shell(shell);
}

/// Start `shell` the way `agy` starts a command, and return once the shell
/// and its worker have written their pids and the ready file is created.
///
/// `agy` runs each command as `zsh -c <command>` leading its own session on a
/// terminal it opens, with SIGHUP ignored, so the shell outlives `agy`.
/// `script` gives the shell that session and terminal without `unsafe`, which
/// the workspace forbids; the shell is this process's grandchild, through
/// `script`, which stays in this process's group. `set -m` starts the worker
/// in a process group of its own, which it can do because it has a terminal.
fn start_command_shell(shell: &CommandShell) {
    let seconds = shell.seconds.to_string();
    let spawned = std::process::Command::new("/usr/bin/script")
        .args([
            "-q",
            "/dev/null",
            "/bin/sh",
            "-c",
            r#"set -m; trap "" HUP; sleep "$2" & echo $! > "$1.tmp" && mv "$1.tmp" "$1"; echo $$ > "$0.tmp" && mv "$0.tmp" "$0"; wait"#,
            shell.shell_pid_file.as_str(),
            shell.worker_pid_file.as_str(),
            seconds.as_str(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(e) = spawned {
        eprintln!("fake_agy: cannot start the command shell: {e}");
        std::process::exit(1);
    }
    // The shell writes its pid after the worker's, so this waits for both.
    for _ in 0..1000 {
        if std::path::Path::new(&shell.shell_pid_file).exists() {
            #[cfg(unix)]
            check_worker_group(&shell.worker_pid_file);
            if let Err(e) = std::fs::write(&shell.ready_file, "") {
                eprintln!("fake_agy: cannot create the ready file: {e}");
                std::process::exit(1);
            }
            return;
        }
        sleep(Duration::from_millis(10));
    }
    eprintln!("fake_agy: the command shell never wrote its pid");
    std::process::exit(1);
}

/// Exit with an error unless the worker leads a process group of its own, so
/// a test that relies on a second group cannot pass without one.
#[cfg(unix)]
fn check_worker_group(worker_pid_file: &str) {
    let worker = std::fs::read_to_string(worker_pid_file)
        .ok()
        .and_then(|pid| pid.trim().parse::<i32>().ok())
        .map(nix::unistd::Pid::from_raw);
    let own_group = worker.is_some_and(|pid| nix::unistd::getpgid(Some(pid)) == Ok(pid));
    if !own_group {
        eprintln!("fake_agy: the command's worker is not in a process group of its own");
        std::process::exit(1);
    }
}

/// Write `pid` to `path` whole, so a reader polling for the file never sees
/// it half-written.
fn write_pid(path: &str, pid: u32) {
    let partial = format!("{path}.tmp");
    if std::fs::write(&partial, pid.to_string()).is_ok() {
        let _ = std::fs::rename(&partial, path);
    }
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

/// Stage this dispatch's CLI log at the `--log-file` path. Real `agy`
/// records the conversation id here and the adapter reads it back as its
/// primary capture signal, so emit realistic id lines by default when a
/// conversation is created. The adapter only reads them on first-turn
/// capture / fork recapture (a normal resume ignores the log), so emitting
/// them unconditionally is harmless. `log_file_content` (e.g. a staged
/// `RESOURCE_EXHAUSTED` line) is appended for the no-answer-branch scan.
fn stage_log(args: &[String], script: &Script) {
    let Some(log_path) = arg_value(args, "--log-file") else {
        return;
    };
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

/// Return the value of `--flag <value>` from argv, or `None` if absent or
/// trailing without a value.
fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Write `script`'s transcript records into the (possibly fresh)
/// conversation dir under `home`. Skipped when `create_brain_dir` is false,
/// which models a broken transcript path, leaving the producer unable to
/// locate the conversation.
fn write_transcript(script: &Script, home: &std::path::Path) {
    let uuid = Uuid::parse_str(&script.conversation_uuid).unwrap_or_else(|e| {
        eprintln!("fake_agy: bad conversation_uuid: {e}");
        std::process::exit(1);
    });
    let transcript = paths::transcript_path(home, uuid);
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(unix)]
    lead_a_session_if_asked(&args);

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
    #[cfg(unix)]
    let held_term = block_sigterm_for(script.command_shell.as_ref());

    // Record this invocation's argv so tests can assert passed flags (e.g.
    // `--conversation <healed-uuid>` on the post-fork resume).
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(FAKE_AGY_INVOCATIONS_FILE)
    {
        let _ = writeln!(f, "{}", args[1..].join(" "));
    }

    if let Some(path) = &script.pid_file_self {
        write_pid(path, std::process::id());
    }
    if let Some(leftover) = &script.leftover {
        start_leftover(leftover);
    }
    if let Some(shell) = &script.command_shell
        && !shell.after_term
    {
        start_command_shell(shell);
    }

    stage_log(&args, &script);

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

    if script.create_brain_dir {
        write_transcript(&script, &home);
    }

    // Drip the model's answer to stdout (the live-text source).
    let stdout = io::stdout();
    let mut out = stdout.lock();
    // Stream events first: the adapter reads them from the same stdout pipe
    // and captures the conversation id from `init`, so they must precede any
    // text the test expects to be classified afterwards.
    for line in &script.stream {
        if line.delay_ms > 0 {
            sleep(Duration::from_millis(line.delay_ms));
        }
        let _ = writeln!(out, "{}", line.json);
        let _ = out.flush();
    }
    for line in &script.stdout {
        if line.delay_ms > 0 {
            sleep(Duration::from_millis(line.delay_ms));
        }
        let _ = writeln!(out, "{}", line.text);
        let _ = out.flush();
    }

    for line in &script.stderr {
        if line.delay_ms > 0 {
            sleep(Duration::from_millis(line.delay_ms));
        }
        let _ = writeln!(err, "{}", line.text);
        let _ = err.flush();
    }

    #[cfg(unix)]
    if let (Some(held), Some(shell)) = (held_term, &script.command_shell) {
        start_command_shell_on_sigterm(held, shell);
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

    if script.exit_delay_ms > 0 {
        sleep(Duration::from_millis(script.exit_delay_ms));
    }
    std::process::exit(script.exit_code);
}
