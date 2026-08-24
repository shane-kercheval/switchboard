//! Subprocess helpers shared between harness adapters.
//!
//! Spawning a CLI subprocess, draining its stderr to a bounded tail buffer,
//! formatting that tail for inclusion in synthesized error events, and
//! force-killing the resulting process group are concerns every harness
//! adapter has. Keeping them in one module means a fix to (say) the UTF-8
//! boundary handling in [`format_stderr_tail`] lands once, not once per
//! adapter; and the `killpg`-vs-plain-`kill` distinction (load-bearing for
//! Codex's two-process tree) is implemented
//! in a single place that any new harness adapter calls without having to
//! re-derive the correct behavior — see [`terminate_then_kill`].
//!
//! **What is NOT here.** `synthesize_truncation_turn_end` stays
//! adapter-local — Claude and Codex construct different diagnostic messages
//! (Codex consumes a parser-buffered `error` event payload that Claude has
//! no equivalent for). Both adapters compose their messages on top of
//! [`format_stderr_tail`] from this module.
//!
//! # Two responsibilities
//!
//! This module currently holds two independent lifecycles, and a reader should
//! know which one they are in:
//!
//! 1. **Bounded process execution** — [`run_bounded`] and its reader, plus the
//!    process-group teardown helpers. Runs one command under a hard time bound,
//!    retaining a capped tail of its stdout.
//! 2. **PATH resolution** — [`CaptureState`] and the statics around it: a
//!    generation-guarded state machine that reads the user's login-shell PATH in
//!    the background, publishes it on a revision stream, and serves a widened
//!    fallback until it lands. Callers reach it through [`resolved_path`],
//!    [`await_capture`], and [`ensure_path_settled`].
//!
//! The second belongs in its own `path_resolver` module, and the resolver state
//! belongs in an owned value rather than process-global statics — the globals are
//! what force the injectable seams in `install_status_for` and the dispatcher's
//! readiness wait. See `docs/implementation_plans/2026-07-30-path-resolver-extraction.md`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncBufReadExt;

use crate::adapter::DispatchError;
use crate::events::TurnId;
use switchboard_core::AgentId;

/// How long to wait for a process group to exit after SIGTERM before
/// escalating to SIGKILL. A ceiling, not a fixed wait — a well-behaved
/// harness exits on SIGTERM in milliseconds, so this is only paid by a
/// SIGTERM-deaf (or slow-to-flush) process. Kept generous enough for Codex's
/// Node-parent-plus-Rust-child tree to flush its session file.
pub const TERMINATE_GRACE: Duration = Duration::from_secs(2);

/// Maximum number of stderr lines retained in the per-dispatch tail buffer.
/// Tail-only (FIFO drop of older lines) — we only need the last few lines
/// of stderr when synthesizing a failure message for a truncated stream.
pub const STDERR_TAIL_CAPACITY: usize = 16;

/// Bound the formatted stderr message length so it stays readable in the
/// UI. Truncation happens on char boundaries (see [`format_stderr_tail`]).
pub const STDERR_MESSAGE_MAX_LEN: usize = 800;

/// Sentinel bracketing the PATH value we ask the login shell to print, so we
/// can recover it even when shell startup emits its own banner/MOTD output
/// around our command.
#[cfg(target_os = "macos")]
const PATH_SENTINEL: &str = "__SWITCHBOARD_PATH__";

/// Extract the PATH value bracketed by [`PATH_SENTINEL`] from login-shell
/// output. Returns `None` if the markers are absent or bracket an empty value.
#[cfg(target_os = "macos")]
fn parse_sentinel_path(output: &str) -> Option<String> {
    let start = output.find(PATH_SENTINEL)? + PATH_SENTINEL.len();
    let rest = &output[start..];
    let end = rest.find(PATH_SENTINEL)?;
    let path = rest[..end].trim();
    (!path.is_empty()).then(|| path.to_owned())
}

/// The `+m` (job control off) flag for shells that accept the POSIX `+` option
/// form, resolved from the shell's basename.
///
/// An interactive shell with job control enabled that finds itself in a
/// background process group of a controlling terminal suspends itself waiting
/// to be foregrounded. The capture always runs in its own process group (see
/// `run_bounded_cancellable`) — so whenever the app itself is launched from a
/// terminal (`make dev`, running the binary by hand), that group is background
/// on a real tty and the capture hangs until the timeout, then falls back. An
/// app launched from Finder/Dock has no controlling terminal, so the same spawn
/// succeeds — which is why this only ever bit terminal-launched instances.
/// `+m` removes the suspension while `-i` keeps `.zshrc` sourcing.
///
/// Shells outside the list (fish, nushell) reject `+m` outright, which would
/// break their capture even in the no-terminal case that works today — so they
/// keep the flag off and, with it, the old terminal-launched hang-then-fallback.
#[cfg(target_os = "macos")]
fn job_control_off_flag(shell: &str) -> Option<&'static str> {
    let name = std::path::Path::new(shell).file_name()?.to_str()?;
    matches!(name, "zsh" | "bash" | "sh" | "ksh" | "dash").then_some("+m")
}

/// Run the user's login shell once to print its PATH between two sentinels.
/// `-ilc` sources both login (`.zprofile`) and interactive (`.zshrc`) startup
/// files — where nvm/asdf/pyenv and `~/.local/bin` typically extend PATH — so
/// the result matches what the user sees in a terminal. `$SHELL` falls back to
/// zsh (the macOS default). `printf` avoids a trailing newline inside the
/// sentinels. `+m` (where supported) stops the interactive shell from
/// suspending itself when the app has a controlling terminal — see
/// [`job_control_off_flag`].
#[cfg(target_os = "macos")]
fn run_login_shell_path(cancel: &Arc<std::sync::atomic::AtomicBool>) -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
    let shell_path = shell.clone();
    let script = format!("printf '%s%s%s' '{PATH_SENTINEL}' \"$PATH\" '{PATH_SENTINEL}'");
    let mut command = std::process::Command::new(&shell);
    if let Some(flag) = job_control_off_flag(&shell) {
        command.arg(flag);
    }
    command.args(["-ilc", &script]);
    // Deliberately ignores the exit status: the sentinel's presence is the
    // honest success signal. A profile ending in a failing exit trap, or a shell
    // killed by a signal after printing, still gave us a valid PATH — and
    // discarding it would fall back and start a retry backoff for nothing.
    let output = match run_bounded_cancellable(command, PATH_CAPTURE_TIMEOUT, cancel) {
        Ok(output) => output,
        Err(BoundedFailure::Cancelled) => return None,
        Err(reason) => {
            tracing::warn!(%shell_path, %reason, "login-shell PATH capture could not run");
            return None;
        }
    };
    let parsed = parse_sentinel_path(&output.stdout);
    if parsed.is_none() {
        // The shell ran but its output carried no sentinels. Overwhelmingly a
        // startup file that exits early, replaces the shell (`exec`), or eats
        // stdout — none of which we can fix, but all of which are obvious from
        // the tail. Logged with the tail because guessing at this from the
        // outside is what made this failure survive two rounds of theories.
        tracing::warn!(
            %shell_path,
            exit_status = ?output.success,
            captured_bytes = output.stdout.len(),
            tail = %output.stdout.chars().rev().take(200).collect::<String>().chars().rev().collect::<String>(),
            "login-shell ran but printed no PATH sentinels"
        );
    }
    parsed
}

/// How long to poll between `try_wait` checks while waiting on a bounded child,
/// and between non-blocking stdout reads. Short enough that a fast command adds
/// no perceptible latency, long enough that the poll costs nothing.
/// `std::process::Child` has no timed wait, and a waiter thread would still have
/// to be abandoned on timeout — the thing this helper exists to avoid — so
/// polling is the honest primitive here.
#[cfg(unix)]
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How long to wait for a finished command's stdout to reach EOF. Only paid when
/// something outlived the command and is holding the pipe.
#[cfg(unix)]
pub const READ_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Non-unix builds never run the bounded helper; exported so callers deriving a
/// budget from it compile everywhere.
#[cfg(not(unix))]
pub const READ_DRAIN_GRACE: Duration = Duration::ZERO;

/// Worst-case wall time for one capture attempt: the shell timeout, plus
/// whichever tail applies. A timed-out attempt pays [`TERMINATE_GRACE`] tearing
/// the process group down; one that succeeds near the deadline pays
/// [`READ_DRAIN_GRACE`] waiting for EOF. Taking the max rather than naming one
/// keeps a budget derived from this correct if either grace is tuned
/// independently — they are equal today, which is exactly how a hand-picked
/// constant silently stops covering the case it was written for.
#[must_use]
pub const fn capture_attempt_budget() -> Duration {
    let tail = if TERMINATE_GRACE.as_nanos() >= READ_DRAIN_GRACE.as_nanos() {
        TERMINATE_GRACE
    } else {
        READ_DRAIN_GRACE
    };
    PATH_CAPTURE_TIMEOUT.saturating_add(tail)
}

/// Cap on retained stdout. The **tail** is kept, not the head: a login shell
/// sources its startup files first and prints the value we want *last*, so
/// capping from the front would discard exactly the answer — turning a chatty
/// profile into a capture failure. Reading always continues to EOF regardless,
/// because stopping would close the read end and `SIGPIPE` the child mid-startup.
#[cfg(unix)]
const MAX_CAPTURED_OUTPUT: usize = 256 * 1024;

/// A bounded command's result: the retained tail of stdout, plus what we could
/// learn about how it exited.
///
/// Status is reported rather than folded into `Option` because callers disagree
/// about it. `fetch_version` wants status-gating — a `--version` that fails has
/// no version. The PATH capture does not: its honest success signal is whether
/// the sentinels are present, and a profile that ends in a failing exit trap
/// still printed a perfectly good PATH. Collapsing the two is how a valid
/// capture gets thrown away.
///
/// `success: None` means the exit status could not be determined — `waitpid`
/// failed, overwhelmingly because something else already reaped the child. A
/// host process that installs a `SIGCHLD` handler or sets `SIGCHLD` to
/// `SIG_IGN` (GUI frameworks do) has the kernel auto-reap, and `waitpid` then
/// returns `ECHILD` for a command that ran perfectly. The output is still valid
/// and is still returned; only the verdict is unknown.
#[cfg(unix)]
#[derive(Debug)]
struct BoundedOutput {
    stdout: String,
    success: Option<bool>,
}

/// Run a command to completion with a hard time bound. Returns `None` only when
/// the command could not be started or did not finish in time; a command that
/// finished unsuccessfully still returns its output.
///
/// **Why this exists rather than `Command::output()`.** `output()` waits without
/// a bound. Bounding it by abandoning a thread — the shape this module used to
/// have — leaves the child alive and owned by nobody: a login shell that blocks
/// on `/dev/tty`, a network call, or a file lock survives, and every retry
/// strands another one for the lifetime of the app. Owning the handle is what
/// makes the timeout actually mean something.
///
/// **Why stdout is drained concurrently.** `output()` polls stdout and stderr
/// while it waits. A naive spawn-then-wait does not, so a child writing more
/// than the pipe buffer blocks on write, never exits, and gets killed at the
/// timeout — turning a working capture into a failure on any machine with a
/// chatty shell profile. The reader thread below preserves that behavior while
/// bounding memory. stderr is discarded rather than drained: nothing reads it,
/// and a null sink cannot fill.
/// Why a bounded command produced no result. Reported rather than collapsed into
/// `None` because these have entirely different causes and remedies, and a
/// failure whose reason is discarded can only be diagnosed by guessing — which
/// is how the PATH capture's real-world failure survived two rounds of theories.
#[cfg(unix)]
#[derive(Debug)]
pub enum BoundedFailure {
    /// The command could not be started at all.
    Spawn(String),
    /// Still running when the deadline passed.
    TimedOut,
    /// Superseded by a newer request.
    Cancelled,
}

