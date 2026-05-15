use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use switchboard_core::{AgentId, AgentRecord};
use tokio::io::AsyncBufReadExt;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::adapter::{DispatchError, EventStream, HarnessAdapter};
use crate::events::{AdapterEvent, FailureKind, TurnId, TurnOutcome};
use crate::parser::{self, ParseOutcome, ParserState};

/// Adapter for Claude Code (`claude -p`). Spawns a `claude` subprocess,
/// feeds the prompt as a positional argument, and maps the stream-json output
/// into `AdapterEvent`s.
///
/// For testing, construct with `with_binary_path(path)` pointing to the
/// `fake_claude` fixture binary — the adapter's behaviour is identical;
/// only the binary changes.
pub struct ClaudeCodeAdapter {
    claude_binary_path: PathBuf,
}

impl ClaudeCodeAdapter {
    /// Production constructor. Uses `claude` from PATH.
    pub fn new() -> Self {
        Self {
            claude_binary_path: PathBuf::from("claude"),
        }
    }

    /// Override the binary path — used by tests to inject the `fake_claude` fixture binary.
    pub fn with_binary_path(path: impl Into<PathBuf>) -> Self {
        Self {
            claude_binary_path: path.into(),
        }
    }
}

impl Default for ClaudeCodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HarnessAdapter for ClaudeCodeAdapter {
    fn probe(&self) -> Result<(), DispatchError> {
        // `which::which` handles both absolute paths (checks existence +
        // executable bit) and relative names (PATH lookup), so it's a
        // stricter check than `resolve_binary` for symmetry with the error
        // we'd otherwise only learn about at spawn time.
        which::which(&self.claude_binary_path)
            .map(|_| ())
            .map_err(|_| DispatchError::BinaryNotFound)
    }

    async fn dispatch(
        &self,
        agent: &AgentRecord,
        cwd: &Path,
        prompt: &str,
        turn_id: TurnId,
    ) -> Result<EventStream, DispatchError> {
        let binary = resolve_binary(&self.claude_binary_path)?;
        let args = build_args(agent, prompt, cwd, None);

        let mut command = tokio::process::Command::new(&binary);
        command
            .args(&args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            // `kill_on_drop(true)` only fires when `child` is dropped — and
            // `child` is owned by `run_producer` (spawned task). Consumers
            // dropping the event stream does NOT propagate; the subprocess
            // continues until natural exit. M4 cancellation will need a
            // `CancellationToken` plumbed through so mid-turn cancel kills
            // the subprocess properly.
            .kill_on_drop(true);
        // Put the child in its own process group so M4's cancel can `killpg`
        // the entire subprocess tree. M2 doesn't add `killpg`; M2.2 just
        // establishes the group.
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DispatchError::BinaryNotFound
            } else {
                DispatchError::SpawnFailed(e)
            }
        })?;

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let agent_id = agent.id;

        tokio::spawn(run_producer(child, stdout, stderr, tx, turn_id, agent_id));

        Ok(Box::pin(UnboundedReceiverStream::new(rx)))
    }
}

/// Use `which` for relative names (PATH lookup); trust absolute paths directly.
/// Spawn itself will return `NotFound` for missing absolute paths and we map it
/// to `BinaryNotFound` at the call site.
fn resolve_binary(path: &Path) -> Result<PathBuf, DispatchError> {
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    which::which(path).map_err(|_| DispatchError::BinaryNotFound)
}

