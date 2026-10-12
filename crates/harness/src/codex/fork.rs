//! Create a Codex fork **without running a turn**.
//!
//! `codex exec fork <parent>` with no prompt creates a new session that
//! inherits the parent's conversation, prints one `thread.started` event with
//! the new id, and exits. No model call, no turn. The fork's rollout stores a
//! pointer into the parent's rollout (`session_meta.history_base`) instead of
//! copying it. `docs/harness-behavior.md` §3.5 has the probes.
//!
//! **Why this call is the one that needs the parent idle.** Creation is where
//! Codex picks the fork's cut-off in the parent's rollout
//! (`history_base.end_byte_offset`). Every later fork turn re-reads the parent
//! up to that cut-off, but rollouts are append-only, so those bytes never
//! change. Only choosing the cut-off needs the parent to be quiet, so a caller
//! holds the parent's session lock for exactly this call and no longer. That is
//! what lets a Codex fork leave its parent free during the fork's first turn,
//! where a Claude fork (which can only come into existence *as* a turn) keeps
//! the parent locked for that whole turn.
//!
//! **Why it is a free function and not a `HarnessAdapter` method.** Same
//! boundary as [`super::account_usage`]: the trait is per-turn, and this is not
//! a turn. It holds no turn slot in the dispatcher, streams no content, writes
//! no journal entry and produces no transcript.
//!
//! **A fork whose parent file later disappears cannot run.** Probed: Codex
//! refuses to resume it (`missing source rollout`) until the parent's file is
//! back. Nothing here can prevent that. It is the transcript loader's job to
//! explain it.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use switchboard_core::AgentSelection;
use tokio::io::AsyncBufReadExt;
use tokio_util::sync::CancellationToken;

use super::CodexLocator;
use super::session_file::{AttachLookupError, find_codex_session_file_for_attach};

/// Production bound for creating a fork.
///
/// **Sized from a measured distribution.** 12 promptless forks of a small
/// session on codex-cli 0.162.1 (2026-10-11): min 0.53s, median 0.61s, max
/// 0.96s. Fifteen seconds is about fifteen times the slowest observed run, which
/// absorbs a cold start or a loaded machine.
///
/// **Not looser than that, because the parent is locked while this runs.** A
/// hung Codex holds the parent's session lock until this bound expires, and
/// every turn sent to the parent in that time is refused. A generous bound
/// costs nothing on the normal path and bounds that cost on the broken one.
pub const CODEX_FORK_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to wait for the stderr drain once the process has finished.
/// Reached only to make an error message complete.
const STDERR_SETTLE: Duration = Duration::from_millis(500);

/// Why a fork was not created.
///
/// Every variant means Switchboard registers nothing. Some of them can still
/// leave a rollout Codex wrote (a non-zero exit after writing, a timeout after
/// writing, an unreadable `thread.started`). That stray session is harmless:
/// nothing in Switchboard refers to it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CodexForkError {
    #[error("Codex is not installed or not on PATH")]
    BinaryNotFound,
    /// The OS reports a missing working directory and a missing binary the
    /// same way at spawn. Told apart because the repairs differ.
    #[error("the project's folder no longer exists: {}", .0.display())]
    WorkingDirectoryMissing(PathBuf),
    #[error("couldn't start Codex to create the fork: {0}")]
    Spawn(std::io::Error),
    /// The child is killed, with its whole process group, before this is
    /// returned.
    #[error("Codex didn't finish creating the fork within {bound:?}{}", stderr_suffix(.stderr))]
    Timeout { bound: Duration, stderr: String },
    #[error("Codex couldn't create the fork ({}){}", exit_description(*.code), stderr_suffix(.stderr))]
    Exit {
        /// `None` when the process was ended by a signal.
        code: Option<i32>,
        stderr: String,
    },
    /// Codex exited 0 without printing a `thread.started` that names the new
    /// session, so there is no id to register.
    #[error("Codex finished without reporting the new session's id{}", stderr_suffix(.stderr))]
    NoThreadStarted { stderr: String },
    /// Codex reported an id, but no rollout with that id exists. Checked so
    /// Switchboard never registers a fork whose session Codex did not write.
    #[error("Codex reported fork {thread_id}, but its session file isn't in ~/.codex/sessions")]
    RolloutNotFound { thread_id: String },
    /// More than one rollout carries the new id. Impossible by Codex's design;
    /// surfaced rather than guessed at, as the attach flow does.
    #[error("Codex reported fork {thread_id}, and more than one session file carries that id")]
    RolloutAmbiguous { thread_id: String },
    /// The caller dropped the future before the fork finished. Nobody receives
    /// this; it exists so the teardown path has something truthful to return.
    #[error("the fork was abandoned before Codex finished")]
    Abandoned,
    /// A failure on Switchboard's side rather than Codex's: the task running
    /// the fork or the rollout lookup panicked, or the OS couldn't report
    /// Codex's exit status.
    #[error("Switchboard failed while creating the fork: {0}")]
    Internal(String),
}