#[cfg(unix)]
impl std::fmt::Display for BoundedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(err) => write!(f, "could not start the command: {err}"),
            Self::TimedOut => f.write_str("still running at the deadline"),
            Self::Cancelled => f.write_str("superseded by a newer request"),
        }
    }
}

#[cfg(unix)]
fn run_bounded(command: std::process::Command, timeout: Duration) -> Option<BoundedOutput> {
    run_bounded_cancellable(
        command,
        timeout,
        &Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
    .ok()
}

/// As [`run_bounded`], but abandons the command early when `cancel` is set,
/// tearing down its process group on the way out.
///
/// Cancellation exists for the supersede case: a Recheck arriving while a
/// capture is running used to wait out the doomed attempt *and then* run a fresh
/// one, so the user paid two captures back to back — up to double the timeout if
/// the first was hanging, which is precisely when they pressed the button.
#[cfg(unix)]
fn run_bounded_cancellable(
    mut command: std::process::Command,
    timeout: Duration,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
) -> Result<BoundedOutput, BoundedFailure> {
    use std::os::unix::process::CommandExt as _;

    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        // Own the whole tree, not just the direct child: a shell's startup
        // files spawn their own children, and killing only the shell would
        // leave those behind. Same rationale as `terminate_then_kill`.
        .process_group(0);

    let mut child = command
        .spawn()
        .map_err(|err| BoundedFailure::Spawn(err.to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| BoundedFailure::Spawn("stdout was not piped".to_owned()))?;

    // The reader owns the tail buffer; the caller reads it whether or not EOF
    // ever arrives. A channel carrying the data would either be unbounded (the
    // memory problem) or block the reader (the pipe-fill problem) — sharing a
    // bounded buffer is neither.
    let tail = Arc::new(Mutex::new(VecDeque::<u8>::new()));
    let (eof_tx, eof_rx) = std::sync::mpsc::channel::<()>();
    spawn_stdout_reader(
        stdout,
        Arc::clone(&tail),
        timeout + READ_DRAIN_GRACE,
        eof_tx,
    );

    let deadline = std::time::Instant::now() + timeout;
    let mut finished: Option<Option<bool>> = None;
    loop {
        if let WaitOutcome::Finished(verdict) = interpret_wait(child.try_wait()) {
            finished = Some(verdict);
            break;
        }
        if cancel.load(std::sync::atomic::Ordering::Relaxed)
            || std::time::Instant::now() >= deadline
        {
            break;
        }
        std::thread::sleep(CHILD_POLL_INTERVAL);
    }

    let Some(success) = finished else {
        // Genuinely overran (or was superseded). Safe to signal precisely
        // because `try_wait` never reported the child gone: it is unreaped, so
        // its PID still names our group and cannot have been recycled.
        let cancelled = cancel.load(std::sync::atomic::Ordering::Relaxed);
        terminate_group_then_kill(&mut child);
        return Err(if cancelled {
            BoundedFailure::Cancelled
        } else {
            BoundedFailure::TimedOut
        });
    };

    // Give EOF a bounded grace, then take whatever the reader has. The child
    // exiting does not close the pipe — anything its startup files left running
    // in the background inherited the write end — so waiting on EOF outright can
    // block forever. Signalling the group instead would be racy at this point:
    // the child has been reaped, so its PID no longer reliably names the group.
    let _ = eof_rx.recv_timeout(READ_DRAIN_GRACE);
    let bytes: Vec<u8> = lock_cache_or_recover(&tail).iter().copied().collect();
    Ok(BoundedOutput {
        stdout: String::from_utf8_lossy(&bytes).into_owned(),
        success,
    })
}

/// What a `try_wait` result says about whether the child is done.
///
/// **An errored wait means the child is gone, not that it overran.** `waitpid`
/// fails here essentially only with `ECHILD`: something already reaped the
/// child, which is exactly what a host process that installs a `SIGCHLD` handler
/// or sets `SIGCHLD` to `SIG_IGN` causes — GUI frameworks do. Reading that as a
/// timeout kills the process group and discards output the command already
/// produced, turning a perfectly good run into a failure *only* inside such a
/// host. That is what made the PATH capture fail in the packaged app while the
/// identical call succeeded from a shell-launched binary.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum WaitOutcome {
    /// Still running.
    Running,
    /// Finished. `Some(ok)` when the exit status was readable, `None` when it
    /// could not be determined.
    Finished(Option<bool>),
}

#[cfg(unix)]
fn interpret_wait(result: std::io::Result<Option<std::process::ExitStatus>>) -> WaitOutcome {
    match result {
        Ok(Some(status)) => WaitOutcome::Finished(Some(status.success())),
        Ok(None) => WaitOutcome::Running,
        Err(err) => {
            tracing::debug!(%err, "could not reap bounded child; assuming it finished");
            WaitOutcome::Finished(None)
        }
    }
}

/// Drain a child's stdout into a bounded tail buffer on a background thread,
/// signalling `eof_tx` when the stream ends.
///
/// The pipe is switched to non-blocking so the thread can honor `deadline`.
/// A plain blocking `read` cannot: a surviving grandchild holding the write end
/// leaves the thread parked inside the syscall with no way to notice anything —
/// one stranded thread and file descriptor per occurrence, and `fetch_version`
/// runs this four times per install refresh.
///
/// The switch is best-effort by design. `F_SETFL` on a pipe we created moments
/// ago can fail only with `EBADF`/`EINVAL`, neither reachable here; treating it
/// as fatal would convert an unreachable condition into "no PATH for the whole
/// app", which is strictly worse than the leak it would prevent. A blocking
/// reader still works in the ordinary case — the child exiting closes the pipe —
/// so the only cost is losing cancellation in the grandchild-holds-the-pipe
/// sub-case. Logged so it isn't silent.
#[cfg(unix)]
fn spawn_stdout_reader(
    stdout: std::process::ChildStdout,
    tail: Arc<Mutex<VecDeque<u8>>>,
    deadline_after: Duration,
    eof_tx: std::sync::mpsc::Sender<()>,
) {
    use std::io::Read as _;

    std::thread::spawn(move || {
        let mut stdout = stdout;
        // Best-effort: if this fails the reader still works, it just can't be
        // cancelled — the pre-existing behavior, not a new failure mode.
        if let Err(err) = nix::fcntl::fcntl(
            &stdout,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        ) {
            tracing::warn!(%err, "could not set stdout non-blocking; reader is uncancellable");
        }
        let deadline = std::time::Instant::now() + deadline_after;
        let mut chunk = [0_u8; 8192];
        loop {
            // Checked every iteration, not only when a read would block. A
            // descendant that keeps *writing* never yields `WouldBlock`, so a
            // deadline consulted solely on that arm is never consulted at all and
            // the thread outlives `run_bounded` indefinitely.
            if std::time::Instant::now() >= deadline {
                break;
            }
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    if let Ok(mut buf) = tail.lock() {
                        buf.extend(&chunk[..read]);
                        let excess = buf.len().saturating_sub(MAX_CAPTURED_OUTPUT);
                        buf.drain(..excess);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(CHILD_POLL_INTERVAL);
                }
                // A signal arriving mid-read is not end-of-stream. Treating it as
                // one silently truncates the output — and the value we want is at
                // the tail, so a truncated read is a failed capture.
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = eof_tx.send(());
    });
}

#[cfg(not(unix))]
struct BoundedOutput {
    stdout: String,
    success: Option<bool>,
}

#[cfg(not(unix))]
fn run_bounded(mut command: std::process::Command, _timeout: Duration) -> Option<BoundedOutput> {
    let output = command.output().ok()?;
    Some(BoundedOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        success: Some(output.status.success()),
    })
}

/// SIGTERM a bounded child's process group, allow a short grace period, then
/// SIGKILL and reap. The blocking sibling of [`terminate_then_kill`], and
/// deliberately identical to it — including the unconditional final group
/// SIGKILL after the grace loop has already reaped the leader.
///
/// That ordering looks like a PID-reuse hazard and is not one worth diverging
/// over: these signal the *group*, a pgid is not reused while the group has any
/// living member, and `killpg` against an empty group is the `ESRCH` no-op
/// [`killpg_signal`] already swallows. Reaching a real victim would require a
/// brand-new process to become a group leader holding that exact freed PID
/// inside a microsecond window. Skipping the final kill instead would trade
/// that for the thing it exists to prevent: descendants that outlive the parent
/// still holding our pipes.
#[cfg(unix)]
fn terminate_group_then_kill(child: &mut std::process::Child) {
    let Ok(pid) = i32::try_from(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    let group = nix::unistd::Pid::from_raw(pid);
    killpg_signal(group, nix::sys::signal::Signal::SIGTERM);
    let deadline = std::time::Instant::now() + TERMINATE_GRACE;
    while std::time::Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
            break;
        }
        std::thread::sleep(CHILD_POLL_INTERVAL);
    }
    killpg_signal(group, nix::sys::signal::Signal::SIGKILL);
    let _ = child.wait();
}

/// How long to wait for the login shell to report its PATH before declaring the
/// capture failed. Generous because the worst case is a launch at login-restore
/// right after an OS update, where the dyld cache is cold, Spotlight is
/// reindexing, and every subprocess a heavy `.zshrc` spawns pays first-run
/// Gatekeeper validation.
///
/// **Routine reads never wait on this window** — [`resolved_path`] returns a
/// snapshot immediately. Three callers deliberately do wait, each with its own
/// bound: turn dispatch (~3s, so an agent isn't spawned on a guessed PATH),
/// auto-create (~5s, so a new project isn't seeded from one), and Recheck (the
/// full derived budget, because the user is watching a spinner). So this figure
/// is not free — it is the ceiling those bounded waits are sized against.
#[cfg(target_os = "macos")]
pub const PATH_CAPTURE_TIMEOUT: Duration = Duration::from_secs(20);

/// Non-macOS builds inherit a correct PATH, so nothing is captured. Exported so
/// callers deriving a budget from it compile everywhere.
#[cfg(not(target_os = "macos"))]
pub const PATH_CAPTURE_TIMEOUT: Duration = Duration::ZERO;

/// Base gap between capture attempts after a failure, doubled per consecutive
/// failure up to [`PATH_RETRY_MAX_COOLDOWN`]. Without a cooldown, a machine
/// whose shell capture is genuinely broken would spawn a fresh login shell on
/// every window-focus refresh forever; without the escalation, it would still do
/// so every 30 seconds for as long as the app is open.
#[cfg(target_os = "macos")]
const PATH_RETRY_COOLDOWN: Duration = Duration::from_secs(30);

/// Ceiling for the escalating retry cooldown. A user who fixes their shell
/// profile mid-session still recovers without restarting — just not instantly.
/// The Recheck action bypasses the cooldown entirely, so this never blocks an
/// explicit user request.
#[cfg(target_os = "macos")]
const PATH_RETRY_MAX_COOLDOWN: Duration = Duration::from_mins(5);

