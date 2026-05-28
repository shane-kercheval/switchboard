//! Gemini CLI adapter — spawns `gemini -p` and maps Gemini's flat
//! stream-json vocabulary onto the normalized event model.
//!
//! Pattern parallels Claude Code (caller-controlled session ID via
//! `--session-id` / `--resume`; pre-generated at agent registration), with
//! these Gemini-specific behaviors documented here so a future refactor
//! doesn't undo them:
//!
//! - **UUID v4 for session IDs.** Gemini's session-file naming uses the
//!   first 8 hex chars of the session ID as a filename suffix. UUID v7s
//!   minted in the same millisecond share their first 8 chars, causing
//!   on-disk session-file interleave under concurrent dispatch. v4's first
//!   8 chars are random across 32 bits, so the collision probability is
//!   ~1/2^32. The v4 mint lives in `switchboard_core::Project::register_agent`'s
//!   `HarnessKind::Gemini` arm; this adapter relies on the caller honoring
//!   that contract.
//! - **`--skip-trust` always.** Gemini's workspace-trust gate blocks
//!   headless dispatches by default with exit 0, empty stdout, and an
//!   error on stderr. Switchboard's bound cwd is by definition the user's
//!   working directory, so the gate's question is already answered;
//!   `--skip-trust` asserts that unconditionally.
//! - **`update_topic` is filtered.** Gemini auto-fires an `update_topic`
//!   builtin on most non-trivial turns to manage its own internal topic
//!   context. The parser's `GEMINI_INTERNAL_TOOL_NAMES` deny-list filters
//!   it from `ToolStarted` / `ToolCompleted` emission so it doesn't
//!   pollute the unified transcript. Constant is shared with the hydrator
//!   so the rule stays in lockstep across surfaces.
//! - **Live tool output may be empty for read-like tools.** Gemini's
//!   stream emits `output:""` for `read_file` and likely other user-data
//!   tools even on success; the real content lives in the session file.
//!   The adapter still emits a `ToolCompleted` event (lifecycle is the
//!   live contract); transcript hydration on project reopen fills in the
//!   real output. See `docs/research/archive/gemini-cli-observed.md` §"`read_file`
//!   stream gap" for the architectural rationale.
//! - **Lazy `harness_version` fetch.** The constructor stays cheap and
//!   non-failing (matches Claude / Codex). `gemini --version` is invoked
//!   on first dispatch via `OnceLock<String>`; failure caches `""`, which
//!   is acceptable to downstream consumers (the version field is
//!   display-only).
//!
//! MCP server registry: loaded from `~/.gemini/settings.json` (user
//! scope) and `<cwd>/.gemini/settings.json` (project / workspace scope).
//! Skills: loaded from `~/.agents/skills/` (user scope) and
//! `<cwd>/.gemini/skills/` (workspace scope). Both are display-only
//! surfaces — failures degrade to empty lists with a warning, never
//! propagate as `Result::Err`. See `config.rs` and `skills.rs` for the
//! per-scope path layout and merge rules.

pub mod config;
pub mod parser;
pub mod session_file;
pub mod skills;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use chrono::Utc;
use switchboard_core::{AgentId, AgentRecord};
use tokio::io::AsyncBufReadExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::adapter::{DispatchError, EventStream, HarnessAdapter};
use crate::events::{AdapterEvent, FailureKind, TurnId, TurnOutcome};

use parser::{GeminiParserState, is_gemini_auth_failure_message, parse_line};

/// Stderr lines emitted by Gemini that carry no diagnostic value. The
/// exit-42 handler filters them before classifying the remaining lines.
const GEMINI_STDERR_NOISE_PREFIXES: &[&str] = &[
    "YOLO mode is enabled",
    "Ripgrep is not available",
    "Approval mode overridden",
];

pub struct GeminiAdapter {
    gemini_binary_path: PathBuf,
    home_dir_override: Option<PathBuf>,
    /// Cached output of `gemini --version`, fetched on first dispatch.
    /// Cleared `""` on lookup failure to avoid re-running on every dispatch
    /// (one failed binary lookup likely means the binary is still missing).
    cached_version: OnceLock<String>,
}

