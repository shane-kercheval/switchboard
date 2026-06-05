pub mod config;
pub mod session_file;
pub mod skills;

pub use session_file::load_claude_transcript;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use chrono::Utc;
use switchboard_core::{AgentId, AgentRecord, SessionLocator};
use tokio::io::AsyncBufReadExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_util::sync::CancellationToken;

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
    /// Lazily-resolved `claude --version`, cached for the lifetime of the
    /// adapter. Empty string caches a failed/absent probe (version is
    /// display-only). Mirrors the Gemini/Antigravity pattern.
    cached_version: OnceLock<String>,
}

impl ClaudeCodeAdapter {
    /// Production constructor. Uses `claude` from PATH.
    pub fn new() -> Self {
        Self {
            claude_binary_path: PathBuf::from("claude"),
            cached_version: OnceLock::new(),
        }
    }

    /// Override the binary path — used by tests to inject the `fake_claude` fixture binary.
    pub fn with_binary_path(path: impl Into<PathBuf>) -> Self {
        Self {
            claude_binary_path: path.into(),
            cached_version: OnceLock::new(),
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
        crate::subprocess::probe_binary(&self.claude_binary_path)
    }

    fn version(&self) -> Option<String> {
        let raw = self.cached_version.get_or_init(|| {
            crate::subprocess::fetch_version(&self.claude_binary_path).unwrap_or_default()
        });
        crate::subprocess::parse_cli_version(raw)
    }

    async fn dispatch(
        &self,
        agent: &AgentRecord,
        cwd: &Path,
        prompt: &str,
        turn_id: TurnId,
        options: crate::DispatchOptions,
    ) -> Result<EventStream, DispatchError> {
        // Claude Code emits `SessionMeta` from its `system/init` stream event
        // on every dispatch — no first-turn gating — so
        // `options.is_first_dispatch_after_attach` has nothing to do here.
        // `options.cancel_token` IS used: it's watched in the producer's
        // `select!` to cancel the turn.
        let binary = crate::subprocess::resolve_binary(&self.claude_binary_path)?;
        let args = build_args(agent, prompt, cwd, None);

        let mut command = tokio::process::Command::new(&binary);
        command
            .args(&args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Null stdin: we never write to it, and an open stdin can stall a
            // harness on an interactive read or a pipe-full deadlock.
            .stdin(Stdio::null())
            // Belt-and-suspenders teardown: `kill_on_drop` fires only when
            // `child` is dropped, which happens if the producer task itself is
            // dropped/aborted. Intentional cancellation flows through
            // `options.cancel_token` (watched in `run_producer`), which kills
            // the whole process group; `kill_on_drop` just covers the
            // producer-task-teardown edge.
            .kill_on_drop(true);
        crate::subprocess::apply_path_env(&mut command);
        // Own process group so `killpg` (in the cancel path) tears down the
        // entire subprocess tree, not just the spawned PID.
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

        tokio::spawn(run_producer(
            child,
            stdout,
            stderr,
            tx,
            turn_id,
            agent_id,
            options.cancel_token,
        ));

        Ok(Box::pin(UnboundedReceiverStream::new(rx)))
    }
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
        "--output-format".to_owned(),
        "stream-json".to_owned(),
        "--include-partial-messages".to_owned(),
        "--verbose".to_owned(),
        "--dangerously-skip-permissions".to_owned(),
    ];
    if let Some(SessionLocator::Uuid(session_id)) = &agent.session_locator {
        // --session-id creates a new session with the given UUID (first turn).
        // --resume continues an existing session (all subsequent turns).
        // Claude Code stores sessions at ~/.claude/projects/<encoded-cwd>/<uuid>.jsonl;
        // we check that path to pick the right flag. The `cwd` used for the
        // session-path lookup must be the SAME cwd we pass to the
        // subprocess — claude computes its own session-storage path from
        // its actual cwd, so any divergence here means we look in the
        // wrong place and pass `--session-id` when we should `--resume`.
        let exists = match home_override {
            Some(home) => session_exists_in(home, cwd, session_id),
            None => session_file_exists(cwd, session_id),
        };
        if exists {
            args.push("--resume".to_owned());
        } else {
            args.push("--session-id".to_owned());
        }
        args.push(session_id.to_string());
    }
    // Per-agent selection (sent every turn when set; unset → harness default).
    // `--model` takes an alias (`sonnet`/`opus`) or a full id; `--effort` takes
    // a reasoning level. Both must go BEFORE the `--` below — see the note.
    if let Some(model) = &agent.model {
        args.push("--model".to_owned());
        args.push(model.clone());
    }
    if let Some(effort) = &agent.effort {
        args.push("--effort".to_owned());
        args.push(effort.clone());
    }
    // `claude -p` takes the prompt as a positional. Pass it last, after a `--`
    // end-of-options separator, so a prompt beginning with `-` (e.g. a markdown
    // bullet) is not parsed as an unknown flag — without it `claude` aborts with
    // `unknown option '- …'` before any model call. Verified against claude 2.1.162.
    // Any flag added later must be pushed BEFORE this `--`, or it lands as a
    // positional alongside the prompt.
    args.push("--".to_owned());
    args.push(prompt.to_owned());
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
fn session_exists_in(home: &Path, cwd: &Path, session_id: &uuid::Uuid) -> bool {
    let Ok(canonical) = cwd.canonicalize() else {
        return false;
    };
    claude_session_file_path(home, &canonical, session_id).exists()
}

/// Compute the canonical Claude Code session-file path. Claude Code stores
/// sessions at `<home>/.claude/projects/<encoded-cwd>/<uuid>.jsonl` where the
/// encoded cwd replaces both `/` and `.` with `-`. For example,
/// `/Users/x/repo/.switchboard/projects/<id>` is encoded as
/// `-Users-x-repo--switchboard-projects-<id>` — the leading dot of
/// `.switchboard` becomes a dash, producing the double-dash `--switchboard`.
/// The empirically-observed rule, confirmed against `~/.claude/projects/`
/// listings for cwds containing dot-prefixed components.
///
/// **Caller contract.** `cwd` must be a *canonical* absolute path (no
/// symlinks, no `..`). The attach-flow caller resolves cwd via
/// `Directory::at(...)` which canonicalizes; pass `directory.path` directly.
/// Passing a non-canonical cwd produces a wrong encoding and the lookup will
/// miss the real session file.
#[must_use]
pub fn claude_session_file_path(home: &Path, cwd: &Path, session_id: &uuid::Uuid) -> PathBuf {
    let encoded = encode_cwd(cwd);
    home.join(".claude")
        .join("projects")
        .join(&encoded)
        .join(format!("{session_id}.jsonl"))
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

// Parallels the Codex / Gemini producers: a single per-line control-flow loop
// plus the cancel and post-loop terminal handling. Splitting it would fragment
// that flow without improving readability.
#[allow(clippy::too_many_lines)]
async fn run_producer(
    mut child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    tx: tokio::sync::mpsc::UnboundedSender<AdapterEvent>,
    turn_id: TurnId,
    agent_id: AgentId,
    cancel_token: CancellationToken,
) {
    // Drain stderr concurrently; prevents pipe-full deadlock if the subprocess
    // writes to stderr while we block reading stdout. The shared `stderr_tail`
    // buffer captures the last few lines for inclusion in failure messages.
    let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::with_capacity(
        crate::subprocess::STDERR_TAIL_CAPACITY,
    )));
    let stderr_task = tokio::spawn(crate::subprocess::drain_stderr(
        stderr,
        agent_id,
        turn_id,
        Arc::clone(&stderr_tail),
        "claude",
    ));

    let mut terminal_seen = false;
    let mut terminal_was_completed = false;
    // Set when the cancellation token fires. On cancel the adapter kills the
    // subprocess group and ends the stream WITHOUT a terminal event — the
    // dispatcher synthesizes `TurnEnd { Cancelled { source } }` (it owns the
    // cancel outcome; a binary token can't carry the source). So the cancel
    // path must skip the truncation synthesis below.
    let mut cancelled = false;
    let mut parser_state = ParserState::default();

    let mut lines = tokio::io::BufReader::new(stdout).lines();

    loop {
        // `select!` over the next-line read AND the cancellation token, so a
        // parked read (a buffering harness producing no output yet) does not
        // block noticing a cancel.
        let line = tokio::select! {
            line = lines.next_line() => line,
            () = cancel_token.cancelled() => {
                cancelled = true;
                break;
            }
        };
        match line {
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
                            context_window_source: None,
                            stable_message_id: None,
                            spend: None,
                            model: None,
                            effort: None,
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
                    context_window_source: None,
                    stable_message_id: None,
                    spend: None,
                    model: None,
                    effort: None,
                });
                terminal_seen = true;
                break;
            }
        }
    }

    if cancelled {
        // Cancellation path: kill the subprocess group (SIGTERM → grace →
        // SIGKILL, leaving Claude's session file resumable with the incomplete
        // turn absent) and end the stream with NO terminal event. The
        // dispatcher synthesizes the `Cancelled` terminal. Kill *before*
        // awaiting the stderr drain: a parked subprocess still holds stderr
        // open, so awaiting the drain first would block until the kill anyway.
        crate::subprocess::terminate_then_kill(&mut child).await;
        let _ = stderr_task.await;
        return;
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
    // non-zero, log the discrepancy — we do not re-emit. Whether to hold
    // terminal emission until after reconciliation is future work.
    match child.wait().await {
        Ok(status) if !status.success() && terminal_was_completed => {
            tracing::warn!(
                %turn_id,
                agent_id = %agent_id,
                exit_code = ?status.code(),
                "harness emitted result:completed but subprocess exited non-zero — log-only"
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

/// Build the synthesized `TurnEnd(Failed)` event emitted when stdout EOFs
/// without a terminal `result` event. Includes the captured stderr tail so
/// the consumer can see the underlying cause (auth error, flag rejection).
fn synthesize_truncation_turn_end(
    turn_id: TurnId,
    stderr_tail: &Mutex<VecDeque<String>>,
) -> AdapterEvent {
    let stderr_msg = crate::subprocess::format_stderr_tail(stderr_tail);
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
        context_window_source: None,
        stable_message_id: None,
        spend: None,
        model: None,
        effort: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchboard_core::HarnessKind;
    use uuid::Uuid;

    fn agent_with_session(session_id: Uuid) -> AgentRecord {
        AgentRecord {
            model: None,
            effort: None,
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            name: "test".to_owned(),
            harness: HarnessKind::ClaudeCode,
            session_locator: Some(SessionLocator::Uuid(session_id)),
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
    fn build_args_dash_leading_prompt_is_last_positional_after_separator() {
        // Regression: `claude -p` takes the prompt as a positional, so a prompt
        // beginning with `-`/`--` must trail a `--` separator or `claude` aborts
        // with `unknown option '- …'`.
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let agent = agent_with_session(Uuid::now_v7());
        for prompt in ["- the left border is cut off", "--help"] {
            let args = build_args(&agent, prompt, project.path(), Some(home.path()));
            assert_eq!(args.last(), Some(&prompt.to_owned()));
            assert_eq!(
                args[args.len() - 2],
                "--",
                "prompt is the last positional, preceded by `--`; got {args:?}"
            );
        }
    }

    #[test]
    fn build_args_omits_session_flags_when_locator_absent() {
        // Defensive: Claude agents always pre-mint a locator, but `build_args`
        // is a pure function — a `None` locator must omit both session flags
        // rather than emit a flag with no id.
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let agent = AgentRecord {
            model: None,
            effort: None,
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            name: "test".to_owned(),
            harness: HarnessKind::ClaudeCode,
            session_locator: None,
            created_at: chrono::Utc::now(),
        };

        let args = build_args(&agent, "hi", project.path(), Some(home.path()));

        assert!(!args.contains(&"--session-id".to_owned()));
        assert!(!args.contains(&"--resume".to_owned()));
    }

    /// Index of `flag` immediately followed by `value` in an arg vec.
    fn flag_value_pos(args: &[String], flag: &str, value: &str) -> Option<usize> {
        args.windows(2).position(|w| w[0] == flag && w[1] == value)
    }

    #[test]
    fn build_args_includes_model_and_effort_when_set() {
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let mut agent = agent_with_session(Uuid::now_v7());
        agent.model = Some("sonnet".to_owned());
        agent.effort = Some("high".to_owned());

        let args = build_args(&agent, "hi", project.path(), Some(home.path()));

        let model_pos = flag_value_pos(&args, "--model", "sonnet").expect("--model sonnet present");
        let effort_pos = flag_value_pos(&args, "--effort", "high").expect("--effort high present");
        // Both must precede the `--` separator, else they'd be parsed as
        // positionals alongside the prompt.
        let sep = args.iter().position(|a| a == "--").unwrap();
        assert!(model_pos < sep, "--model must precede `--`; got {args:?}");
        assert!(effort_pos < sep, "--effort must precede `--`; got {args:?}");
    }

    #[test]
    fn build_args_omits_model_and_effort_when_unset() {
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let agent = agent_with_session(Uuid::now_v7());

        let args = build_args(&agent, "hi", project.path(), Some(home.path()));

        assert!(!args.contains(&"--model".to_owned()));
        assert!(!args.contains(&"--effort".to_owned()));
    }

    #[test]
    fn build_args_carries_model_and_effort_on_resume_path() {
        // The selection rides every turn, including resumes — same flags,
        // still before the `--` separator.
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let mut agent = agent_with_session(session_id);
        agent.model = Some("opus".to_owned());
        agent.effort = Some("max".to_owned());

        let canonical = project.path().canonicalize().unwrap();
        let encoded = encode_cwd(&canonical);
        let session_dir = home.path().join(".claude").join("projects").join(&encoded);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join(format!("{session_id}.jsonl")), "").unwrap();

        let args = build_args(&agent, "hi", project.path(), Some(home.path()));

        assert!(
            args.contains(&"--resume".to_owned()),
            "resume path: {args:?}"
        );
        let sep = args.iter().position(|a| a == "--").unwrap();
        assert!(flag_value_pos(&args, "--model", "opus").unwrap() < sep);
        assert!(flag_value_pos(&args, "--effort", "max").unwrap() < sep);
    }

    #[test]
    fn encode_cwd_replaces_dots_and_slashes_with_dashes() {
        // All cases below are empirically verified against `claude` itself
        // by running it in each cwd shape and inspecting where it created
        // its session-storage directory under `~/.claude/projects/`. See
        // `docs/research/archive/claude-code-cli-observed.md` for the probe.

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