/// Memoized [`scan_well_known_bin_dirs`]. The scan runs on every `fallback_path`
/// call — which, while degraded, means every dispatch and every probe — and the
/// directories involved don't appear mid-session. Cleared by
/// [`invalidate_path_cache`] so a user who installs a CLI into a newly-created
/// directory and then hits Recheck is re-scanned rather than served a stale
/// answer.
#[cfg(target_os = "macos")]
static WELL_KNOWN_BIN_DIRS: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);

#[cfg(target_os = "macos")]
fn well_known_bin_dirs() -> Vec<PathBuf> {
    let mut cached = lock_cache_or_recover(&WELL_KNOWN_BIN_DIRS);
    cached.get_or_insert_with(scan_well_known_bin_dirs).clone()
}

#[cfg(target_os = "macos")]
fn invalidate_well_known_bin_dirs() {
    *lock_cache_or_recover(&WELL_KNOWN_BIN_DIRS) = None;
}

/// Directories harness CLIs are conventionally installed into, used to widen
/// [`fallback_path`] when the login-shell capture fails. Only existing
/// directories are returned, so the fallback PATH stays readable in logs.
#[cfg(target_os = "macos")]
fn scan_well_known_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/local/sbin"),
    ];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.extend(home_bin_dirs(&home));
    }
    dirs.retain(|dir| dir.is_dir());
    dirs
}

/// Per-user candidates under `home`, filtered to those that exist. Split out so
/// the existence filter is testable against an injected root rather than
/// whatever happens to be installed on the machine running the tests.
#[cfg(target_os = "macos")]
fn home_bin_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        home.join(".local/bin"),
        home.join(".bun/bin"),
        home.join(".deno/bin"),
        home.join(".cargo/bin"),
        home.join(".volta/bin"),
        home.join(".asdf/shims"),
        home.join(".local/share/mise/shims"),
    ];
    dirs.extend(node_version_manager_bin_dirs(home));
    dirs.retain(|dir| dir.is_dir());
    dirs
}

/// Sort key for a node version directory name (`v22.22.1`, `22.9.0`): its
/// numeric segments, so `v22` sorts above `v9` — which a lexicographic sort gets
/// backwards. A segment with no leading digits contributes 0, so an unparseable
/// name sorts low rather than being dropped.
#[cfg(target_os = "macos")]
fn version_sort_key(name: &str) -> [u64; 3] {
    let mut key = [0_u64; 3];
    for (slot, segment) in key.iter_mut().zip(name.trim_start_matches('v').split('.')) {
        let digits: String = segment.chars().take_while(char::is_ascii_digit).collect();
        *slot = digits.parse().unwrap_or(0);
    }
    key
}

/// How many node versions per manager the fallback will consider, newest first.
///
/// **Known limitation.** These directories also become the *child's* PATH, so a
/// harness CLI found under one version may end up running under another
/// version's `node` — and a CLI installed only under a stale version reports as
/// available even though the user's terminal can't run it. Capping bounds that
/// blast radius; it doesn't eliminate it. Making it correct in general means
/// giving each discovered binary its own execution PATH (that version's `bin`
/// first) and threading it through dispatch — a much larger change for a code
/// path that only runs when the login-shell capture has already failed. Note
/// this is *not* resolved via the manager's `default` alias: real nvm defaults
/// are usually `lts/*` or a bare major, so alias resolution would add parsing
/// for a rule that misses the common cases anyway.
#[cfg(target_os = "macos")]
const MAX_NODE_VERSIONS_IN_FALLBACK: usize = 2;

/// nvm and fnm install node — and the npm-global CLIs that sit beside it — under
/// a per-version directory, so there is no fixed path to hard-code. Take the
/// newest [`MAX_NODE_VERSIONS_IN_FALLBACK`] per manager.
#[cfg(target_os = "macos")]
fn node_version_manager_bin_dirs(home: &Path) -> Vec<PathBuf> {
    let roots = [
        home.join(".nvm/versions/node"),
        home.join("Library/Application Support/fnm/node-versions"),
    ];
    let mut dirs = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        versions.sort_by_cached_key(|path| {
            std::cmp::Reverse(version_sort_key(
                &path.file_name().unwrap_or_default().to_string_lossy(),
            ))
        });
        for version in versions.into_iter().take(MAX_NODE_VERSIONS_IN_FALLBACK) {
            // nvm lays out `<version>/bin`; fnm `<version>/installation/bin`.
            dirs.push(version.join("bin"));
            dirs.push(version.join("installation/bin"));
        }
    }
    dirs
}

/// PATH to use when the login-shell capture fails: the process PATH plus
/// [`well_known_bin_dirs`]. The process PATH is correct for a terminal launch
/// and minimal (`/usr/bin:/bin:/usr/sbin:/sbin`) for a GUI launch — which is why
/// it alone finds none of the harness CLIs. Appending, never prepending, means
/// this can only *add* resolution candidates: a PATH the user actually
/// configured still wins.
#[cfg(target_os = "macos")]
fn fallback_path() -> String {
    merge_path(
        &std::env::var("PATH").unwrap_or_default(),
        &well_known_bin_dirs(),
    )
}

/// Append `extras` to `process_path`, preserving the original order and dropping
/// duplicates. Pure so the ordering and dedup rules are testable without
/// depending on what happens to exist on the machine running the tests.
#[cfg(target_os = "macos")]
fn merge_path(process_path: &str, extras: &[PathBuf]) -> String {
    let mut entries: Vec<String> = Vec::new();
    for entry in process_path.split(':').filter(|entry| !entry.is_empty()) {
        if !entries.iter().any(|kept| kept == entry) {
            entries.push(entry.to_owned());
        }
    }
    for dir in extras {
        let dir = dir.to_string_lossy().into_owned();
        if !entries.contains(&dir) {
            entries.push(dir);
        }
    }
    entries.join(":")
}

/// Whether a capture may be re-attempted after `failures` consecutive failures,
/// the most recent `elapsed` ago. The gap doubles per failure up to
/// [`PATH_RETRY_MAX_COOLDOWN`]. Pure so the rule is testable without sleeping.
#[cfg(target_os = "macos")]
fn should_retry_capture(elapsed: Duration, failures: u32) -> bool {
    let backoff = PATH_RETRY_COOLDOWN
        .saturating_mul(2_u32.saturating_pow(failures.saturating_sub(1).min(16)))
        .min(PATH_RETRY_MAX_COOLDOWN);
    elapsed >= backoff
}

/// Where the PATH currently in use came from. Reaches the frontend on every
/// install status so the UI can distinguish "still working it out" from "this is
/// the answer" — without it, a probe that races the first capture renders as a
/// confident "Not installed", which is the very symptom this module exists to
/// stop producing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PathSource {
    /// No answer yet — either unstarted or a capture is running. Results derived
    /// now are provisional.
    Capturing,
    /// The user's login shell answered — this is the terminal PATH.
    LoginShell,
    /// The capture failed; [`fallback_path`] is in use. Results are real but may
    /// miss a CLI installed outside the well-known locations.
    Fallback,
}

/// The capture lifecycle, as an explicit state rather than a set of fields whose
/// combinations have to be interpreted.
///
/// This replaced a six-field struct where conditions were *inferred* from which
/// fields happened to be empty. Two separate bugs came out of that: a
/// normalization step that mistook "a capture is running" for "the state is
/// damaged" and churned endlessly, and a completion path that mistook "the
/// thread panicked" for "I was superseded" and could respawn into the same
/// panic. Supersession is now a comparison of two named numbers, not a deduction
/// from absence.
#[cfg(target_os = "macos")]
#[derive(Debug)]
enum CaptureState {
    /// Nothing resolved, nothing running. The next reader arms a capture.
    Idle,
    /// A capture is running for this generation. Superseded when it differs from
    /// [`PathCache::generation`] — that is the Recheck-during-capture case, and
    /// it is the state the old representation could not express.
    ///
    /// `prior_failures` rides along because the backoff must keep escalating
    /// across retries: without it, every attempt looks like the first failure
    /// and a permanently-broken profile respawns a shell at a fixed interval
    /// forever.
    Capturing {
        generation: u64,
        prior_failures: u32,
        /// Set when this capture is superseded, so the shell is abandoned
        /// immediately rather than waited out before the replacement starts.
        cancel: Arc<std::sync::atomic::AtomicBool>,
    },
    /// The login shell answered.
    Resolved { path: String },
    /// The last attempt failed; `at`/`count` drive the retry backoff. The
    /// degraded PATH is deliberately *not* stored — caching a fallback is the
    /// original bug this module was rewritten to fix.
    Failed { at: std::time::Instant, count: u32 },
}

#[cfg(target_os = "macos")]
struct PathCache {
    /// The generation currently *requested*. Bumped by invalidation only.
    generation: u64,
    /// Bumped on every state transition — publish, failure, and invalidation.
    /// A probe reads this before and after its work to detect that the PATH
    /// changed underneath it, which is what keeps a result from being labelled
    /// with a PATH it never used.
    revision: u64,
    state: CaptureState,
}

#[cfg(target_os = "macos")]
static PATH_CACHE: Mutex<PathCache> = Mutex::new(PathCache {
    generation: 0,
    revision: 0,
    state: CaptureState::Idle,
});

/// Signalled whenever the capture state reaches a terminal value. Lets an
/// explicit Recheck wait for a real answer instead of probing against the
/// fallback and reporting it as final.
///
/// Correctness rests on one invariant worth stating: every `Capturing`→terminal
/// transition happens on the capture thread, before that capture's
/// [`CaptureGuard`] drops and calls `notify_all`. A waiter therefore never
/// observes a gap between the state settling and being woken — including across
/// a supersede, where the guard re-enters `Capturing` before notifying.
#[cfg(target_os = "macos")]
static CAPTURE_DONE: std::sync::Condvar = std::sync::Condvar::new();

/// Async counterpart to [`CAPTURE_DONE`], carrying the revision. Dispatch needs
/// to wait for a settled PATH without blocking an async worker — which is the
/// defect this module was rewritten to remove, so reintroducing it on the send
/// path would be self-defeating.
#[cfg(target_os = "macos")]
static REVISION_TX: std::sync::OnceLock<tokio::sync::watch::Sender<u64>> =
    std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
fn revision_tx() -> &'static tokio::sync::watch::Sender<u64> {
    REVISION_TX.get_or_init(|| tokio::sync::watch::channel(0).0)
}

/// Subscribe to PATH-state changes. The app layer uses this to tell the frontend
/// when a capture settles, replacing an earlier callback registry: a callback
/// had to be invoked outside the lock, which meant notification was scattered
/// across sites and the Tauri-facing concern lived in this crate. A stream has
/// one publication point and leaves the emit where it belongs.
#[cfg(target_os = "macos")]
#[must_use]
pub fn subscribe_revisions() -> tokio::sync::watch::Receiver<u64> {
    revision_tx().subscribe()
}

/// Take a lock, recovering from poisoning rather than degrading forever.
///
/// Poisoning means a panic happened mid-mutation. Treating that as fatal would
/// reintroduce the failure shape this module exists to eliminate — a state no
/// user action can clear. The state machine has no half-written representation
/// to repair: a panic can only leave it in `Capturing`, which the owning
/// [`CaptureGuard`] resolves on unwind.
#[cfg(unix)]
fn lock_cache_or_recover<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("harness PATH lock was poisoned; recovering");
        poisoned.into_inner()
    })
}