/// `home_override` is `None` in production (reads `$HOME`) and `Some(path)` in tests.
fn build_args(
    agent: &AgentRecord,
    prompt: &str,
    cwd: &Path,
    home_override: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        "-p".to_owned(),
        prompt.to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
        "--include-partial-messages".to_owned(),
        "--verbose".to_owned(),
        "--dangerously-skip-permissions".to_owned(),
    ];
    if let Some(session_id) = agent.session_id {
        // --session-id creates a new session with the given UUID (first turn).
        // --resume continues an existing session (all subsequent turns).
        // Claude Code stores sessions at ~/.claude/projects/<encoded-cwd>/<uuid>.jsonl;
        // we check that path to pick the right flag. The `cwd` used for the
        // session-path lookup must be the SAME cwd we pass to the
        // subprocess — claude computes its own session-storage path from
        // its actual cwd, so any divergence here means we look in the
        // wrong place and pass `--session-id` when we should `--resume`.
        let exists = match home_override {
            Some(home) => session_exists_in(home, cwd, &session_id),
            None => session_file_exists(cwd, &session_id),
        };
        if exists {
            args.push("--resume".to_owned());
        } else {
            args.push("--session-id".to_owned());
        }
        args.push(session_id.to_string());
    }
    args
}

/// Production wrapper: reads `$HOME` and delegates to `session_exists_in`.
fn session_file_exists(cwd: &Path, session_id: &uuid::Uuid) -> bool {
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    session_exists_in(Path::new(&home), cwd, session_id)
}

/// Pure check — testable without touching the real `$HOME`.
/// Claude Code stores sessions at `<home>/.claude/projects/<encoded-cwd>/<uuid>.jsonl`
/// where the encoded cwd replaces both `/` and `.` with `-`. For example,
/// `/Users/x/repo/.switchboard/projects/<id>` is encoded as
/// `-Users-x-repo--switchboard-projects-<id>` — the leading dot of
/// `.switchboard` becomes a dash, producing the double-dash `--switchboard`.
/// The empirically-observed rule, confirmed against `~/.claude/projects/`
/// listings for cwds containing dot-prefixed components.
fn session_exists_in(home: &Path, cwd: &Path, session_id: &uuid::Uuid) -> bool {
    let Ok(canonical) = cwd.canonicalize() else {
        return false;
    };
    let encoded = encode_cwd(&canonical);
    home.join(".claude")
        .join("projects")
        .join(&encoded)
        .join(format!("{session_id}.jsonl"))
        .exists()
}

/// Encodes a canonical absolute path the way Claude Code does for its
/// session-storage directory naming: every `/` and `.` becomes `-`. Switchboard's
/// own working paths reliably contain `.switchboard`, so getting this rule
/// exactly right is load-bearing — any mismatch causes the adapter to think a
/// session file is missing and pass `--session-id`, which claude rejects with
/// "Session ID … is already in use" on subsequent turns.
fn encode_cwd(canonical: &Path) -> String {
    canonical.to_string_lossy().replace(['/', '.'], "-")
}

/// Most recent stderr lines we keep around so we can include them in the
/// synthesized failure message when the subprocess exits without a terminal
/// event. Bounded to avoid unbounded growth on a chatty subprocess.
const STDERR_TAIL_CAPACITY: usize = 16;