fn stderr_suffix(tail: &str) -> String {
    if tail.is_empty() {
        String::new()
    } else {
        format!(": {tail}")
    }
}

fn exit_description(code: Option<i32>) -> String {
    code.map_or_else(
        || "ended by a signal".to_owned(),
        |code| format!("exit code {code}"),
    )
}

/// Create a fork of `parent_thread_id` with no turn, and return the new
/// session's locator.
///
/// The caller holds the parent's session lock for exactly this call: it is
/// where Codex picks the fork's cut-off in the parent's rollout, and the module
/// doc explains why nothing after it needs the parent idle.
///
/// `selection` is passed as the same `-m` / `-c model_reasoning_effort=` flags a
/// dispatch sends, so the fork's recorded settings match the agent the caller
/// registers. `home_dir` is where the new rollout is looked up
/// (`<home_dir>/.codex/sessions`).
///
/// **The partition date comes from where Codex wrote the file**, not from
/// today's date. A creation that crossed midnight would otherwise look in the
/// wrong folder and report a fork Codex did write as missing. A new fork's
/// filename always has the single-id form (`rollout-<time>-<id>.jsonl`): only a
/// thread rolled back with Codex's "revert" carries two ids, and a fork is a new
/// thread whose thread id is also its rollout id.
///
/// **Cancellation-safe, the same way as [`super::account_usage::read_account_usage`].**
/// The work runs on a task that owns the child. Dropping this future signals the
/// task, which kills the process group and returns. `kill_on_drop` alone would
/// reach only the direct child.
///
/// `timeout` bounds binary resolution, spawn, and the wait for Codex to exit.
/// It does not bound teardown (`terminate_then_kill` carries its own grace) or
/// the final rollout lookup (a directory scan). Production passes
/// [`CODEX_FORK_TIMEOUT`]; a parameter so the kill-on-timeout path is testable
/// without a fifteen-second test.
///
/// # Errors
///
/// Returns [`CodexForkError`] for a missing binary or working directory, a
/// failed spawn, a timeout, a non-zero exit, a missing `thread.started`, or a
/// reported id with no (or more than one) rollout on disk.
pub async fn fork_codex_session(
    binary: &Path,
    home_dir: &Path,
    cwd: &Path,
    parent_thread_id: &str,
    selection: &AgentSelection,
    timeout: Duration,
) -> Result<CodexLocator, CodexForkError> {
    let binary = binary.to_owned();
    let cwd = cwd.to_owned();
    let args = fork_args(parent_thread_id, selection);
    let cancel = CancellationToken::new();
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move { fork_on_task(&binary, &cwd, args, timeout, &cancel).await }
    });
    let _guard = cancel.drop_guard();
    let thread_id = task.await.map_err(|e| {
        tracing::error!(error = %e, "codex fork task did not complete");
        CodexForkError::Internal(e.to_string())
    })??;
    // A scan of every date folder, so it runs off the async workers.
    let lookup = tokio::task::spawn_blocking({
        let home_dir = home_dir.to_owned();
        let thread_id = thread_id.clone();
        move || find_codex_session_file_for_attach(&home_dir, &thread_id)
    })
    .await
    .map_err(|e| CodexForkError::Internal(e.to_string()))?;
    match lookup {
        Ok((_, partition_date)) => Ok(CodexLocator {
            thread_id,
            partition_date,
        }),
        Err(AttachLookupError::NotFound { .. }) => {
            Err(CodexForkError::RolloutNotFound { thread_id })
        }
        Err(AttachLookupError::Ambiguous { .. }) => {
            Err(CodexForkError::RolloutAmbiguous { thread_id })
        }
    }
}