#[cfg(target_os = "macos")]
fn lock_path_cache() -> std::sync::MutexGuard<'static, PathCache> {
    lock_cache_or_recover(&PATH_CACHE)
}

/// Advance to a new state, bumping the revision and waking waiters.
#[cfg(target_os = "macos")]
fn transition(cache: &mut PathCache, state: CaptureState) {
    cache.state = state;
    bump_revision(cache);
}

/// Mark the resolved PATH as having changed, without changing the state. Used by
/// invalidation while a capture is already running: the state stays `Capturing`
/// for the superseded generation, but readers must still learn the previous
/// answer is void.
#[cfg(target_os = "macos")]
fn bump_revision(cache: &mut PathCache) {
    cache.revision = cache.revision.wrapping_add(1);
    // `send_replace`, not `send`: `send` fails *and leaves the value unchanged*
    // when there are no receivers, which is the common case since subscribers
    // are transient. Correctness would then rest on `ensure_path_settled`
    // re-reading the state under the lock after subscribing — true today, but an
    // undocumented ordering doing load-bearing work. `send_replace` always
    // succeeds, so no wakeup depends on it.
    //
    // Safe under the lock: a watch send neither blocks nor re-enters this mutex.
    // Nothing that could (an arbitrary callback) belongs here — the app layer
    // subscribes to this stream instead.
    revision_tx().send_replace(cache.revision);
}

/// Owns the `Capturing` state for one capture attempt, and settles it on drop —
/// including on an unwind. Without this a panicking capture thread would strand
/// the state in `Capturing` forever: no further capture would ever start and
/// Recheck would silently do nothing, which is the same "stuck until relaunch"
/// shape as the bug this module exists to fix.
#[cfg(target_os = "macos")]
struct CaptureGuard {
    generation: u64,
}

#[cfg(target_os = "macos")]
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        {
            let mut cache = lock_path_cache();
            // Still `Capturing` for our generation means `publish_capture` never
            // ran — the thread panicked. Fall back to `Idle` so the next reader
            // re-arms, rather than respawning here and risking a panic loop.
            if matches!(cache.state, CaptureState::Capturing { generation, .. } if generation == self.generation)
            {
                if self.generation == cache.generation {
                    tracing::warn!("harness PATH capture ended without publishing; resetting");
                    transition(&mut cache, CaptureState::Idle);
                } else {
                    // Superseded mid-flight: the invalidation that bumped the
                    // generation could not start its own capture while we held
                    // this state, so starting it is our responsibility. Nothing
                    // else will, and a Recheck clicked during a capture would
                    // otherwise wait forever.
                    start_capture(&mut cache);
                }
            }
        }
        CAPTURE_DONE.notify_all();
    }
}

/// The PATH used to locate and run harness CLIs: the user's login-shell PATH
/// once captured, [`fallback_path`] until then. A GUI launch
/// (Spotlight/Launchpad/login restore) inherits only a minimal PATH that omits
/// nvm, `~/.local/bin`, Homebrew, etc.; the capture recovers the full terminal
/// PATH.
///
/// **Never waits.** The lock is held only long enough to read the state and
/// possibly arm a capture; the shell itself runs on its own thread. An earlier
/// design held the lock across the capture, which meant a slow shell froze every
/// PATH consumer — including turn dispatch — for the whole timeout. Callers that
/// need the final answer use [`await_capture`] or [`ensure_path_settled`].
#[cfg(target_os = "macos")]
fn resolved_path() -> String {
    let mut cache = lock_path_cache();
    if let CaptureState::Resolved { path } = &cache.state {
        return path.clone();
    }
    arm_capture_if_due(&mut cache);
    drop(cache);
    fallback_path()
}

/// Arm a capture unless one is running or the retry backoff is still in effect.
#[cfg(target_os = "macos")]
fn arm_capture_if_due(cache: &mut PathCache) {
    let due = match &cache.state {
        CaptureState::Idle => true,
        CaptureState::Failed { at, count } => should_retry_capture(at.elapsed(), *count),
        CaptureState::Capturing { .. } | CaptureState::Resolved { .. } => false,
    };
    if due {
        start_capture(cache);
    }
}

/// Consecutive failures recorded so far, carried into the next attempt.
#[cfg(target_os = "macos")]
fn prior_failures(state: &CaptureState) -> u32 {
    match state {
        CaptureState::Failed { count, .. } => *count,
        CaptureState::Capturing { prior_failures, .. } => *prior_failures,
        CaptureState::Idle | CaptureState::Resolved { .. } => 0,
    }
}

/// Start a capture with the failure history cleared — the user-initiated path.
#[cfg(target_os = "macos")]
fn start_capture_fresh(cache: &mut PathCache) {
    transition(cache, CaptureState::Idle);
    start_capture(cache);
}

/// Enter `Capturing` for the current generation and spawn the shell. Caller
/// holds the lock; the spawned thread does not.
#[cfg(target_os = "macos")]
fn start_capture(cache: &mut PathCache) {
    let generation = cache.generation;
    let prior_failures = prior_failures(&cache.state);
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    transition(
        cache,
        CaptureState::Capturing {
            generation,
            prior_failures,
            cancel: Arc::clone(&cancel),
        },
    );
    std::thread::spawn(move || {
        // Settles the state on every exit path, including an unwind.
        let _guard = CaptureGuard { generation };
        let captured = run_login_shell_path(&cancel);
        let mut cache = lock_path_cache();
        publish_capture(&mut cache, generation, captured, std::time::Instant::now());
    });
}

/// Fold a finished capture into the cache.
///
/// **Only a success is stored, and only if it is still current.** A failure
/// records its timestamp (gating the retry backoff) without caching the degraded
/// PATH, so a bad answer can never outlive the conditions that produced it. A
/// result from a superseded generation is left for [`CaptureGuard`] to restart —
/// it describes a PATH the user has since asked us to re-read.
#[cfg(target_os = "macos")]
fn publish_capture(
    cache: &mut PathCache,
    generation: u64,
    captured: Option<String>,
    attempted_at: std::time::Instant,
) {
    if cache.generation != generation {
        tracing::debug!(
            stale_generation = generation,
            current_generation = cache.generation,
            "discarding superseded harness PATH capture"
        );
        return;
    }
    if let Some(path) = captured {
        tracing::info!(source = "login_shell", %path, "resolved harness PATH");
        transition(cache, CaptureState::Resolved { path });
    } else {
        let count = prior_failures(&cache.state).saturating_add(1);
        tracing::warn!(
            source = "fallback",
            path = %fallback_path(),
            timeout_secs = PATH_CAPTURE_TIMEOUT.as_secs(),
            consecutive_failures = count,
            "login-shell PATH capture failed; using the process PATH widened with well-known \
             install directories. Harness CLIs installed elsewhere will report as not \
             installed until a later capture succeeds."
        );
        transition(
            cache,
            CaptureState::Failed {
                at: attempted_at,
                count,
            },
        );
    }
}

#[cfg(target_os = "macos")]
fn source_of(state: &CaptureState) -> PathSource {
    match state {
        CaptureState::Resolved { .. } => PathSource::LoginShell,
        CaptureState::Failed { .. } => PathSource::Fallback,
        CaptureState::Idle | CaptureState::Capturing { .. } => PathSource::Capturing,
    }
}

/// Where the PATH now in use came from, paired with the revision it was read at.
/// A caller that probes across a revision change learns its result describes a
/// PATH that is no longer current — see `install_status_for`.
#[must_use]
pub fn path_source_at() -> (PathSource, u64) {
    #[cfg(target_os = "macos")]
    {
        let cache = lock_path_cache();
        (source_of(&cache.state), cache.revision)
    }
    #[cfg(not(target_os = "macos"))]
    (PathSource::LoginShell, 0)
}

/// The current revision. Cheap; used to detect that the PATH changed while a
/// probe was running.
#[must_use]
pub fn path_revision() -> u64 {
    #[cfg(target_os = "macos")]
    {
        lock_path_cache().revision
    }
    #[cfg(not(target_os = "macos"))]
    0
}

/// Where the PATH now in use came from.
#[must_use]
pub fn path_source() -> PathSource {
    path_source_at().0
}

/// Block until the capture settles (bounded by `timeout`), then report the
/// resulting source. Arms one if none is running and the backoff allows it.
///
/// **Blocking by design** — for the explicit Recheck action, which must not
/// report a provisional answer as final. Call from a blocking context
/// (`spawn_blocking`), never an async worker; async callers use
/// [`ensure_path_settled`], and routine readers use [`resolved_path`], which
/// never waits.
#[must_use]
pub fn await_capture(timeout: Duration) -> PathSource {
    #[cfg(target_os = "macos")]
    {
        let mut cache = lock_path_cache();
        arm_capture_if_due(&mut cache);
        let deadline = std::time::Instant::now() + timeout;
        // Waits out a chained restart too: a superseded capture's guard re-enters
        // `Capturing` before waking us, so we stay parked until the generation
        // the user actually asked for reaches a terminal state.
        while matches!(cache.state, CaptureState::Capturing { .. }) {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                break;
            };
            let (guard, _) = CAPTURE_DONE
                .wait_timeout(cache, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cache = guard;
        }
        source_of(&cache.state)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = timeout;
        PathSource::LoginShell
    }
}