async fn run_producer(
    mut child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    tx: tokio::sync::mpsc::UnboundedSender<AdapterEvent>,
    turn_id: TurnId,
    agent_id: AgentId,
) {
    // Drain stderr concurrently; prevents pipe-full deadlock if the subprocess
    // writes to stderr while we block reading stdout. The shared `stderr_tail`
    // buffer captures the last few lines for inclusion in failure messages.
    let stderr_tail: Arc<Mutex<VecDeque<String>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_CAPACITY)));
    let stderr_task = tokio::spawn(drain_stderr(
        stderr,
        agent_id,
        turn_id,
        Arc::clone(&stderr_tail),
    ));

    let mut terminal_seen = false;
    let mut terminal_was_completed = false;
    let mut parser_state = ParserState::default();

    let mut lines = tokio::io::BufReader::new(stdout).lines();

    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let outcome = parser::parse_line(&line, turn_id, agent_id, &mut parser_state);
                let events = match outcome {
                    ParseOutcome::Event(event) => vec![event],
                    ParseOutcome::Events(events) => events,
                    ParseOutcome::Skip => continue,
                    ParseOutcome::Error(msg) => {
                        let _ = tx.send(AdapterEvent::TurnEnd {
                            turn_id,
                            outcome: TurnOutcome::Failed {
                                kind: FailureKind::AdapterFailure,
                                message: format!("malformed JSON from harness: {msg}"),
                            },
                            ended_at: Utc::now(),
                            usage: None,
                        });
                        terminal_seen = true;
                        break;
                    }
                };
                for event in events {
                    match &event {
                        AdapterEvent::TurnEnd {
                            outcome: TurnOutcome::Completed,
                            ..
                        } => {
                            terminal_was_completed = true;
                            terminal_seen = true;
                        }
                        AdapterEvent::TurnEnd { .. } => {
                            terminal_seen = true;
                        }
                        _ => {}
                    }
                    // Receiver drop is intentional: if the consumer disconnects
                    // mid-stream, we let the producer drain and exit cleanly.
                    // Per-turn cancel (M4) will handle the shutdown case properly.
                    let _ = tx.send(event);
                }
                if terminal_seen {
                    break;
                }
            }
            Ok(None) => break, // stdout EOF
            Err(e) => {
                let _ = tx.send(AdapterEvent::TurnEnd {
                    turn_id,
                    outcome: TurnOutcome::Failed {
                        kind: FailureKind::AdapterFailure,
                        message: format!("stdout read error: {e}"),
                    },
                    ended_at: Utc::now(),
                    usage: None,
                });
                terminal_seen = true;
                break;
            }
        }
    }

    // Wait for the stderr drain to finish before reading the tail — gives the
    // drain task a chance to capture any final lines after stdout EOF.
    let _ = stderr_task.await;

    // Stream contract: ensure exactly one terminal event was emitted. The
    // failure message includes the tail of stderr so the consumer can see
    // why the subprocess exited silently (auth error, flag rejection, etc.).
    if !terminal_seen {
        let _ = tx.send(synthesize_truncation_turn_end(turn_id, &stderr_tail));
    }

    // Reap subprocess. If the parser observed Completed but the exit code is
    // non-zero, log the discrepancy — per M1 policy, we do not re-emit. M2
    // revisits whether to hold terminal emission until after reconciliation.
    match child.wait().await {
        Ok(status) if !status.success() && terminal_was_completed => {
            tracing::warn!(
                %turn_id,
                agent_id = %agent_id,
                exit_code = ?status.code(),
                "harness emitted result:completed but subprocess exited non-zero — log-only per M1 policy"
            );
        }
        Err(e) => {
            tracing::warn!(
                %turn_id,
                agent_id = %agent_id,
                error = %e,
                "failed to wait on harness subprocess"
            );
        }
        _ => {}
    }
}