impl GeminiAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            gemini_binary_path: PathBuf::from("gemini"),
            home_dir_override: None,
            cached_version: OnceLock::new(),
        }
    }

    pub fn with_binary_path(path: impl Into<PathBuf>) -> Self {
        Self {
            gemini_binary_path: path.into(),
            home_dir_override: None,
            cached_version: OnceLock::new(),
        }
    }

    pub fn with_binary_and_home(
        binary_path: impl Into<PathBuf>,
        home_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            gemini_binary_path: binary_path.into(),
            home_dir_override: Some(home_dir.into()),
            cached_version: OnceLock::new(),
        }
    }

    /// Resolve the cached version string. Returns `""` if `gemini --version`
    /// cannot be invoked or its output is empty — both acceptable to
    /// downstream consumers (the field is display-only).
    fn resolve_version(&self) -> String {
        self.cached_version
            .get_or_init(|| fetch_version(&self.gemini_binary_path))
            .clone()
    }
}

impl Default for GeminiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HarnessAdapter for GeminiAdapter {
    fn probe(&self) -> Result<(), DispatchError> {
        which::which(&self.gemini_binary_path)
            .map(|_| ())
            .map_err(|_| DispatchError::BinaryNotFound)
    }

    async fn dispatch(
        &self,
        agent: &AgentRecord,
        cwd: &Path,
        prompt: &str,
        turn_id: TurnId,
        options: crate::DispatchOptions,
    ) -> Result<EventStream, DispatchError> {
        if prompt.trim().is_empty() {
            return Err(DispatchError::InvalidPrompt(
                "prompt is empty or whitespace-only".to_owned(),
            ));
        }

        let binary = crate::subprocess::resolve_binary(&self.gemini_binary_path)?;
        let home_dir = resolve_home_dir(self.home_dir_override.as_deref());
        let args = build_args(agent, prompt, cwd, &home_dir);
        let harness_version = self.resolve_version();

        // Pre-load the MCP / skills registries once per dispatch. Mirrors
        // Codex's "load fresh on every emission, no caching layer" policy
        // — registry edits made between dispatches surface immediately
        // without a Switchboard restart.
        let mcp_servers = config::load_mcp_servers(&home_dir, cwd);
        let skills_list = skills::load_skills(&home_dir, cwd);

        let mut command = tokio::process::Command::new(&binary);
        command
            .args(&args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Null stdin: we never write to it, and an open stdin can stall a
            // harness on an interactive read or a pipe-full deadlock.
            .stdin(Stdio::null())
            .kill_on_drop(true);
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
            harness_version,
            mcp_servers,
            skills_list,
            options.cancel_token,
        ));

        Ok(Box::pin(UnboundedReceiverStream::new(rx)))
    }
}