/// Await a settled PATH without blocking a runtime worker, then report the
/// source. For turn dispatch: an agent spawned during the capture window would
/// otherwise run its entire turn against the fallback PATH, and unlike detection
/// there is no corrective re-probe once the process is running.
///
/// **Accepted residual:** on a machine whose shell is slower than `timeout` —
/// the login-restore case this module targets — this returns
/// [`PathSource::Capturing`] and the caller proceeds on the fallback anyway. The
/// alternative is blocking Send for the full capture timeout, which is the
/// behavior this redesign removed.
pub async fn ensure_path_settled(timeout: Duration) -> PathSource {
    #[cfg(target_os = "macos")]
    {
        {
            let mut cache = lock_path_cache();
            arm_capture_if_due(&mut cache);
            if !matches!(cache.state, CaptureState::Capturing { .. }) {
                return source_of(&cache.state);
            }
        }
        let mut rx = revision_tx().subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            {
                let cache = lock_path_cache();
                if !matches!(cache.state, CaptureState::Capturing { .. }) {
                    return source_of(&cache.state);
                }
            }
            if tokio::time::timeout_at(deadline, rx.changed())
                .await
                .is_err()
            {
                return PathSource::Capturing;
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = timeout;
        PathSource::LoginShell
    }
}

/// Start resolving the PATH at app startup so the answer is ready before
/// anything asks. Nothing blocks on it: readers use [`fallback_path`] until it
/// lands, and the app layer — subscribed via [`subscribe_revisions`] — tells the
/// frontend to re-probe once it does.
pub fn warm_path_cache() {
    #[cfg(target_os = "macos")]
    {
        let mut cache = lock_path_cache();
        arm_capture_if_due(&mut cache);
    }
}

/// Discard the cached PATH and start a fresh capture. Backs the "Recheck"
/// action: the user-visible symptom of a failed capture is every harness reading
/// "not installed", and they need a retry that isn't "quit and relaunch".
///
/// Bumping the generation invalidates any capture already in flight (its result
/// is discarded and its guard starts the replacement) and clears the retry
/// backoff, so an explicit user action is never swallowed by a cooldown.
pub fn invalidate_path_cache() {
    #[cfg(target_os = "macos")]
    {
        let mut cache = lock_path_cache();
        cache.generation = cache.generation.wrapping_add(1);
        invalidate_well_known_bin_dirs();
        // Clear the failure history, not just the cooldown gate. A deliberate
        // Recheck is evidence the user changed something; carrying the count
        // forward would let a few clicks on a broken profile push the *next
        // automatic* retry toward its 5-minute ceiling. (The Recheck's own
        // attempt always runs immediately — this only affects what happens if
        // that attempt also fails.) Both branches must clear it: a Recheck that
        // lands mid-retry supersedes a `Capturing` state whose `prior_failures`
        // the restarting guard would otherwise carry straight back out.
        match cache.state {
            CaptureState::Capturing {
                generation,
                ref cancel,
                ..
            } => {
                // Cancel the doomed attempt rather than waiting it out: its
                // result is already superseded, and the replacement can't start
                // until it releases the slot. Its guard sees the generation
                // mismatch and starts the replacement. Spawning a second shell
                // here instead would race two captures for one slot.
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                let cancel = Arc::clone(cancel);
                cache.state = CaptureState::Capturing {
                    generation,
                    prior_failures: 0,
                    cancel,
                };
                bump_revision(&mut cache);
            }
            _ => start_capture_fresh(&mut cache),
        }
    }
}

/// Set the resolved PATH on a harness subprocess so the spawned CLI — and the
/// tools *it* shells out to (git, node, ripgrep, an `env`-shebang interpreter,
/// …) — resolve the same way they would in the user's terminal. Without this, a
/// GUI-launched app hands the child its minimal PATH and those lookups fail even
/// when the harness binary itself was found. No-op off macOS, where the
/// inherited PATH is already correct.
pub fn apply_path_env(command: &mut tokio::process::Command) {
    #[cfg(target_os = "macos")]
    command.env("PATH", resolved_path());
    #[cfg(not(target_os = "macos"))]
    let _ = command;
}

/// Resolve a harness binary path to an absolute path. Absolute paths are
/// trusted as-is (spawn will return `NotFound` if the binary is missing and
/// the caller maps that to `BinaryNotFound`). Relative names go through `which`,
/// searching the resolved login-shell PATH on macOS so a GUI launch finds the
/// same binary the terminal would.
pub fn resolve_binary(path: &Path) -> Result<PathBuf, DispatchError> {
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    #[cfg(target_os = "macos")]
    let found = which::which_in(path, Some(resolved_path()), std::path::Path::new("."));
    #[cfg(not(target_os = "macos"))]
    let found = which::which(path);
    found.map_err(|_| DispatchError::BinaryNotFound)
}

/// Check whether a harness binary is present and executable. Absolute paths are
/// checked directly; relative names search `which` over the resolved
/// login-shell PATH on macOS so a GUI launch probes the same locations the
/// terminal would.
pub fn probe_binary(path: &Path) -> Result<(), DispatchError> {
    #[cfg(target_os = "macos")]
    let found = which::which_in(path, Some(resolved_path()), std::path::Path::new("."));
    #[cfg(not(target_os = "macos"))]
    let found = which::which(path);
    found.map(|_| ()).map_err(|_| DispatchError::BinaryNotFound)
}

/// Extract just the version number from a `--version` line, since CLIs pad it
/// differently: `claude` prints `2.1.156 (Claude Code)`, `codex` prints
/// `codex-cli 0.134.0`, others print a bare `0.44.0`. Returns the first
/// whitespace-separated token that looks like a dotted version (optionally
/// `v`-prefixed, which is stripped), or `None` if the line has none — callers
/// then show "Installed" without a number rather than echoing the binary name.
#[must_use]
pub fn parse_cli_version(line: &str) -> Option<String> {
    line.split_whitespace()
        .map(|tok| tok.strip_prefix('v').unwrap_or(tok))
        .find(|tok| {
            let mut segments = tok.split('.');
            let major_numeric = segments
                .next()
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
            // Require at least `<digits>.<digit>…` so "codex-cli" (no dot) and
            // "(Claude" are rejected but "2.1.156" / "0.44.0" match.
            let minor_starts_numeric = segments
                .next()
                .is_some_and(|s| s.bytes().next().is_some_and(|b| b.is_ascii_digit()));
            major_numeric && minor_starts_numeric
        })
        .map(str::to_owned)
}

/// Best-effort version string for a harness CLI: the first line of
/// `<binary> --version`, trimmed. Returns `None` when the binary can't be
/// resolved/invoked or reports nothing — the value is display-only, never
/// load-bearing, so any failure collapses to "unknown" rather than an error.
pub fn fetch_version(binary: &Path) -> Option<String> {
    let resolved = resolve_binary(binary).ok()?;
    let mut command = std::process::Command::new(&resolved);
    // Same PATH augmentation as a dispatched turn: a `--version` that shells
    // out (or is an env-shebang script) must resolve its interpreter/tools the
    // way a real turn would, so the displayed version matches what runs.
    #[cfg(target_os = "macos")]
    command.env("PATH", resolved_path());
    command.arg("--version");
    // Bounded like the PATH capture: this runs on a blocking pool thread, and a
    // `--version` that hangs (a wedged CLI, a stalled network check in a wrapper
    // script) would otherwise occupy that thread for the life of the app.
    // Status-gated, unlike the PATH capture: a `--version` that exits non-zero
    // has no version to report, so its output is noise rather than an answer.
    // An *unknown* verdict is accepted rather than rejected — the command ran and
    // produced output, and `parse_cli_version` rejects anything that isn't a
    // version string, so the parse is the real gate in that case.
    let output = run_bounded(command, VERSION_PROBE_TIMEOUT)?;
    if output.success == Some(false) {
        return None;
    }
    let line = output.stdout.lines().next().unwrap_or("").trim().to_owned();
    (!line.is_empty()).then_some(line)
}

/// Time bound for a `<binary> --version` probe. Far tighter than the PATH
/// capture: printing a version string is local work with no startup files to
/// source, so anything this slow is wedged rather than busy.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Drain a child's stderr stream into a bounded tail buffer.
///
/// Each line is also emitted at `tracing::debug!` with the harness name as
/// context so a `RUST_LOG=debug` run shows the stderr inline with the rest
/// of the trace. `harness_name` is the short identifier ("claude", "codex")
/// used in the log message — passed as a parameter so this function isn't
/// duplicated per adapter just to change the log prefix.
pub async fn drain_stderr(
    stderr: tokio::process::ChildStderr,
    agent_id: AgentId,
    turn_id: TurnId,
    tail: Arc<Mutex<VecDeque<String>>>,
    harness_name: &'static str,
) {
    drain_stderr_with_observer(stderr, agent_id, turn_id, tail, harness_name, |_| {}).await;
}

/// [`drain_stderr`], plus a per-line observer invoked **before** the line
/// reaches the bounded tail.
///
/// Exists because the tail is a *display* buffer — capped at
/// [`STDERR_TAIL_CAPACITY`] and front-evicting — so it cannot be the source of
/// truth for any signal that must not be missed. An adapter that classifies on
/// stderr content (Antigravity reads auth and `Error:` lines from it) has to
/// record what it saw as the line arrives; rescanning the tail later loses
/// anything a chatty subsequent burst has already evicted, permanently and
/// silently.
///
/// The observer is a generic extension point, not a harness-specific hook: the
/// predicate and the state it accumulates live in the calling adapter. Keeping
/// the read loop, `tracing` emission, tail eviction, and read-error handling in
/// one place here is the point — a per-adapter copy of this loop would drift
/// most easily in its least-exercised branch, the read-error path.
pub async fn drain_stderr_with_observer<F>(
    stderr: tokio::process::ChildStderr,
    agent_id: AgentId,
    turn_id: TurnId,
    tail: Arc<Mutex<VecDeque<String>>>,
    harness_name: &'static str,
    mut observe: F,
) where
    F: FnMut(&str) + Send,
{
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                tracing::debug!(agent_id = %agent_id, %turn_id, "{harness_name} stderr: {line}");
                observe(&line);
                if let Ok(mut buf) = tail.lock() {
                    if buf.len() >= STDERR_TAIL_CAPACITY {
                        buf.pop_front();
                    }
                    buf.push_back(line);
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(agent_id = %agent_id, %turn_id, error = %e, "stderr read error");
                break;
            }
        }
    }
}

/// Return a single-line, length-bounded representation of the captured
/// stderr tail buffer. Empty string if no lines were captured.
///
/// Length-bounding is performed on **char boundaries** — slicing a String
/// by byte offsets can land mid-UTF-8 and panic (real risk with non-ASCII
/// paths or error messages in stderr).
pub fn format_stderr_tail(tail: &Mutex<VecDeque<String>>) -> String {
    let Ok(buf) = tail.lock() else {
        return String::new();
    };
    if buf.is_empty() {
        return String::new();
    }
    let joined = buf.iter().cloned().collect::<Vec<_>>().join(" | ");
    if joined.len() > STDERR_MESSAGE_MAX_LEN {
        // Find the lowest char boundary at or after `joined.len() - MAX`.
        // Walk byte positions forward from that target until we hit a
        // valid boundary; result is guaranteed to be `<= MAX` chars worth
        // of suffix (typically fewer if multi-byte chars sit at the edge).
        let target = joined.len() - STDERR_MESSAGE_MAX_LEN;
        let start = (target..=joined.len())
            .find(|&i| joined.is_char_boundary(i))
            .unwrap_or(joined.len());
        let mut truncated = joined[start..].to_owned();
        truncated.insert(0, '…');
        truncated
    } else {
        joined
    }
}

/// Terminate a harness subprocess and **the whole process group** it leads:
/// SIGTERM the group, give the parent up to [`TERMINATE_GRACE`] to flush and
/// exit, then SIGKILL the group unconditionally to sweep any survivor. Reaps
/// the direct child (so callers must not `wait()` again, or must tolerate an
/// already-reaped result).
///
/// **Why the group, not just the PID.** `tokio::process::Child::kill` signals
/// only the spawned PID. For a two-process tree (Codex's Node parent + Rust
/// child), killing only the parent leaves the child holding the write end of
/// the stdout/stderr pipes; a drain task then blocks forever on an EOF that
/// never arrives. `process_group(0)` at spawn (used by every adapter here)
/// makes the spawned child its own process-group leader (`pgid == pid`), so
/// passing its PID to `killpg` signals every process in the group.
///
/// **Why the final SIGKILL is unconditional.** Waiting on the direct child
/// only tells us the *parent* exited — not that the *group* is empty. A
/// descendant that ignores SIGTERM and outlives the parent (still holding our
/// pipes) would otherwise be missed, and the adapter's stderr drain would hang
/// forever. So after the parent's grace window we always `killpg(SIGKILL)`:
/// when the group already exited it's a harmless `ESRCH` no-op; when a survivor
/// remains it's what actually tears it down. We do **not** `wait` the survivor
/// — it's a grandchild, reparented to (and reaped by) init once the parent
/// died; we only reap our direct child.
///
/// **Why SIGTERM-first.** A graceful signal lets the harness flush and leave
/// its session file in a resumable state — load-bearing for both cancellation
/// (the user stopped a healthy turn) and the adapter error paths (a parse /
/// stream-read error means *we* stopped reading, not that the harness is
/// unhealthy; it may be mid-write to its session file). The grace is a ceiling
/// for the parent: a process that exits promptly on SIGTERM adds no latency.
///
/// Cross-platform: non-unix has no process-group concept, so it falls back to
/// `child.kill()` (SIGKILL-equivalent).
pub async fn terminate_then_kill(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        let Some(pid) = child.id() else {
            // No PID → already exited/reaped; nothing to signal.
            let _ = child.wait().await;
            return;
        };
        let group = nix::unistd::Pid::from_raw(pid.cast_signed());
        killpg_signal(group, nix::sys::signal::Signal::SIGTERM);
        // Give the parent the grace window to flush + exit on SIGTERM. We
        // ignore the result: whether it exited or timed out, the unconditional
        // group SIGKILL below is what guarantees teardown (see doc).
        let _ = tokio::time::timeout(TERMINATE_GRACE, child.wait()).await;
        killpg_signal(group, nix::sys::signal::Signal::SIGKILL);
        let _ = child.wait().await;
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill().await;
    }
}