async fn drain_stderr(
    stderr: tokio::process::ChildStderr,
    agent_id: AgentId,
    turn_id: TurnId,
    tail: Arc<Mutex<VecDeque<String>>>,
) {
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                tracing::debug!(agent_id = %agent_id, %turn_id, "harness stderr: {line}");
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

/// Build the synthesized `TurnEnd(Failed)` event emitted when stdout EOFs
/// without a terminal `result` event. Includes the captured stderr tail so
/// the consumer can see the underlying cause (auth error, flag rejection).
fn synthesize_truncation_turn_end(
    turn_id: TurnId,
    stderr_tail: &Mutex<VecDeque<String>>,
) -> AdapterEvent {
    let stderr_msg = format_stderr_tail(stderr_tail);
    let message = if stderr_msg.is_empty() {
        "harness exited without terminal result event (no stderr captured)".to_owned()
    } else {
        format!("harness exited without terminal result event; stderr: {stderr_msg}")
    };
    AdapterEvent::TurnEnd {
        turn_id,
        outcome: TurnOutcome::Failed {
            kind: FailureKind::AdapterFailure,
            message,
        },
        ended_at: Utc::now(),
        usage: None,
    }
}

/// Bound the failure-message length so it stays readable in the UI.
const STDERR_MESSAGE_MAX_LEN: usize = 800;

/// Returns a single-line, length-bounded representation of the captured
/// stderr tail. Empty string if no lines were captured. Length-bounding is
/// performed on **char boundaries** — slicing a String by byte offsets can
/// land mid-UTF-8 and panic (real risk with non-ASCII paths or error
/// messages in stderr).
fn format_stderr_tail(tail: &Mutex<VecDeque<String>>) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use switchboard_core::HarnessKind;
    use uuid::Uuid;

    fn agent_with_session(session_id: Uuid) -> AgentRecord {
        AgentRecord {
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            name: "test".to_owned(),
            harness: HarnessKind::ClaudeCode,
            session_id: Some(session_id),
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn session_exists_in_encodes_path_and_detects_file() {
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let session_id = Uuid::now_v7();

        assert!(
            !session_exists_in(home.path(), project.path(), &session_id),
            "no file yet"
        );

        let canonical = project.path().canonicalize().unwrap();
        let encoded = encode_cwd(&canonical);
        let session_dir = home.path().join(".claude").join("projects").join(&encoded);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join(format!("{session_id}.jsonl")), "").unwrap();

        assert!(
            session_exists_in(home.path(), project.path(), &session_id),
            "file exists now"
        );
    }

    #[test]
    fn build_args_uses_session_id_when_no_file() {
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let agent = agent_with_session(Uuid::now_v7());

        let args = build_args(&agent, "hi", project.path(), Some(home.path()));

        assert!(args.contains(&"--session-id".to_owned()));
        assert!(!args.contains(&"--resume".to_owned()));
    }

    #[test]
    fn build_args_uses_resume_when_session_file_exists() {
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent = agent_with_session(session_id);

        let canonical = project.path().canonicalize().unwrap();
        let encoded = encode_cwd(&canonical);
        let session_dir = home.path().join(".claude").join("projects").join(&encoded);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join(format!("{session_id}.jsonl")), "").unwrap();

        let args = build_args(&agent, "hi", project.path(), Some(home.path()));

        assert!(args.contains(&"--resume".to_owned()));
        assert!(!args.contains(&"--session-id".to_owned()));
    }

    #[test]
    fn encode_cwd_replaces_dots_and_slashes_with_dashes() {
        // All cases below are empirically verified against `claude` itself
        // by running it in each cwd shape and inspecting where it created
        // its session-storage directory under `~/.claude/projects/`. See
        // `docs/research/claude-code-cli-observed.md` for the probe.

        // Switchboard's actual on-disk layout: `.switchboard/` dot-prefixed
        // component must produce `--switchboard` (double dash).
        assert_eq!(
            encode_cwd(Path::new("/Users/x/repo/.switchboard/projects/abc")),
            "-Users-x-repo--switchboard-projects-abc"
        );
        // No-dots baseline.
        assert_eq!(
            encode_cwd(Path::new("/Users/shanekercheval/repos/temp")),
            "-Users-shanekercheval-repos-temp"
        );
        // Mid-component dot (e.g., a username with a dot, or a package-style name).
        assert_eq!(
            encode_cwd(Path::new("/private/tmp/sw-probe/foo.bar/sub")),
            "-private-tmp-sw-probe-foo-bar-sub"
        );
        // Leading dot of a path component (hidden directory).
        assert_eq!(
            encode_cwd(Path::new("/private/tmp/sw-probe/.hidden/sub")),
            "-private-tmp-sw-probe--hidden-sub"
        );
        // Multiple dots in one component (mixed leading + mid).
        assert_eq!(
            encode_cwd(Path::new("/private/tmp/sw-probe/foo/.bar.baz")),
            "-private-tmp-sw-probe-foo--bar-baz"
        );
        // Version-style component with several mid-dots.
        assert_eq!(
            encode_cwd(Path::new("/private/tmp/sw-probe/foo/version.1.2.3")),
            "-private-tmp-sw-probe-foo-version-1-2-3"
        );
    }

    #[test]
    fn session_exists_in_handles_dot_components_in_cwd() {
        // The cwd we spawn claude in is the user's bound working directory,
        // which can contain dots (hidden directories, dotted usernames like
        // `/Users/john.doe/...`, dotted middle components like
        // `my.app/src/`). The encoding rule `/` + `.` → `-` has to match
        // claude's actual rule, otherwise we look for the session file in
        // the wrong place, pass `--session-id` on the second turn, and
        // claude rejects with "Session ID already in use".
        let home = tempfile::TempDir::new().unwrap();
        let parent = tempfile::TempDir::new().unwrap();
        // A user-realistic working directory containing a dot-prefixed
        // component (hidden dir) plus a mid-component dot — both shapes
        // the encoding must handle.
        let cwd = parent.path().join(".config").join("my.app");
        std::fs::create_dir_all(&cwd).unwrap();
        let session_id = Uuid::now_v7();

        // Pre-create the session file at the path claude would write it to.
        let canonical = cwd.canonicalize().unwrap();
        let encoded = encode_cwd(&canonical);
        // Sanity: dot-prefixed and mid-dot components are both stripped.
        assert!(
            encoded.contains("--config-my-app"),
            "encoded path should strip both dots (got: {encoded})"
        );
        let session_dir = home.path().join(".claude").join("projects").join(&encoded);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join(format!("{session_id}.jsonl")), "").unwrap();

        // Detection works through the dot-stripping encoding.
        assert!(session_exists_in(home.path(), &cwd, &session_id));

        // build_args therefore picks --resume on the second turn — not
        // --session-id, which would cause the "already in use" rejection.
        let agent = agent_with_session(session_id);
        let args = build_args(&agent, "hi", &cwd, Some(home.path()));
        assert!(
            args.contains(&"--resume".to_owned()),
            "expected --resume when session file exists, got: {args:?}"
        );
        assert!(
            !args.contains(&"--session-id".to_owned()),
            "must not pass --session-id when the session already exists"
        );
    }

    #[test]
    fn format_stderr_tail_handles_non_ascii_at_truncation_boundary() {
        // Regression: byte-slicing on a String can land mid-UTF-8 and
        // panic with "byte index N is not a char boundary." Stderr from
        // real subprocesses often contains paths or messages with
        // multi-byte characters (e.g., accented usernames, emoji, smart
        // quotes). Truncation must walk to a char boundary.
        let tail: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
        // 600 ASCII chars + a 3-byte "…" emoji repeated so multi-byte
        // chars sit near the truncation boundary at byte 800.
        let mut payload = "A".repeat(600);
        for _ in 0..150 {
            payload.push('…'); // 3 bytes per ellipsis
        }
        // payload is 600 + 450 = 1050 bytes, well over 800. The byte at
        // position (len - 800) almost certainly lands mid-character.
        tail.lock().unwrap().push_back(payload);

        let result = format_stderr_tail(&tail);
        // The leading-ellipsis prefix + char-boundary slicing means total
        // bytes is bounded by STDERR_MESSAGE_MAX_LEN + a small constant
        // (the prefix). Critically: NO PANIC.
        assert!(
            result.starts_with('…'),
            "truncated output should be prefixed with …"
        );
        assert!(result.chars().count() < 850);
    }

    #[test]
    fn probe_reports_missing_binary_for_absolute_path() {
        let adapter = ClaudeCodeAdapter::with_binary_path("/nonexistent/path/to/claude");
        assert!(matches!(
            adapter.probe(),
            Err(DispatchError::BinaryNotFound)
        ));
    }

    #[test]
    fn probe_reports_missing_binary_for_relative_name() {
        let adapter =
            ClaudeCodeAdapter::with_binary_path("this-binary-does-not-exist-on-PATH-xyz123");
        assert!(matches!(
            adapter.probe(),
            Err(DispatchError::BinaryNotFound)
        ));
    }
}