fn resolve_home_dir(override_path: Option<&Path>) -> PathBuf {
    if let Some(path) = override_path {
        return path.to_owned();
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn fetch_version(binary: &Path) -> String {
    let Ok(resolved) = crate::subprocess::resolve_binary(binary) else {
        return String::new();
    };
    let output = std::process::Command::new(&resolved)
        .arg("--version")
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_owned(),
        _ => String::new(),
    }
}

/// Build the `gemini` invocation. Order matches the captured fixture shape
/// so future-CLI flag-order assertions can pin against it.
fn build_args(agent: &AgentRecord, prompt: &str, cwd: &Path, home_dir: &Path) -> Vec<String> {
    let mut args = vec![
        "-p".to_owned(),
        prompt.to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
    ];
    if let Some(session_id) = agent.session_id {
        if session_file::session_file_exists_for(home_dir, cwd, &session_id) {
            args.push("--resume".to_owned());
        } else {
            args.push("--session-id".to_owned());
        }
        args.push(session_id.to_string());
    }
    args.push("--yolo".to_owned());
    args.push("--skip-trust".to_owned());
    args
}

// Parallels `ClaudeCodeAdapter::run_producer` / `CodexAdapter::run_producer`.
// Splitting further would fragment the per-line control flow without
// improving readability.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_producer(
    mut child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    tx: tokio::sync::mpsc::UnboundedSender<AdapterEvent>,
    turn_id: TurnId,
    agent_id: AgentId,
    harness_version: String,
    mcp_servers: Vec<crate::events::McpServerStatus>,
    skills_list: Vec<String>,
    cancel_token: CancellationToken,
) {
    let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::with_capacity(
        crate::subprocess::STDERR_TAIL_CAPACITY,
    )));
    let stderr_task = tokio::spawn(crate::subprocess::drain_stderr(
        stderr,
        agent_id,
        turn_id,
        Arc::clone(&stderr_tail),
        "gemini",
    ));

    let mut terminal_seen = false;
    let mut terminal_was_completed = false;
    // Set when cancellation fires: kill the group and end the stream with NO
    // terminal event (the dispatcher synthesizes `Cancelled`). Distinct from
    // `force_kill_child`, which is an error path that still emits a `Failed`
    // terminal.
    let mut cancelled = false;
    let mut state = GeminiParserState::default();
    // Set on any error path that ends the producer loop with the subprocess
    // still potentially running. Mirrors Codex's pattern; load-bearing for
    // Gemini specifically because Gemini ignores bare-PID SIGTERM and
    // requires process-group signaling (see docs/research/archive/gemini-cli-observed.md,
    // "SIGTERM behaviour" findings). Without the explicit kill, a Gemini
    // process that keeps writing after we stop draining stdout would block
    // on a full pipe and `child.wait()` would hang indefinitely, delaying
    // `AgentIdle` and stranding the UI in an in-flight state.
    let mut force_kill_child = false;

    let mut lines = tokio::io::BufReader::new(stdout).lines();

    loop {
        // `select!` over the read AND the cancellation token so a parked read
        // (Gemini buffering its whole response) still notices a cancel.
        let line = tokio::select! {
            line = lines.next_line() => line,
            () = cancel_token.cancelled() => {
                cancelled = true;
                break;
            }
        };
        match line {
            Ok(Some(line)) => {
                let outcome = parse_line(&line, turn_id, agent_id, &mut state);
                let events = match outcome {
                    crate::parser::ParseOutcome::Event(event) => vec![event],
                    crate::parser::ParseOutcome::Events(events) => events,
                    crate::parser::ParseOutcome::Skip => continue,
                    crate::parser::ParseOutcome::Error(msg) => {
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
                        force_kill_child = true;
                        break;
                    }
                };
                for event in events {
                    match &event {
                        AdapterEvent::SessionMeta { .. } => {
                            // The parser knows only what the stream's
                            // `init` event carries (model). The adapter
                            // owns `harness_version` (lazy `gemini --version`
                            // fetch) and the per-dispatch MCP / skills
                            // registries (loaded from settings.json /
                            // ~/.agents/skills). Inject all three before
                            // forwarding so the sidebar populates
                            // immediately.
                            let event = inject_session_meta_fields(
                                event,
                                &harness_version,
                                &mcp_servers,
                                &skills_list,
                            );
                            let _ = tx.send(event);
                            continue;
                        }
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
            Ok(None) => break,
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
                force_kill_child = true;
                break;
            }
        }
    }

    if cancelled {
        // Cancellation path: kill the group and end the stream with NO
        // terminal event (the dispatcher synthesizes `Cancelled`). Kill before
        // awaiting the stderr drain — a parked subprocess holds stderr open.
        crate::subprocess::terminate_then_kill(&mut child).await;
        let _ = stderr_task.await;
        return;
    }

    if force_kill_child {
        crate::subprocess::terminate_then_kill(&mut child).await;
    }

    let _ = stderr_task.await;

    // Reap and synthesize. The exit code lets us distinguish:
    //   - Exit 42 + empty stdout → input rejection; classify the first
    //     non-noise stderr line.
    //   - Anything else without a terminal `result` → AdapterFailure
    //     (cancellation, crash).
    let exit_status = child.wait().await;
    if !terminal_seen {
        let _ = tx.send(synthesize_terminal_failure(
            turn_id,
            &stderr_tail,
            exit_status
                .as_ref()
                .ok()
                .and_then(std::process::ExitStatus::code),
        ));
    } else if let Ok(status) = exit_status
        && !status.success()
        && terminal_was_completed
    {
        tracing::warn!(
            %turn_id,
            agent_id = %agent_id,
            exit_code = ?status.code(),
            "gemini emitted result:success but subprocess exited non-zero — log-only"
        );
    }
}

/// Overlay the runtime-known fields onto a parser-emitted `SessionMeta`.
/// The parser knows only `model` (from the stream's `init` event); the
/// adapter owns `harness_version` (lazy version probe), `mcp_servers`,
/// and `skills` (loaded from settings.json / ~/.agents/skills). In-place
/// mutation so a future field added to `SessionMeta` doesn't get
/// silently dropped to its default.
fn inject_session_meta_fields(
    mut event: AdapterEvent,
    version: &str,
    mcp: &[crate::events::McpServerStatus],
    skills_list: &[String],
) -> AdapterEvent {
    if let AdapterEvent::SessionMeta {
        harness_version,
        mcp_servers,
        skills,
        ..
    } = &mut event
    {
        version.clone_into(harness_version);
        mcp.clone_into(mcp_servers);
        skills_list.clone_into(skills);
    }
    event
}

/// Synthesize the terminal `TurnEnd(Failed)` event for the no-terminal-seen
/// EOF case. Exit 42 with a recognizable stderr line → `AuthFailure` if
/// the message matches the shared auth-substring set, else `AdapterFailure`
/// (exit-42 is input rejection — not the model's fault, so `HarnessError`
/// would be wrong). Everything else falls through to `AdapterFailure` with
/// the stderr tail attached.
fn synthesize_terminal_failure(
    turn_id: TurnId,
    stderr_tail: &Mutex<VecDeque<String>>,
    exit_code: Option<i32>,
) -> AdapterEvent {
    if exit_code == Some(42) {
        let actionable = extract_actionable_stderr(stderr_tail);
        if !actionable.is_empty() {
            let kind = if is_gemini_auth_failure_message(&actionable) {
                FailureKind::AuthFailure
            } else {
                FailureKind::AdapterFailure
            };
            return AdapterEvent::TurnEnd {
                turn_id,
                outcome: TurnOutcome::Failed {
                    kind,
                    message: actionable,
                },
                ended_at: Utc::now(),
                usage: None,
            };
        }
    }
    let tail = crate::subprocess::format_stderr_tail(stderr_tail);
    let message = if tail.is_empty() {
        "gemini exited without terminal result event (no stderr captured)".to_owned()
    } else {
        format!("gemini exited without terminal result event; stderr: {tail}")
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

/// Return the first stderr line that isn't a known noise prefix. Empty
/// string if every line is filtered or the buffer is empty.
fn extract_actionable_stderr(stderr_tail: &Mutex<VecDeque<String>>) -> String {
    let Ok(buf) = stderr_tail.lock() else {
        return String::new();
    };
    for line in buf.iter() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if GEMINI_STDERR_NOISE_PREFIXES
            .iter()
            .any(|p| trimmed.starts_with(p))
        {
            continue;
        }
        return trimmed.to_owned();
    }
    String::new()
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
            name: "g".to_owned(),
            harness: HarnessKind::Gemini,
            session_id: Some(session_id),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn build_args_first_turn_uses_session_id_flag() {
        let home = tempfile::TempDir::new().unwrap();
        let cwd = tempfile::TempDir::new().unwrap();
        let agent = agent_with_session(Uuid::new_v4());
        let args = build_args(&agent, "hi", cwd.path(), home.path());

        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "hi");
        assert_eq!(args[2], "--output-format");
        assert_eq!(args[3], "stream-json");
        assert!(args.contains(&"--session-id".to_owned()));
        assert!(!args.contains(&"--resume".to_owned()));
        assert!(args.contains(&"--yolo".to_owned()));
        assert!(args.contains(&"--skip-trust".to_owned()));
    }

    #[test]
    fn build_args_subsequent_turn_uses_resume_flag() {
        let home = tempfile::TempDir::new().unwrap();
        let cwd = tempfile::TempDir::new().unwrap();
        let session_id = Uuid::new_v4();
        let agent = agent_with_session(session_id);

        // Pre-create the project mapping + a session file so the existence
        // check sees the prefix-matched file. `projects.json` is keyed by
        // the canonical cwd (Gemini canonicalizes before writing; the
        // helper canonicalizes before reading).
        let gemini = home.path().join(".gemini");
        std::fs::create_dir_all(&gemini).unwrap();
        let canonical_cwd = cwd.path().canonicalize().unwrap();
        let cwd_str = canonical_cwd.to_str().unwrap();
        std::fs::write(
            gemini.join("projects.json"),
            format!(r#"{{"projects":{{"{cwd_str}":"proj"}}}}"#),
        )
        .unwrap();
        let chats = gemini.join("tmp").join("proj").join("chats");
        std::fs::create_dir_all(&chats).unwrap();
        let prefix = session_file::id_prefix(&session_id);
        std::fs::write(
            chats.join(format!("session-2026-05-17T22-11-{prefix}.jsonl")),
            "",
        )
        .unwrap();

        let args = build_args(&agent, "hi", cwd.path(), home.path());
        assert!(
            args.contains(&"--resume".to_owned()),
            "expected --resume when session file exists; got {args:?}"
        );
        assert!(!args.contains(&"--session-id".to_owned()));
    }

    #[test]
    fn probe_reports_missing_binary_for_absolute_path() {
        let adapter = GeminiAdapter::with_binary_path("/nonexistent/path/to/gemini");
        assert!(matches!(
            adapter.probe(),
            Err(DispatchError::BinaryNotFound)
        ));
    }

    #[test]
    fn probe_reports_missing_binary_for_relative_name() {
        let adapter = GeminiAdapter::with_binary_path("this-binary-does-not-exist-on-PATH-xyz123");
        assert!(matches!(
            adapter.probe(),
            Err(DispatchError::BinaryNotFound)
        ));
    }

    #[tokio::test]
    async fn dispatch_rejects_empty_prompt_before_spawn() {
        let adapter = GeminiAdapter::with_binary_path("/nonexistent");
        let agent = agent_with_session(Uuid::new_v4());
        let result = adapter
            .dispatch(
                &agent,
                std::path::Path::new("/tmp"),
                "   ",
                Uuid::now_v7(),
                crate::DispatchOptions::default(),
            )
            .await;
        assert!(matches!(result, Err(DispatchError::InvalidPrompt(_))));
    }

    fn tail_with(lines: &[&str]) -> Mutex<VecDeque<String>> {
        let mut buf = VecDeque::new();
        for line in lines {
            buf.push_back((*line).to_owned());
        }
        Mutex::new(buf)
    }

    #[test]
    fn extract_actionable_stderr_skips_noise_lines() {
        let tail = tail_with(&[
            "YOLO mode is enabled. All tool calls will be automatically approved.",
            "Ripgrep is not available. Falling back to GrepTool.",
            "Error when talking to Gemini API: ModelNotFoundError",
        ]);
        assert_eq!(
            extract_actionable_stderr(&tail),
            "Error when talking to Gemini API: ModelNotFoundError"
        );
    }

    #[test]
    fn extract_actionable_stderr_returns_empty_when_all_noise() {
        let tail = tail_with(&[
            "YOLO mode is enabled. All tool calls will be automatically approved.",
            "Approval mode overridden to \"default\".",
        ]);
        assert_eq!(extract_actionable_stderr(&tail), "");
    }

    #[test]
    fn synthesize_terminal_failure_classifies_exit42_auth_substring_as_auth_failure() {
        let tail = tail_with(&["API returned 401 Unauthorized"]);
        let event = synthesize_terminal_failure(Uuid::now_v7(), &tail, Some(42));
        match event {
            AdapterEvent::TurnEnd { outcome, .. } => {
                let TurnOutcome::Failed { kind, .. } = outcome else {
                    panic!("expected Failed");
                };
                assert_eq!(kind, FailureKind::AuthFailure);
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn synthesize_terminal_failure_classifies_exit42_non_auth_as_adapter_failure() {
        let tail = tail_with(&["Error resuming session: Invalid session identifier \"x\"."]);
        let event = synthesize_terminal_failure(Uuid::now_v7(), &tail, Some(42));
        match event {
            AdapterEvent::TurnEnd { outcome, .. } => {
                let TurnOutcome::Failed { kind, message } = outcome else {
                    panic!("expected Failed");
                };
                assert_eq!(kind, FailureKind::AdapterFailure);
                assert!(message.contains("Invalid session identifier"));
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn synthesize_terminal_failure_no_exit42_classifies_as_adapter_failure_with_tail() {
        let tail = tail_with(&["random stderr line"]);
        let event = synthesize_terminal_failure(Uuid::now_v7(), &tail, Some(0));
        match event {
            AdapterEvent::TurnEnd { outcome, .. } => {
                let TurnOutcome::Failed { kind, message } = outcome else {
                    panic!("expected Failed");
                };
                assert_eq!(kind, FailureKind::AdapterFailure);
                assert!(message.contains("random stderr line"));
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
    }
}
