use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use chrono::Utc;
use switchboard_core::{AgentId, AgentRecord};
use tokio::io::AsyncBufReadExt;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::adapter::{DispatchError, EventStream, HarnessAdapter};
use crate::events::{AdapterEvent, FailureKind, TurnId, TurnOutcome};
use crate::parser::{self, ParseOutcome};

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
    async fn dispatch(
        &self,
        agent: &AgentRecord,
        project_root: &Path,
        prompt: &str,
        turn_id: TurnId,
    ) -> Result<EventStream, DispatchError> {
        let binary = resolve_binary(&self.claude_binary_path)?;
        let args = build_args(agent, prompt, project_root, None);

        let mut child = tokio::process::Command::new(&binary)
            .args(&args)
            .current_dir(project_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
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
    project_root: &Path,
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
        // we check that path to pick the right flag.
        let exists = match home_override {
            Some(home) => session_exists_in(home, project_root, &session_id),
            None => session_file_exists(project_root, &session_id),
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
fn session_file_exists(project_root: &Path, session_id: &uuid::Uuid) -> bool {
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    session_exists_in(Path::new(&home), project_root, session_id)
}

/// Pure check — testable without touching the real `$HOME`.
/// Claude Code stores sessions at `<home>/.claude/projects/<encoded-cwd>/<uuid>.jsonl`
/// where the encoded cwd is the canonicalized path with every `/` replaced by `-`.
fn session_exists_in(home: &Path, project_root: &Path, session_id: &uuid::Uuid) -> bool {
    let Ok(canonical) = project_root.canonicalize() else {
        return false;
    };
    let encoded = canonical.to_string_lossy().replace('/', "-");
    home.join(".claude")
        .join("projects")
        .join(&encoded)
        .join(format!("{session_id}.jsonl"))
        .exists()
}

async fn run_producer(
    mut child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    tx: tokio::sync::mpsc::UnboundedSender<AdapterEvent>,
    turn_id: TurnId,
    agent_id: AgentId,
) {
    // Drain stderr concurrently; prevents pipe-full deadlock if the subprocess
    // writes to stderr while we block reading stdout.
    let stderr_task = tokio::spawn(drain_stderr(stderr, agent_id, turn_id));

    let mut terminal_seen = false;
    let mut terminal_was_completed = false;

    let mut lines = tokio::io::BufReader::new(stdout).lines();

    loop {
        match lines.next_line().await {
            Ok(Some(line)) => match parser::parse_line(&line, turn_id) {
                ParseOutcome::Event(event) => {
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
                    // Per-turn cancel (M3) will handle the shutdown case properly.
                    let _ = tx.send(event);
                    if terminal_seen {
                        break;
                    }
                }
                ParseOutcome::Skip => {}
                ParseOutcome::Error(msg) => {
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Failed {
                            kind: FailureKind::AdapterFailure,
                            message: format!("malformed JSON from harness: {msg}"),
                        },
                        ended_at: Utc::now(),
                    });
                    terminal_seen = true;
                    break;
                }
            },
            Ok(None) => break, // stdout EOF
            Err(e) => {
                let _ = tx.send(AdapterEvent::TurnEnd {
                    turn_id,
                    outcome: TurnOutcome::Failed {
                        kind: FailureKind::AdapterFailure,
                        message: format!("stdout read error: {e}"),
                    },
                    ended_at: Utc::now(),
                });
                terminal_seen = true;
                break;
            }
        }
    }

    // Stream contract: ensure exactly one terminal event was emitted.
    if !terminal_seen {
        let _ = tx.send(AdapterEvent::TurnEnd {
            turn_id,
            outcome: TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message: "harness exited without terminal result event".to_owned(),
            },
            ended_at: Utc::now(),
        });
    }

    let _ = stderr_task.await;

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

async fn drain_stderr(stderr: tokio::process::ChildStderr, agent_id: AgentId, turn_id: TurnId) {
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                tracing::debug!(agent_id = %agent_id, %turn_id, "harness stderr: {line}");
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(agent_id = %agent_id, %turn_id, error = %e, "stderr read error");
                break;
            }
        }
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
        let encoded = canonical.to_string_lossy().replace('/', "-");
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
        let encoded = canonical.to_string_lossy().replace('/', "-");
        let session_dir = home.path().join(".claude").join("projects").join(&encoded);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join(format!("{session_id}.jsonl")), "").unwrap();

        let args = build_args(&agent, "hi", project.path(), Some(home.path()));

        assert!(args.contains(&"--resume".to_owned()));
        assert!(!args.contains(&"--session-id".to_owned()));
    }
}