/// Signal a process group, ignoring `ESRCH` ("no such process" — the group
/// already exited between the caller's check and this signal, which is a
/// no-op success, not a failure).
#[cfg(unix)]
fn killpg_signal(pgid: nix::unistd::Pid, signal: nix::sys::signal::Signal) {
    let _ = nix::sys::signal::killpg(pgid, signal);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_stderr_tail_returns_empty_string_when_buffer_is_empty() {
        let tail: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
        assert_eq!(format_stderr_tail(&tail), "");
    }

    #[test]
    fn format_stderr_tail_joins_lines_with_pipe_separator() {
        let tail: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
        tail.lock().unwrap().push_back("first".to_owned());
        tail.lock().unwrap().push_back("second".to_owned());
        assert_eq!(format_stderr_tail(&tail), "first | second");
    }

    #[test]
    fn format_stderr_tail_handles_non_ascii_at_truncation_boundary() {
        // Regression: byte-slicing on a String can land mid-UTF-8 and
        // panic with "byte index N is not a char boundary." Stderr from
        // real subprocesses often contains paths or messages with
        // multi-byte characters (e.g., accented usernames, emoji, smart
        // quotes). Truncation must walk to a char boundary.
        let tail: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
        // 600 ASCII chars + 150 "…" (3 bytes each) → 1050 bytes total,
        // well over the 800-byte truncation threshold. The byte at
        // (len - 800) almost certainly lands mid-character.
        let mut payload = "A".repeat(600);
        for _ in 0..150 {
            payload.push('…');
        }
        tail.lock().unwrap().push_back(payload);

        let result = format_stderr_tail(&tail);
        // Critically: NO PANIC. Plus the leading-ellipsis prefix marks
        // the truncation visually.
        assert!(
            result.starts_with('…'),
            "truncated output should be prefixed with …"
        );
        // Total chars bounded by STDERR_MESSAGE_MAX_LEN + a small constant
        // (the prefix and the boundary walk overhead).
        assert!(result.chars().count() < 850);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn job_control_off_flag_keys_on_the_shell_basename() {
        assert_eq!(job_control_off_flag("/bin/zsh"), Some("+m"));
        assert_eq!(job_control_off_flag("/opt/homebrew/bin/bash"), Some("+m"));
        assert_eq!(job_control_off_flag("zsh"), Some("+m"));
        // fish rejects `+m` outright — passing it would break the capture even
        // in contexts that work today.
        assert_eq!(job_control_off_flag("/opt/homebrew/bin/fish"), None);
        assert_eq!(job_control_off_flag(""), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_sentinel_path_extracts_value_between_markers() {
        let out = format!("{PATH_SENTINEL}/usr/bin:/bin{PATH_SENTINEL}");
        assert_eq!(parse_sentinel_path(&out).as_deref(), Some("/usr/bin:/bin"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_sentinel_path_ignores_surrounding_banner_noise() {
        // A login shell may print MOTD/banner lines around our printf; the
        // sentinels let us recover the value regardless.
        let out = format!("Welcome to zsh\nbanner line\n{PATH_SENTINEL}/a:/b:/c{PATH_SENTINEL}\n");
        assert_eq!(parse_sentinel_path(&out).as_deref(), Some("/a:/b:/c"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_sentinel_path_none_when_missing_or_empty() {
        assert_eq!(parse_sentinel_path("no markers here"), None);
        // Both markers present but bracketing nothing → None.
        assert_eq!(
            parse_sentinel_path(&format!("{PATH_SENTINEL}{PATH_SENTINEL}")),
            None
        );
        // Only the opening marker → can't bracket a value → None.
        assert_eq!(parse_sentinel_path(&format!("{PATH_SENTINEL}/a:/b")), None);
    }

    /// A fresh, unset cancel flag for test-constructed `Capturing` states.
    #[cfg(target_os = "macos")]
    fn no_cancel() -> Arc<std::sync::atomic::AtomicBool> {
        Arc::new(std::sync::atomic::AtomicBool::new(false))
    }

    #[cfg(target_os = "macos")]
    fn capturing_cache(generation: u64) -> PathCache {
        PathCache {
            generation,
            revision: 0,
            state: CaptureState::Capturing {
                generation,
                prior_failures: 0,
                cancel: no_cancel(),
            },
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn should_retry_capture_backs_off_per_failure_and_caps() {
        // The behavior the permanent-"not installed" bug turned on: a failed
        // capture must be retryable, but not on every single probe — and a
        // machine where it always fails must not respawn a shell forever at a
        // fixed interval.
        assert!(!should_retry_capture(Duration::ZERO, 1));
        assert!(!should_retry_capture(
            PATH_RETRY_COOLDOWN
                .checked_sub(Duration::from_millis(1))
                .expect("cooldown exceeds 1ms"),
            1
        ));
        assert!(should_retry_capture(PATH_RETRY_COOLDOWN, 1));

        // Second consecutive failure doubles the gap.
        assert!(!should_retry_capture(PATH_RETRY_COOLDOWN, 2));
        assert!(should_retry_capture(PATH_RETRY_COOLDOWN * 2, 2));

        // And the escalation is capped, so recovery stays possible.
        assert!(should_retry_capture(PATH_RETRY_MAX_COOLDOWN, 99));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn publish_capture_stores_a_success_but_never_a_failure() {
        // The regression this whole cache shape exists for: a fallback PATH
        // must not be remembered. Caching it is what made one slow login-shell
        // probe pin "not installed" for the lifetime of the process.
        let mut cache = capturing_cache(0);
        let attempted_at = std::time::Instant::now();

        publish_capture(&mut cache, 0, None, attempted_at);
        assert!(
            matches!(cache.state, CaptureState::Failed { count: 1, .. }),
            "a failed capture must record the failure, not a PATH: {:?}",
            cache.state
        );
        assert_eq!(source_of(&cache.state), PathSource::Fallback);

        // Consecutive failures accumulate, which is what escalates the backoff.
        // Re-arm the way `start_capture` does — carrying the failure count, which
        // is what keeps the backoff escalating instead of resetting on every attempt.
        cache.state = CaptureState::Capturing {
            generation: 0,
            prior_failures: prior_failures(&cache.state),
            cancel: no_cancel(),
        };
        publish_capture(&mut cache, 0, None, attempted_at);
        assert!(
            matches!(cache.state, CaptureState::Failed { count: 2, .. }),
            "consecutive failures must accumulate across the Capturing transition: {:?}",
            cache.state
        );

        // A later success stores, and clears the backoff so a subsequent
        // failure gets its own full retry window.
        cache.state = CaptureState::Capturing {
            generation: 0,
            prior_failures: 2,
            cancel: no_cancel(),
        };
        publish_capture(&mut cache, 0, Some("/a:/b".to_owned()), attempted_at);
        assert!(
            matches!(&cache.state, CaptureState::Resolved { path } if path == "/a:/b"),
            "{:?}",
            cache.state
        );
        assert_eq!(source_of(&cache.state), PathSource::LoginShell);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn every_transition_advances_the_revision() {
        // The revision is what lets a probe notice the PATH changed underneath
        // it. A transition that forgot to bump it would let a result be labelled
        // with a PATH it never used — silently, and only under a race.
        let mut cache = capturing_cache(0);
        let start = cache.revision;
        publish_capture(
            &mut cache,
            0,
            Some("/a".to_owned()),
            std::time::Instant::now(),
        );
        let after_success = cache.revision;
        assert_ne!(after_success, start);

        cache.state = CaptureState::Capturing {
            generation: 0,
            prior_failures: 0,
            cancel: no_cancel(),
        };
        publish_capture(&mut cache, 0, None, std::time::Instant::now());
        assert_ne!(cache.revision, after_success);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn publish_capture_discards_a_result_from_a_superseded_generation() {
        // A capture started before a Recheck describes the PATH the user just
        // asked us to re-read. Publishing it would undo the Recheck.
        let mut cache = capturing_cache(6);
        cache.generation = 7;

        publish_capture(
            &mut cache,
            6,
            Some("/stale".to_owned()),
            std::time::Instant::now(),
        );
        assert!(
            matches!(cache.state, CaptureState::Capturing { generation: 6, .. }),
            "a stale capture must neither publish nor settle the state: {:?}",
            cache.state
        );

        publish_capture(
            &mut cache,
            7,
            Some("/current".to_owned()),
            std::time::Instant::now(),
        );
        assert!(matches!(&cache.state, CaptureState::Resolved { path } if path == "/current"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn arm_capture_if_due_respects_the_backoff_and_never_doubles_up() {
        // A fresh failure stays in backoff — that is what stops a broken profile
        // from respawning a shell on every window focus. A resolved PATH is not
        // re-captured, and a running capture is never joined by a second.
        let mut cache = PathCache {
            generation: 0,
            revision: 0,
            state: CaptureState::Failed {
                at: std::time::Instant::now(),
                count: 1,
            },
        };
        arm_capture_if_due(&mut cache);
        assert!(
            matches!(cache.state, CaptureState::Failed { .. }),
            "a fresh failure must stay in backoff: {:?}",
            cache.state
        );

        cache.state = CaptureState::Resolved {
            path: "/a".to_owned(),
        };
        arm_capture_if_due(&mut cache);
        assert!(
            matches!(cache.state, CaptureState::Resolved { .. }),
            "a resolved PATH must not be re-captured"
        );

        cache.state = CaptureState::Capturing {
            generation: 0,
            prior_failures: 0,
            cancel: no_cancel(),
        };
        arm_capture_if_due(&mut cache);
        assert!(
            matches!(cache.state, CaptureState::Capturing { generation: 0, .. }),
            "a running capture must not be joined by a second"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_explicit_recheck_clears_the_retry_history_from_either_state() {
        // A Recheck is the user saying they changed something. The escalating
        // backoff must not survive it — otherwise clicking Recheck during an
        // automatic retry leaves the *next* automatic attempt on the escalated
        // cadence, which is the opposite of what the click asked for.
        let _serial = serialized_path_cache_test();

        // From a settled failure.
        {
            let mut cache = lock_path_cache();
            cache.state = CaptureState::Failed {
                at: std::time::Instant::now(),
                count: 4,
            };
        }
        invalidate_path_cache();
        assert!(
            matches!(
                lock_path_cache().state,
                CaptureState::Capturing {
                    prior_failures: 0,
                    ..
                }
            ),
            "history must clear from Failed: {:?}",
            lock_path_cache().state
        );

        // And from mid-retry, where the superseded capture's guard is what
        // restarts — reading `prior_failures` straight back out of this state.
        {
            let mut cache = lock_path_cache();
            let generation = cache.generation;
            cache.state = CaptureState::Capturing {
                generation,
                prior_failures: 4,
                cancel: no_cancel(),
            };
        }
        invalidate_path_cache();
        assert!(
            matches!(
                lock_path_cache().state,
                CaptureState::Capturing {
                    prior_failures: 0,
                    ..
                }
            ),
            "history must clear from Capturing too: {:?}",
            lock_path_cache().state
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_capture_that_ends_without_publishing_leaves_the_cache_re_armable() {
        // A panic before `publish_capture` must not strand the state in
        // `Capturing`: that would mean no capture ever starts again and Recheck
        // silently does nothing — the same "stuck until relaunch" shape as the
        // original bug. Equally it must not respawn from the guard, which would
        // loop straight back into the same panic.
        let _serial = serialized_path_cache_test();
        let generation = {
            let mut cache = lock_path_cache();
            cache.generation = cache.generation.wrapping_add(1);
            let generation = cache.generation;
            transition(
                &mut cache,
                CaptureState::Capturing {
                    generation,
                    prior_failures: 0,
                    cancel: no_cancel(),
                },
            );
            generation
        };

        drop(CaptureGuard { generation });

        assert!(
            matches!(lock_path_cache().state, CaptureState::Idle),
            "expected Idle so the next reader re-arms"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn version_sort_key_orders_numerically_not_lexicographically() {
        // `v9` must sort below `v22` — the whole reason this isn't a string sort.
        assert!(version_sort_key("v22.22.1") > version_sort_key("v9.99.99"));
        assert!(version_sort_key("v22.22.1") > version_sort_key("v22.9.0"));
        assert_eq!(version_sort_key("22.22.1"), version_sort_key("v22.22.1"));
        // Unparseable names collapse to zeros and sort low rather than panicking
        // or being dropped from the candidate list.
        assert_eq!(version_sort_key("not-a-version"), [0, 0, 0]);
        assert!(version_sort_key("v1.0.0") > version_sort_key("not-a-version"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn merge_path_appends_extras_after_the_process_path_and_dedupes() {
        // Fixed inputs, not the ambient environment: the ordering contract is
        // that the user's own PATH keeps its precedence and the extras only
        // widen the search. Appending — never prepending — is what makes this
        // incapable of shadowing a binary the user deliberately put first.
        let merged = merge_path(
            "/usr/bin:/bin",
            &[
                PathBuf::from("/opt/homebrew/bin"),
                // Already present: must not be duplicated or moved.
                PathBuf::from("/usr/bin"),
            ],
        );
        assert_eq!(merged, "/usr/bin:/bin:/opt/homebrew/bin");

        // Empty segments (a trailing or doubled colon) are dropped rather than
        // becoming an implicit "current directory" entry.
        assert_eq!(merge_path("/usr/bin::/bin:", &[]), "/usr/bin:/bin");
        // Duplicates within the process PATH itself collapse too.
        assert_eq!(merge_path("/bin:/usr/bin:/bin", &[]), "/bin:/usr/bin");
        assert_eq!(
            merge_path("", &[PathBuf::from("/opt/homebrew/bin")]),
            "/opt/homebrew/bin"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn node_version_manager_bin_dirs_finds_both_layouts_newest_first() {
        // The `<version>/bin` (nvm) vs `<version>/installation/bin` (fnm) split
        // is exactly the kind of path shape that is silently wrong if inferred
        // rather than checked, and the newest-first cap is what bounds which
        // node an agent's `env node` shebang resolves to.
        let home = tempfile::tempdir().expect("tempdir");
        let nvm = home.path().join(".nvm/versions/node");
        for version in ["v9.99.99", "v20.1.0", "v22.22.1"] {
            std::fs::create_dir_all(nvm.join(version).join("bin")).expect("seed nvm");
        }
        let fnm = home
            .path()
            .join("Library/Application Support/fnm/node-versions");
        std::fs::create_dir_all(fnm.join("v18.0.0").join("installation/bin")).expect("seed fnm");

        let dirs = node_version_manager_bin_dirs(home.path());

        // Newest first, capped — and v9 must not outrank v22 or v20.
        assert_eq!(dirs[0], nvm.join("v22.22.1/bin"));
        assert_eq!(dirs[2], nvm.join("v20.1.0/bin"));
        assert!(
            !dirs.iter().any(|dir| dir.starts_with(nvm.join("v9.99.99"))),
            "the cap must drop the oldest version, not the newest: {dirs:?}"
        );
        assert!(
            dirs.contains(&fnm.join("v18.0.0/installation/bin")),
            "fnm's installation/bin layout must be found: {dirs:?}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scan_drops_candidates_that_do_not_exist() {
        // Injected roots, not the ambient machine: the previous version asserted
        // "at least one candidate exists here", which says nothing about the
        // filter and passes vacuously on a container with none of them.
        //
        // The contract is that a nonexistent candidate never reaches the PATH. A
        // bogus entry would be harmless to `which` but would make the logged
        // fallback PATH misleading exactly when someone is diagnosing a report.
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(home.path().join(".local/bin")).expect("seed");

        let dirs = home_bin_dirs(home.path());

        assert!(
            dirs.contains(&home.path().join(".local/bin")),
            "an existing candidate must be kept: {dirs:?}"
        );
        assert!(
            !dirs.iter().any(|dir| dir.ends_with(".cargo/bin")),
            "a candidate that does not exist must be dropped: {dirs:?}"
        );
        assert!(dirs.iter().all(|dir| dir.is_dir()));
    }

    #[cfg(unix)]
    #[test]
    fn run_bounded_kills_the_process_group_when_the_command_overruns() {
        // The leak this replaced: abandoning a thread left the shell alive, and
        // every retry stranded another one for the life of the app.
        // A marker unique to this test, so the `pgrep` below can't match a
        // `sleep` belonging to another test or to the developer's shell.
        const MARKER: &str = "switchboard_run_bounded_pgroup_probe";
        let mut command = std::process::Command::new("/bin/sh");
        // The inner `sh` is a *grandchild* — killing only the direct child would
        // leave it running, which is why this signals the whole group.
        command.args([
            "-c",
            &format!("sh -c 'sleep 60 # {MARKER}' & sleep 60 # {MARKER}"),
        ]);

        let started = std::time::Instant::now();
        let result = run_bounded(command, Duration::from_millis(300));

        assert!(
            result.is_none(),
            "an overrunning command must not return output"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "run_bounded must return at its deadline, not wait out the command"
        );
        // The group is signalled before returning; nothing from it survives to
        // hold our pipes open, which is what the reader-thread join relies on.
        // `pgrep` invoked directly, not via `sh -c`: a shell wrapper's own
        // command line would contain the marker and match itself. `pgrep` never
        // matches its own process, and exits non-zero when nothing matches.
        let survivors = std::process::Command::new("pgrep")
            .args(["-f", MARKER])
            .output()
            .expect("pgrep should run");
        assert!(
            !survivors.status.success(),
            "the spawned process group should be gone, found: {}",
            String::from_utf8_lossy(&survivors.stdout)
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_bounded_returns_stdout_larger_than_the_pipe_buffer() {
        // Regression guard for the drain: without a concurrent reader, a command
        // whose output exceeds the pipe buffer blocks on write, never exits, and
        // gets killed at the deadline — silently converting a working login-shell
        // capture into a failure on any machine with a chatty shell profile.
        let mut command = std::process::Command::new("/bin/sh");
        command.args([
            "-c",
            "for i in $(seq 1 40000); do echo aaaaaaaaaaaaaaaaaaaa; done; echo TAIL_MARKER",
        ]);

        let output = run_bounded(command, Duration::from_secs(30)).expect("should complete");

        assert_eq!(output.success, Some(true));
        // Bounded, not unbounded: the whole point is that a chatty profile can't
        // grow the buffer without limit.
        assert!(
            output.stdout.len() <= MAX_CAPTURED_OUTPUT,
            "retained {} bytes, cap is {MAX_CAPTURED_OUTPUT}",
            output.stdout.len()
        );
        // And it retains the *tail*: a login shell prints the value we want last,
        // so capping from the front would discard exactly the answer.
        assert!(
            output.stdout.trim_end().ends_with("TAIL_MARKER"),
            "expected the tail to survive truncation, got: {:?}",
            &output.stdout[output.stdout.len().saturating_sub(80)..]
        );
    }

    #[cfg(unix)]
    #[test]
    fn stdout_reader_terminates_when_a_descendant_writes_past_the_deadline() {
        // The deadline used to be consulted only when a read would block, so a
        // descendant that keeps writing kept the reader in the success arm and
        // the thread outlived `run_bounded` forever. `fetch_version` runs this
        // four times per install refresh, so anything leaving a chatty process
        // behind accumulated threads for the life of the app.
        let marker = "switchboard_reader_deadline_probe";
        let mut command = std::process::Command::new("/bin/sh");
        // The direct child exits immediately; the grandchild inherits stdout and
        // keeps writing, so EOF never arrives and reads never block.
        command.args([
            "-c",
            &format!("sh -c 'while :; do echo {marker}; done' & exit 0"),
        ]);

        let started = std::time::Instant::now();
        let output = run_bounded(command, Duration::from_millis(200));
        // The direct child exited cleanly, so this is the success path.
        assert!(output.is_some());

        // The reader's own deadline is the command timeout plus the drain grace;
        // it must expire rather than run forever.
        let reader_budget = Duration::from_millis(200) + READ_DRAIN_GRACE + Duration::from_secs(5);
        let mut gone = false;
        while started.elapsed() < reader_budget {
            let survivors = std::process::Command::new("pgrep")
                .args(["-f", marker])
                .output()
                .expect("pgrep should run");
            if !survivors.status.success() {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Tidy up regardless, so a failure here can't leak into other tests.
        let _ = std::process::Command::new("pkill")
            .args(["-f", marker])
            .status();
        // The reader exiting drops the read end; the writer then takes SIGPIPE.
        // Its disappearance is the observable proof the thread stopped.
        assert!(gone, "the reader thread outlived its deadline");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreapable_child_reads_as_finished_not_as_still_running() {
        use std::os::unix::process::ExitStatusExt as _;

        // The reported failure: inside the app the PATH capture failed while the
        // identical call succeeded from a shell-launched binary. A GUI host sets
        // `SIGCHLD` to `SIG_IGN` (or installs a handler), the kernel auto-reaps,
        // and `waitpid` then returns `ECHILD` for a command that ran perfectly.
        // Reading that as "still running" meant the poll loop ran to its deadline,
        // killed the group, and discarded the output — recording a capture
        // failure for a shell that had printed a perfectly good PATH.
        let echild = std::io::Error::from_raw_os_error(10); // ECHILD
        assert_eq!(
            interpret_wait(Err(echild)),
            WaitOutcome::Finished(None),
            "an unreapable child is finished with an unknown verdict, never still-running"
        );

        // The ordinary outcomes are unchanged.
        assert_eq!(interpret_wait(Ok(None)), WaitOutcome::Running);
        assert_eq!(
            interpret_wait(Ok(Some(std::process::ExitStatus::from_raw(0)))),
            WaitOutcome::Finished(Some(true))
        );
        assert_eq!(
            interpret_wait(Ok(Some(std::process::ExitStatus::from_raw(1 << 8)))),
            WaitOutcome::Finished(Some(false))
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unknown_verdict_still_yields_a_version() {
        // `fetch_version` gates on the exit status, so an unknown verdict must
        // not be treated as failure — under an auto-reaping host every version
        // would silently disappear from the CLI list.
        let output = BoundedOutput {
            stdout: "2.1.220 (Claude Code)".to_owned(),
            success: None,
        };
        assert!(output.success != Some(false));
        assert_eq!(
            parse_cli_version(&output.stdout).as_deref(),
            Some("2.1.220"),
            "the parse is the real gate when the verdict is unknown"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_bounded_abandons_a_command_when_cancelled() {
        // What makes a Recheck landing mid-capture cost one capture instead of
        // two: the superseded attempt is abandoned rather than waited out, so the
        // replacement starts immediately. Without it the user pays the doomed
        // attempt's full remaining time first — up to the whole timeout, which is
        // exactly the case they pressed the button in.
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        });

        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "sleep 60"]);
        let started = std::time::Instant::now();

        let result = run_bounded_cancellable(command, Duration::from_mins(1), &cancel);

        assert!(
            matches!(result, Err(BoundedFailure::Cancelled)),
            "a cancelled command must say it was cancelled, not merely fail: {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancellation must not wait out the timeout, took {:?}",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_bounded_returns_output_from_a_command_that_exits_non_zero() {
        // The regression that gated the PATH capture on exit status: a shell
        // whose profile ends in a failing hook still printed a perfectly good
        // PATH, and discarding it falls back and starts a retry backoff for
        // nothing. Status is reported, not used to suppress output.
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "printf hello; exit 3"]);

        let output = run_bounded(command, Duration::from_secs(10)).expect("should complete");

        assert_eq!(output.stdout, "hello");
        assert_eq!(output.success, Some(false));
    }

    #[test]
    fn resolve_binary_returns_absolute_path_verbatim() {
        // Absolute path → trusted as-is, no PATH lookup. Even nonexistent
        // absolute paths return Ok; spawn at the call site is what fails
        // with NotFound, mapped to BinaryNotFound by the adapter.
        let path = std::path::Path::new("/nonexistent/absolute/binary");
        assert_eq!(resolve_binary(path).unwrap(), path.to_path_buf());
    }

    #[test]
    fn resolve_binary_relative_name_not_on_path_returns_binary_not_found() {
        // Relative names resolve against the shared PATH cache, and reading it
        // arms a capture — serialized so the arm can't race a cache-state test.
        #[cfg(target_os = "macos")]
        let _serial = serialized_path_cache_test();
        let path = std::path::Path::new("definitely-not-a-real-binary-name-xyz123");
        assert!(matches!(
            resolve_binary(path),
            Err(DispatchError::BinaryNotFound)
        ));
    }

    #[test]
    fn probe_binary_is_stricter_than_resolve_for_absolute_paths() {
        // Intentional divergence: `resolve_binary` trusts an absolute path
        // verbatim (failure deferred to spawn), but `probe_binary` actually
        // checks existence + exec bit, so a missing absolute path fails at
        // probe time. This test locks that contract so a future refactor can't
        // silently collapse the two.
        let path = std::path::Path::new("/nonexistent/absolute/binary");
        assert!(resolve_binary(path).is_ok());
        assert!(matches!(
            probe_binary(path),
            Err(DispatchError::BinaryNotFound)
        ));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn apply_path_env_sets_resolved_path_on_child() {
        // The behavior the fix exists for: a spawned harness subprocess must
        // see the resolved (login-shell) PATH, not the parent's minimal GUI
        // PATH. Run a child that prints its inherited PATH and assert it equals
        // what we resolved.
        //
        // Settle the capture first, and snapshot the expected value while
        // serialized: `resolved_path` serves the fallback while a capture is in
        // flight, so reads either side of the publish would disagree. The guard
        // is released before the await — it only needs to cover the snapshot.
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "printf %s \"$PATH\""]);
        // Both reads of the resolved PATH happen inside the serialized window —
        // `apply_path_env` is synchronous, so the command's environment is fixed
        // before the guard drops. Reading either one outside it lets a
        // concurrent invalidation land between them and they disagree.
        #[cfg(target_os = "macos")]
        let _serial = serialized_path_cache_test_async().await;
        #[cfg(target_os = "macos")]
        let _ = await_capture(PATH_CAPTURE_TIMEOUT);
        apply_path_env(&mut command);
        let expected = resolved_path();
        let output = command.output().await.expect("sh should run");
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
    }

    /// Serializes the tests that drive the real global PATH cache. They share
    /// one process-wide static, and an invalidation from one test supersedes a
    /// capture another is waiting on — so running them concurrently tests the
    /// harness, not the code.
    ///
    /// A `tokio` mutex rather than a `std` one because the async cases must hold
    /// exclusion *across* an await (they invalidate, then wait for the capture);
    /// a `std` guard held over an await is both a lint error and a genuine
    /// deadlock risk. Sync cases take it with `blocking_lock`, which is valid
    /// because a plain `#[test]` is not inside a runtime.
    #[cfg(target_os = "macos")]
    static PATH_CACHE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[cfg(target_os = "macos")]
    fn serialized_path_cache_test() -> tokio::sync::MutexGuard<'static, ()> {
        PATH_CACHE_TEST_LOCK.blocking_lock()
    }

    /// Whether the developer's real login shell can run the capture from a
    /// terminal-launched test process. Shells without `+m` support (fish,
    /// nushell) suspend in the capture's background process group when `cargo
    /// test` has a controlling terminal — by design the capture then degrades
    /// to the fallback (see [`job_control_off_flag`]), so tests asserting a
    /// *successful* real capture would fail on such a machine for a documented,
    /// intended reason. Those tests bail out early instead; capture success
    /// itself stays pinned hermetically (pinned zsh) by
    /// `tests/path_capture_pty.rs`.
    #[cfg(target_os = "macos")]
    fn dev_shell_supports_capture() -> bool {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
        if job_control_off_flag(&shell).is_some() {
            return true;
        }
        eprintln!("skipping real-capture assertion: $SHELL ({shell}) has no job-control-off flag");
        false
    }

    #[cfg(target_os = "macos")]
    async fn serialized_path_cache_test_async() -> tokio::sync::MutexGuard<'static, ()> {
        PATH_CACHE_TEST_LOCK.lock().await
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn await_capture_resolves_to_the_login_shell_path() {
        let _serial = serialized_path_cache_test();
        if !dev_shell_supports_capture() {
            return;
        }
        // End-to-end through the real capture: the whole point of the module is
        // that a GUI-launched app ends up with the terminal's PATH, and
        // `await_capture` is the contract Recheck and dispatch rely on to get a
        // final answer rather than a provisional one.
        assert_eq!(await_capture(PATH_CAPTURE_TIMEOUT), PathSource::LoginShell);
        assert_ne!(path_source(), PathSource::Capturing);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn ensure_path_settled_waits_for_an_in_flight_capture() {
        if !dev_shell_supports_capture() {
            return;
        }
        // The async counterpart to `await_capture`, and the one turn dispatch
        // uses. It must not report `Capturing` while a capture is running —
        // that is the caller's signal to proceed on the fallback PATH, and an
        // agent spawned that way runs its whole turn against a guessed PATH
        // with no corrective re-probe.
        let _serial = serialized_path_cache_test_async().await;
        invalidate_path_cache();
        assert!(
            matches!(lock_path_cache().state, CaptureState::Capturing { .. }),
            "precondition: a capture should be in flight"
        );

        let settled = ensure_path_settled(PATH_CAPTURE_TIMEOUT).await;

        assert_eq!(settled, PathSource::LoginShell);
        assert!(!matches!(
            lock_path_cache().state,
            CaptureState::Capturing { .. }
        ));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn ensure_path_settled_returns_immediately_when_already_resolved() {
        if !dev_shell_supports_capture() {
            return;
        }
        // The common case by far: captures settle in under a second and every
        // later dispatch must pay nothing. A version that always waited would
        // add latency to every send.
        let _serial = serialized_path_cache_test_async().await;
        assert_eq!(
            ensure_path_settled(PATH_CAPTURE_TIMEOUT).await,
            PathSource::LoginShell
        );

        let started = std::time::Instant::now();
        let settled = ensure_path_settled(PATH_CAPTURE_TIMEOUT).await;

        assert_eq!(settled, PathSource::LoginShell);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "an already-resolved PATH must not wait, took {:?}",
            started.elapsed()
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn ensure_path_settled_reports_capturing_when_the_budget_expires() {
        // Bounded, not indefinite: the caller (dispatch, auto-create) must get
        // control back and decide what to do, rather than the send path hanging
        // on a wedged shell. `Capturing` is how "still pending" is reported.
        let _serial = serialized_path_cache_test_async().await;
        invalidate_path_cache();

        let settled = ensure_path_settled(Duration::from_millis(1)).await;

        assert_eq!(settled, PathSource::Capturing);
        // And the capture is still running — the timeout abandons the wait, not
        // the work.
        assert!(matches!(
            lock_path_cache().state,
            CaptureState::Capturing { .. }
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn invalidate_path_cache_forces_a_fresh_capture() {
        if !dev_shell_supports_capture() {
            return;
        }
        // The contract the Recheck button rests on. Previously this was only
        // verified against a frontend mock — nothing proved the backend honored
        // it.
        let _serial = serialized_path_cache_test();
        assert_eq!(await_capture(PATH_CAPTURE_TIMEOUT), PathSource::LoginShell);

        invalidate_path_cache();

        // A fresh capture is underway (or already done — it can be fast), but
        // either way the previous value is gone and a new attempt was started
        // rather than the backoff suppressing it.
        assert_eq!(await_capture(PATH_CAPTURE_TIMEOUT), PathSource::LoginShell);
    }

    #[test]
    fn fetch_version_returns_first_line_for_present_binary() {
        // Resolving `cargo` by name reads the shared PATH cache (see
        // `resolve_binary_relative_name_...` for why that needs serializing).
        #[cfg(target_os = "macos")]
        let _serial = serialized_path_cache_test();
        // `cargo` is guaranteed present wherever `cargo test` runs and
        // supports `--version`; it stands in for a harness CLI to prove the
        // first-line extraction without depending on a real harness install.
        let version = fetch_version(std::path::Path::new("cargo"))
            .expect("cargo --version should report a line");
        assert!(
            version.contains("cargo"),
            "unexpected version line: {version}"
        );
        assert!(!version.contains('\n'), "should be a single trimmed line");
    }

    #[test]
    fn fetch_version_none_for_missing_binary() {
        #[cfg(target_os = "macos")]
        let _serial = serialized_path_cache_test();
        assert_eq!(
            fetch_version(std::path::Path::new("definitely-not-a-real-binary-xyz123")),
            None
        );
    }

    #[test]
    fn parse_cli_version_extracts_number_from_real_formats() {
        // Captured live: each harness pads its --version differently.
        assert_eq!(
            parse_cli_version("2.1.156 (Claude Code)").as_deref(),
            Some("2.1.156")
        );
        assert_eq!(
            parse_cli_version("codex-cli 0.134.0").as_deref(),
            Some("0.134.0")
        );
        assert_eq!(parse_cli_version("0.44.0").as_deref(), Some("0.44.0"));
        assert_eq!(parse_cli_version("1.0.3").as_deref(), Some("1.0.3"));
    }

    #[test]
    fn parse_cli_version_strips_leading_v_and_handles_no_version() {
        assert_eq!(parse_cli_version("v1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(parse_cli_version(""), None);
        assert_eq!(parse_cli_version("no version here"), None);
    }
}