/// The probed command shape: `exec` flags, then `fork <parent>`, with no
/// prompt. The flags go **before** `fork`, as probed. No `--` separator: the
/// only positional is a Codex thread id, which never begins with `-`.
///
/// `-C .` with the child's working directory set to the project folder, as a
/// first-turn dispatch does: `.` resolves against the child's cwd, so the path
/// is not encoded twice.
fn fork_args(parent_thread_id: &str, selection: &AgentSelection) -> Vec<String> {
    let mut args = vec![
        "exec".to_owned(),
        "--json".to_owned(),
        "--skip-git-repo-check".to_owned(),
        "--dangerously-bypass-approvals-and-sandbox".to_owned(),
        "-C".to_owned(),
        ".".to_owned(),
    ];
    super::push_selection_flags(
        &mut args,
        selection.model.as_deref(),
        selection.effort.as_deref(),
    );
    args.push("fork".to_owned());
    args.push(parent_thread_id.to_owned());
    args
}

/// The body of [`fork_codex_session`], running where a dropped caller cannot
/// interrupt it. Returns the new session's thread id.
async fn fork_on_task(
    binary: &Path,
    cwd: &Path,
    args: Vec<String>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<String, CodexForkError> {
    let deadline = tokio::time::Instant::now() + timeout;

    let resolved =
        crate::subprocess::resolve_binary(binary).map_err(|_| CodexForkError::BinaryNotFound)?;

    let mut command = tokio::process::Command::new(&resolved);
    command
        .args(&args)
        .current_dir(cwd)
        // Closed, as probed: with no prompt argument Codex creates the fork
        // and exits, and an open stdin is never read.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::subprocess::apply_path_env(&mut command);
    // Own process group, like a dispatched turn, so teardown reaches every
    // process Codex starts, not just the one spawned here.
    #[cfg(unix)]
    command.process_group(0);

    let mut child =
        command
            .spawn()
            .map_err(|e| match crate::subprocess::map_spawn_error(e, cwd) {
                crate::DispatchError::BinaryNotFound => CodexForkError::BinaryNotFound,
                crate::DispatchError::WorkingDirectoryMissing(path) => {
                    CodexForkError::WorkingDirectoryMissing(path)
                }
                crate::DispatchError::SpawnFailed(e) => CodexForkError::Spawn(e),
                other => CodexForkError::Spawn(std::io::Error::other(other.to_string())),
            })?;
    // Captured now because reaping the leader clears it, and the sweep after a
    // normal exit still needs the group's id.
    let leader = child.id();

    let tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
    let draining = child.stderr.take().map(|stderr| {
        let tail = Arc::clone(&tail);
        tokio::spawn(async move {
            if let Err(e) = crate::subprocess::drain_stderr_into_tail(stderr, tail, |line| {
                tracing::debug!("codex exec fork stderr: {line}");
            })
            .await
            {
                tracing::warn!(error = %e, "codex exec fork stderr read error");
            }
        })
    });
    let Some(stdout) = child.stdout.take() else {
        crate::subprocess::terminate_then_kill(&mut child).await;
        return Err(CodexForkError::Spawn(std::io::Error::other(
            "the child exposed no stdout",
        )));
    };

    let finished = tokio::select! {
        finished = tokio::time::timeout_at(deadline, run_to_exit(&mut child, stdout)) => Some(finished),
        () = cancel.cancelled() => None,
    };
    let Some(finished) = finished else {
        crate::subprocess::terminate_then_kill(&mut child).await;
        // Nobody reads the stderr tail of an abandoned fork, so stop the reader
        // rather than leave it detached for as long as a process outside the
        // group holds the pipe.
        if let Some(draining) = draining {
            draining.abort();
        }
        return Err(CodexForkError::Abandoned);
    };

    let Ok((thread_id, status)) = finished else {
        // Kill before settling the drain: a hung child holds its stderr open,
        // so the drain could never finish while it lives (the ordering
        // `account_usage::read_on_task` documents).
        crate::subprocess::terminate_then_kill(&mut child).await;
        let stderr = settled_stderr(draining, &tail).await;
        return Err(CodexForkError::Timeout {
            bound: timeout,
            stderr,
        });
    };
    // **Codex has exited and been reaped; kill anything it left in its process
    // group**, on success and failure alike. No Codex helper was seen
    // surviving a promptless fork, but the fork has nothing else that would
    // ever stop one, and the group is ours. This is stricter than ordinary
    // turns: `run_producer` kills the group only on cancel or a forced kill,
    // so a turn that exits normally leaves any such helper running. Changing
    // that touches every Codex turn and is a separate decision.
    if let Some(leader) = leader {
        crate::subprocess::kill_process_group(leader);
    }
    let stderr = settled_stderr(draining, &tail).await;
    // A failed `wait` is the OS refusing to report the exit, not Codex
    // failing, so it is not reported as an exit status.
    let status = status
        .map_err(|e| CodexForkError::Internal(format!("couldn't read Codex's exit status: {e}")))?;
    if !status.success() {
        return Err(CodexForkError::Exit {
            code: status.code(),
            stderr,
        });
    }
    thread_id.ok_or(CodexForkError::NoThreadStarted { stderr })
}

/// Read stdout to EOF, keeping the first `thread.started` id, then wait for the
/// process to exit.
///
/// Byte-oriented, then lossy, for the reason `account_usage::read_response`
/// gives: one invalid byte must not discard an id that arrives on a later line.
/// A read error ends the read like EOF does; the exit status then decides.
async fn run_to_exit(
    child: &mut tokio::process::Child,
    stdout: tokio::process::ChildStdout,
) -> (Option<String>, std::io::Result<std::process::ExitStatus>) {
    let mut thread_id = None;
    let mut chunks = tokio::io::BufReader::new(stdout).split(b'\n');
    while let Ok(Some(chunk)) = chunks.next_segment().await {
        if thread_id.is_none() {
            thread_id = thread_started_in(&String::from_utf8_lossy(&chunk));
        }
    }
    (thread_id, child.wait().await)
}

/// The new session's id, if `line` is a `thread.started` event naming one.
///
/// `None` for anything else: a line that is not JSON, another event type, or a
/// `thread.started` whose id is missing or not a string. The id field is read by
/// the stream parser's own helper, so a fork and a dispatch agree on it.
fn thread_started_in(line: &str) -> Option<String> {
    let event: Value = serde_json::from_str(line).ok()?;
    if event.get("type").and_then(Value::as_str) != Some("thread.started") {
        return None;
    }
    let id = super::parser::thread_started_id(&event);
    if id.is_none() {
        tracing::warn!("codex exec fork printed thread.started without a string thread_id");
    }
    id.map(str::to_owned)
}

/// The stderr tail, once the drain has had a moment to finish.
///
/// **The drain never outlives this call.** Callers kill the process group
/// first, so the pipe normally closes at once. If it is still open after
/// [`STDERR_SETTLE`], a process outside the group holds it, and the reader is
/// aborted rather than left running detached for as long as that process
/// lives.
async fn settled_stderr(
    draining: Option<tokio::task::JoinHandle<()>>,
    tail: &Mutex<VecDeque<String>>,
) -> String {
    if let Some(mut draining) = draining
        && tokio::time::timeout(STDERR_SETTLE, &mut draining)
            .await
            .is_err()
    {
        draining.abort();
        let _ = draining.await;
    }
    crate::subprocess::format_stderr_tail(tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_started_yields_its_id() {
        assert_eq!(
            thread_started_in(r#"{"type":"thread.started","thread_id":"01a12d13-2870-7f61"}"#),
            Some("01a12d13-2870-7f61".to_owned())
        );
    }

    #[test]
    fn a_line_that_is_not_json_yields_nothing() {
        assert_eq!(thread_started_in("Reading prompt from stdin..."), None);
        assert_eq!(thread_started_in(r#"{"type":"thread.started","#), None);
    }

    #[test]
    fn other_events_yield_nothing_even_with_a_thread_id() {
        assert_eq!(
            thread_started_in(r#"{"type":"turn.started","thread_id":"01a12d13"}"#),
            None
        );
    }

    #[test]
    fn thread_started_without_a_string_id_yields_nothing() {
        assert_eq!(thread_started_in(r#"{"type":"thread.started"}"#), None);
        assert_eq!(
            thread_started_in(r#"{"type":"thread.started","thread_id":42}"#),
            None
        );
    }

    #[test]
    fn args_put_selection_flags_before_fork_and_the_parent_last() {
        let selection = AgentSelection {
            model: Some("gpt-6-luna".to_owned()),
            effort: Some("low".to_owned()),
            ..AgentSelection::default()
        };
        assert_eq!(
            fork_args("parent-id", &selection),
            [
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--dangerously-bypass-approvals-and-sandbox",
                "-C",
                ".",
                "-m",
                "gpt-6-luna",
                "-c",
                "model_reasoning_effort=low",
                "fork",
                "parent-id",
            ]
        );
    }

    #[test]
    fn args_omit_selection_flags_that_are_unset() {
        let args = fork_args("parent-id", &AgentSelection::default());
        assert!(!args.iter().any(|arg| arg == "-m" || arg == "-c"));
        assert_eq!(args[args.len() - 2..], ["fork", "parent-id"]);
    }
}
