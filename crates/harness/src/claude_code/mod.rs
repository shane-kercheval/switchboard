pub mod config;
pub(crate) mod facets;
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
    /// display-only). Mirrors the Antigravity pattern.
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
        //
        // Fail closed unless this Claude agent has a Uuid session locator.
        // Without one `build_args` emits no session flags at all, so claude
        // mints its own session id — one Switchboard never learns. The turn
        // looks successful and every later send silently starts a fresh
        // session, so the agent quietly loses its memory forever.
        //
        // The guard covers a *missing* locator as well as a fork's, because
        // both produce that identical silent failure. (Locator-`None` is a
        // legitimate pre-first-turn state for Codex/Antigravity — but no other
        // harness reaches this adapter, and `Project::register_agent` always
        // pre-mints a locator for Claude, so here it only means a corrupted
        // registry.) Unreachable via core's APIs; this is the boundary that
        // keeps corruption loud instead of silent.
        if !matches!(agent.session_locator, Some(SessionLocator::Uuid(_))) {
            return Err(DispatchError::InvalidAgentState(format!(
                "Claude agent {} has no session locator{} — refusing to dispatch \
                 a turn that would start an untracked session",
                agent.id,
                if agent.forked_from_session.is_some() {
                    " (and carries fork provenance)"
                } else {
                    ""
                }
            )));
        }
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
            agent.effort.clone(),
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
        match (exists, agent.forked_from_session) {
            // The agent's own session exists: an ordinary resume. A forked
            // agent takes this branch for every turn after its first, so the
            // fork flags appear exactly once in its lifetime even though the
            // provenance field is never cleared.
            (true, _) => {
                args.push("--resume".to_owned());
                args.push(session_id.to_string());
            }
            // An unmaterialized fork: resume the PARENT and branch, landing the
            // branch on this agent's own pre-generated id. Claude enforces this
            // exact trio — `--session-id` alongside `--resume`/`--continue`
            // without `--fork-session` aborts with "--session-id can only be
            // used with --continue or --resume if --fork-session is also
            // specified" (2.1.226).
            //
            // Deriving fork-vs-resume from file existence (rather than
            // consuming a flag) is what makes this idempotent: if this dispatch
            // dies before Claude creates the file, the next send retries the
            // fork; once the file exists, the arm above takes over permanently.
            // Nothing to persist, nothing to roll back. Caveat: that reasoning
            // treats the file as present-or-absent — a *truncated* file (killed
            // mid-copy) reads as present. See harness-behavior.md §3.5 for
            // whether that state is reachable.
            (false, Some(parent_session)) => {
                args.push("--resume".to_owned());
                args.push(parent_session.to_string());
                args.push("--session-id".to_owned());
                args.push(session_id.to_string());
                args.push("--fork-session".to_owned());
            }
            // First turn of an ordinary agent: create the session under our id.
            (false, None) => {
                args.push("--session-id".to_owned());
                args.push(session_id.to_string());
            }
        }
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
    args.push(claude_transport_prompt(prompt));
    args
}

/// The exact text handed to `claude -p` for a given dispatch prompt.
///
/// Claude's headless CLI still routes a bare slash-leading positional through
/// its interactive command parser (`/plugin`, `/context`, unknown commands), so
/// the model may never see a plain Switchboard message. A single leading ASCII
/// space bypasses that parser without changing the message's meaning. This lives
/// at the adapter boundary: the journal and frontend retain the user's exact
/// text, and every dispatch source (compose, prompt, workflow, forward) gets the
/// same literal-message contract.
///
/// **Public because the transcript merge must reproduce it.** Correlating a
/// journaled send against the prompt Claude recorded is an exact string
/// comparison, and the session file holds *this* text, not the journal's. A
/// second, drifting copy of the rule in the merge is precisely the bug that
/// motivated exporting it — the merge calls this instead of guessing at
/// normalization.
#[must_use]
pub fn claude_transport_prompt(prompt: &str) -> String {
    if prompt.starts_with('/') {
        format!(" {prompt}")
    } else {
        prompt.to_owned()
    }
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
/// encoded cwd replaces every character outside `[A-Za-z0-9-]` with `-` (see
/// [`encode_cwd`]). For example, `/Users/x/repo/.switchboard/projects/<id>` is
/// encoded as `-Users-x-repo--switchboard-projects-<id>` — the leading dot of
/// `.switchboard` becomes a dash, producing the double-dash `--switchboard`.
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

/// Longest encoded directory name Claude Code emits before it truncates and
/// appends a hash of the full path (`sQ` in the CLI bundle).
const MAX_ENCODED_LEN: usize = 200;

/// Encodes a canonical absolute path the way Claude Code does for its
/// session-storage directory naming.
///
/// Getting this rule exactly right is load-bearing: any mismatch causes the
/// adapter to think a session file is missing and pass `--session-id`, which
/// claude rejects with "Session ID … is already in use" on subsequent turns,
/// stranding the agent permanently (the check is a filesystem lookup, so it
/// fails identically on every retry and across app restarts).
///
/// The CLI's rule, read out of the 2.1.226 bundle:
///
/// ```js
/// bes = e => e.replace(/[^a-zA-Z0-9]/g, "-")
/// gw  = e => { let t = bes(e); return t.length <= sQ ? t
///              : `${t.slice(0, sQ)}-${T$g(e)}` }          // sQ = 200
/// T$g = e => Math.abs(xdt(e)).toString(36)
/// xdt = e => { let t = 0; for (…) t = (t << 5) - t + e.charCodeAt(r) | 0; return t }
/// ```
///
/// Three details that a "replace the separators" reading misses, each of which
/// produces the permanent-strand failure above:
///
/// - **Every** non-alphanumeric collapses, not just `/` and `.` — `_` and
///   spaces included, so `switchboard-mcp_oauth` lives under `…-mcp-oauth`.
/// - The regex has no `u` flag, so it runs over **UTF-16 code units**: an
///   astral character (emoji) becomes *two* dashes, not one. Hence
///   `encode_utf16` rather than `chars`.
/// - Names longer than 200 characters are truncated and suffixed with a
///   base-36 Java-style string hash **of the untruncated path**.
///
/// Known remaining divergence: the CLI NFC-normalizes the path first, and this
/// does not. A decomposed (NFD) path — which macOS produces routinely — still
/// resolves to the wrong directory. Closing that needs a Unicode-normalization
/// dependency; see the note in `docs/harness-behavior.md`.
fn encode_cwd(canonical: &Path) -> String {
    let raw = canonical.to_string_lossy();
    let encoded: String = raw
        .encode_utf16()
        .map(|unit| match u8::try_from(unit) {
            Ok(byte) if byte.is_ascii_alphanumeric() => char::from(byte),
            _ => '-',
        })
        .collect();
    if encoded.len() <= MAX_ENCODED_LEN {
        return encoded;
    }
    // `encoded` is all ASCII, so byte-slicing at 200 is char-safe and matches
    // the CLI's `slice(0, 200)` over UTF-16 units.
    format!(
        "{}-{}",
        &encoded[..MAX_ENCODED_LEN],
        java_string_hash_base36(&raw)
    )
}

/// `Math.abs(xdt(path)).toString(36)` — the suffix Claude Code appends to a
/// truncated directory name. `xdt` is the classic `h = h * 31 + c` string hash
/// evaluated over UTF-16 code units with JavaScript's `| 0` int32 truncation at
/// every step, so every operation here wraps.
fn java_string_hash_base36(raw: &str) -> String {
    let hash = raw.encode_utf16().fold(0i32, |acc, unit| {
        acc.wrapping_shl(5)
            .wrapping_sub(acc)
            .wrapping_add(i32::from(unit))
    });
    // `Math.abs(-2147483648)` is 2147483648 in JS (the result is a double, not
    // an int32), which `unsigned_abs` reproduces exactly.
    to_base36(hash.unsigned_abs())
}

fn to_base36(mut value: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_owned();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(char::from(DIGITS[(value % 36) as usize]));
        value /= 36;
    }
    out.iter().rev().collect()
}

// Parallels the Codex producer: a single per-line control-flow loop
// plus the cancel and post-loop terminal handling. Splitting it would fragment
// that flow without improving readability.
// Arg count matches the Codex producer, which carries the same allow:
// the params are independent handles (child, pipes, tx, ids, dispatched effort,
// cancel token) with no meaningful grouping — bundling them into a struct here
// would add a type without removing a decision.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_producer(
    mut child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    tx: tokio::sync::mpsc::UnboundedSender<AdapterEvent>,
    turn_id: TurnId,
    agent_id: AgentId,
    dispatched_effort: Option<String>,
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
    // Set when the cancellation token fires. On cancel the adapter kills the
    // subprocess group and ends the stream WITHOUT a terminal event — the
    // dispatcher synthesizes `TurnEnd { Cancelled { source } }` (it owns the
    // cancel outcome; a binary token can't carry the source). So the cancel
    // path must skip the truncation synthesis below.
    let mut cancelled = false;
    let mut parser_state = ParserState::with_dispatched_effort(dispatched_effort.clone());

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
                        let outcome = TurnOutcome::Failed {
                            kind: FailureKind::AdapterFailure,
                            message: format!("malformed JSON from harness: {msg}"),
                        };
                        // Preserve already-folded whole-dispatch telemetry
                        // (usage/spend/ids) when cycles completed before the
                        // glitch — same rule as the dirty-exit gate below;
                        // bare terminal only when nothing was folded.
                        let event = parser_state
                            .take_final_turn_end(turn_id, outcome.clone())
                            .unwrap_or_else(|| AdapterEvent::TurnEnd {
                                turn_id,
                                outcome,
                                ended_at: Utc::now(),
                                usage: None,
                                context_window_source: None,
                                stable_message_id: None,
                                first_message_id: parser_state
                                    .first_assistant_message_id()
                                    .map(str::to_owned),
                                spend: None,
                                model: None,
                                effort: dispatched_effort.clone(),
                            });
                        let _ = tx.send(event);
                        terminal_seen = true;
                        break;
                    }
                };
                for event in events {
                    // Only failure terminals arrive mid-stream (error result,
                    // auth failure, malformed JSON): a successful `result`
                    // folds into parser state instead, because a
                    // background-agent dispatch emits one `result` per
                    // internal cycle with irregular timing and only the
                    // stream ending reliably bounds the turn. The Completed
                    // terminal is emitted at stdout EOF below, gated on the
                    // child's exit status.
                    if matches!(&event, AdapterEvent::TurnEnd { .. }) {
                        terminal_seen = true;
                    }
                    let _ = tx.send(event);
                }
                if terminal_seen {
                    break;
                }
            }
            Ok(None) => break, // stdout EOF
            Err(e) => {
                let outcome = TurnOutcome::Failed {
                    kind: FailureKind::AdapterFailure,
                    message: format!("stdout read error: {e}"),
                };
                // Same folded-telemetry preservation as the malformed-JSON arm.
                let event = parser_state
                    .take_final_turn_end(turn_id, outcome.clone())
                    .unwrap_or_else(|| AdapterEvent::TurnEnd {
                        turn_id,
                        outcome,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: parser_state
                            .first_assistant_message_id()
                            .map(str::to_owned),
                        spend: None,
                        model: None,
                        effort: dispatched_effort.clone(),
                    });
                let _ = tx.send(event);
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

    if terminal_seen {
        // A failure terminal was already emitted mid-stream; just reap.
        if let Err(e) = child.wait().await {
            tracing::warn!(
                %turn_id,
                agent_id = %agent_id,
                error = %e,
                "failed to wait on harness subprocess"
            );
        }
        return;
    }

    // Stdout EOF with no terminal emitted: this is the happy-path terminal
    // point. Reap FIRST — the terminal outcome is gated on the exit status,
    // because a folded intermediate `result` is not proof the dispatch
    // finished: a kill between background-agent cycles leaves a Completed
    // stash behind, and emitting it would silently feed forwards/workflows a
    // partial answer as authoritative. Stdout has already closed, so exit is
    // imminent and this wait adds no meaningful latency (~0.5s measured,
    // including Stop-hook teardown — see harness-behavior.md §6 @ 2.1.198).
    let wait_result = child.wait().await;
    let outcome = match &wait_result {
        Ok(status) if status.success() => TurnOutcome::Completed,
        Ok(status) => {
            let exit_desc = status.code().map_or_else(
                || "was killed by a signal".to_owned(),
                |code| format!("exited with code {code}"),
            );
            let stderr_msg = crate::subprocess::format_stderr_tail(&stderr_tail);
            let message = if stderr_msg.is_empty() {
                format!("harness {exit_desc} after an intermediate result — dispatch incomplete")
            } else {
                format!(
                    "harness {exit_desc} after an intermediate result — dispatch incomplete; stderr: {stderr_msg}"
                )
            };
            TurnOutcome::Failed {
                kind: FailureKind::HarnessError,
                message,
            }
        }
        Err(e) => {
            tracing::warn!(
                %turn_id,
                agent_id = %agent_id,
                error = %e,
                "failed to wait on harness subprocess"
            );
            TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message: format!("failed to reap harness subprocess: {e}"),
            }
        }
    };
    // The folded final `result` (whole-dispatch telemetry) with the gated
    // outcome; when no successful result was ever folded, the stream contract
    // still guarantees exactly one terminal via truncation synthesis.
    let event = parser_state
        .take_final_turn_end(turn_id, outcome)
        .unwrap_or_else(|| {
            synthesize_truncation_turn_end(
                turn_id,
                &stderr_tail,
                parser_state.first_assistant_message_id().map(str::to_owned),
                dispatched_effort.clone(),
            )
        });
    let _ = tx.send(event);
}

/// Build the synthesized `TurnEnd(Failed)` event emitted when stdout EOFs
/// without a terminal `result` event. Includes the captured stderr tail so
/// the consumer can see the underlying cause (auth error, flag rejection).
fn synthesize_truncation_turn_end(
    turn_id: TurnId,
    stderr_tail: &Mutex<VecDeque<String>>,
    first_message_id: Option<String>,
    dispatched_effort: Option<String>,
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
        // Carry the dedup identity if the turn produced any assistant message
        // before crashing — so its `Failed` row matches the on-disk copy.
        first_message_id,
        spend: None,
        model: None,
        // A failed turn still ran at the dispatched effort — carry it so the
        // Failed row matches the on-disk copy the way `model` does.
        effort: dispatched_effort,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchboard_core::HarnessKind;
    use uuid::Uuid;

    fn agent_with_session(session_id: Uuid) -> AgentRecord {
        AgentRecord {
            session_home: None,
            model: None,
            effort: None,
            profiles: switchboard_core::AgentProfiles::default(),
            forked_from_session: None,
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            name: "test".to_owned(),
            harness: HarnessKind::ClaudeCode,
            session_locator: Some(SessionLocator::Uuid(session_id)),
            created_at: chrono::Utc::now(),
        }
    }

    /// The exact shape that stranded a real agent: a cwd containing `_`, with
    /// the session file where Claude Code actually writes it. Before the fix
    /// this picked `--session-id`, which claude rejects with "Session ID … is
    /// already in use", failing every turn forever.
    #[test]
    fn build_args_resumes_when_the_cwd_contains_an_underscore() {
        let home = tempfile::TempDir::new().unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("switchboard-mcp_oauth");
        std::fs::create_dir_all(&project).unwrap();
        let canonical = project.canonicalize().unwrap();
        let session_id = Uuid::now_v7();

        // Directory name spelled out the way Claude Code spells it, rather than
        // via `encode_cwd` — otherwise this test cannot fail.
        let encoded: String = canonical
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        assert!(
            encoded.contains("switchboard-mcp-oauth"),
            "fixture must exercise the underscore case, got {encoded}"
        );
        let session_dir = home.path().join(".claude").join("projects").join(&encoded);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join(format!("{session_id}.jsonl")), "").unwrap();

        let agent = agent_with_session(session_id);
        let args = build_args(&agent, "hi", &canonical, Some(home.path()));

        assert!(args.contains(&"--resume".to_owned()));
        assert!(!args.contains(&"--session-id".to_owned()));
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

    /// Write an empty session file where Claude Code would put it, so
    /// `build_args` sees the agent's session as materialized.
    fn materialize_session(home: &Path, cwd: &Path, session_id: Uuid) {
        let canonical = cwd.canonicalize().unwrap();
        let session_dir = home
            .join(".claude")
            .join("projects")
            .join(encode_cwd(&canonical));
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join(format!("{session_id}.jsonl")), "").unwrap();
    }

    /// Index of `flag` in `args`, asserting it appears exactly once.
    fn flag_at(args: &[String], flag: &str) -> usize {
        let hits: Vec<usize> = args
            .iter()
            .enumerate()
            .filter(|(_, a)| a.as_str() == flag)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(hits.len(), 1, "expected one {flag} in {args:?}");
        hits[0]
    }

    #[test]
    fn build_args_forks_from_the_parent_on_an_unmaterialized_forks_first_turn() {
        // The whole contract in one assertion set: resume the PARENT, land on
        // OUR id, and carry `--fork-session` (which claude requires whenever
        // `--session-id` accompanies `--resume`).
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let parent_session = Uuid::now_v7();
        let own_session = Uuid::now_v7();
        let mut agent = agent_with_session(own_session);
        agent.forked_from_session = Some(parent_session);

        let args = build_args(&agent, "hi", project.path(), Some(home.path()));

        assert_eq!(
            args[flag_at(&args, "--resume") + 1],
            parent_session.to_string()
        );
        assert_eq!(
            args[flag_at(&args, "--session-id") + 1],
            own_session.to_string()
        );
        assert!(args.contains(&"--fork-session".to_owned()), "{args:?}");
        // Every flag must precede the end-of-options separator, or it lands as
        // a positional alongside the prompt.
        assert!(
            flag_at(&args, "--fork-session") < flag_at(&args, "--"),
            "{args:?}"
        );
    }

    #[test]
    fn build_args_resumes_own_session_once_a_fork_has_materialized() {
        // `forked_from_session` is never cleared, so "don't re-fork" rests
        // entirely on the file check. If this regressed, every turn after the
        // first would re-branch the parent — silently discarding the fork's own
        // history on each send.
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let parent_session = Uuid::now_v7();
        let own_session = Uuid::now_v7();
        let mut agent = agent_with_session(own_session);
        agent.forked_from_session = Some(parent_session);
        materialize_session(home.path(), project.path(), own_session);

        let args = build_args(&agent, "hi", project.path(), Some(home.path()));

        assert_eq!(
            args[flag_at(&args, "--resume") + 1],
            own_session.to_string()
        );
        assert!(!args.contains(&"--fork-session".to_owned()), "{args:?}");
        assert!(!args.contains(&"--session-id".to_owned()), "{args:?}");
        assert!(!args.contains(&parent_session.to_string()), "{args:?}");
    }

    #[test]
    fn build_args_retries_the_fork_when_the_first_dispatch_left_no_file() {
        // Self-healing: a first dispatch that died before claude created the
        // file is indistinguishable from never having dispatched, so the next
        // send forks again rather than resuming a session that doesn't exist.
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let parent_session = Uuid::now_v7();
        let mut agent = agent_with_session(Uuid::now_v7());
        agent.forked_from_session = Some(parent_session);

        let first = build_args(&agent, "hi", project.path(), Some(home.path()));
        let retry = build_args(&agent, "hi", project.path(), Some(home.path()));

        assert_eq!(first, retry);
        assert!(retry.contains(&"--fork-session".to_owned()), "{retry:?}");
    }

    #[test]
    fn build_args_omits_fork_flags_for_a_non_forked_agent() {
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent = agent_with_session(session_id);

        let args = build_args(&agent, "hi", project.path(), Some(home.path()));

        assert!(!args.contains(&"--fork-session".to_owned()), "{args:?}");
        assert_eq!(
            args[flag_at(&args, "--session-id") + 1],
            session_id.to_string()
        );
        assert!(!args.contains(&"--resume".to_owned()), "{args:?}");
    }

    #[test]
    fn build_args_carries_model_and_effort_on_the_fork_dispatch() {
        // The per-agent selection rides the fork turn like any other, and still
        // lands before the `--` separator.
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let mut agent = agent_with_session(Uuid::now_v7());
        agent.forked_from_session = Some(Uuid::now_v7());
        agent.model = Some("opus".to_owned());
        agent.effort = Some("high".to_owned());

        let args = build_args(&agent, "hi", project.path(), Some(home.path()));

        assert_eq!(args[flag_at(&args, "--model") + 1], "opus");
        assert_eq!(args[flag_at(&args, "--effort") + 1], "high");
        assert!(flag_at(&args, "--model") < flag_at(&args, "--"), "{args:?}");
    }

    #[test]
    fn build_args_ignores_fork_provenance_without_a_locator() {
        // Documents (not endorses) the arg-layer behavior: no session flags,
        // like any locator-less agent. The real boundary is `dispatch`, which
        // fails closed on this record shape before `build_args` ever runs —
        // see `dispatch_rejects_fork_provenance_without_a_locator`. If that
        // guard were removed, this degrade would let claude mint an untracked
        // session id.
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let mut agent = agent_with_session(Uuid::now_v7());
        agent.session_locator = None;
        agent.forked_from_session = Some(Uuid::now_v7());

        let args = build_args(&agent, "hi", project.path(), Some(home.path()));

        assert!(!args.contains(&"--fork-session".to_owned()), "{args:?}");
        assert!(!args.contains(&"--resume".to_owned()), "{args:?}");
        assert!(!args.contains(&"--session-id".to_owned()), "{args:?}");
    }

    /// Fail-closed contract for a corrupted registry record. Spawning without a
    /// session locator *succeeds* — claude mints its own id — so the turn looks
    /// fine and continuity dies silently on the next send. Both shapes (with and
    /// without fork provenance) produce that identical failure, so both are
    /// refused. The guard fires before binary resolution, so no real claude is
    /// needed.
    async fn assert_dispatch_refused(agent: &AgentRecord) {
        let project = tempfile::TempDir::new().unwrap();
        let result = ClaudeCodeAdapter::new()
            .dispatch(
                agent,
                project.path(),
                "hi",
                Uuid::now_v7(),
                crate::DispatchOptions::default(),
            )
            .await;

        // `expect_err` needs `Debug` on the Ok side, which the event stream
        // doesn't have — match instead.
        match result {
            Err(DispatchError::InvalidAgentState(msg)) => {
                assert!(msg.contains(&agent.id.to_string()), "got: {msg}");
            }
            Err(other) => panic!("expected InvalidAgentState, got: {other:?}"),
            Ok(_) => panic!("a locator-less Claude agent must not dispatch"),
        }
    }

    #[tokio::test]
    async fn dispatch_rejects_fork_provenance_without_a_locator() {
        let mut agent = agent_with_session(Uuid::now_v7());
        agent.session_locator = None;
        agent.forked_from_session = Some(Uuid::now_v7());
        assert_dispatch_refused(&agent).await;
    }

    #[tokio::test]
    async fn dispatch_rejects_a_claude_agent_with_no_locator() {
        // Same silent-failure class as the fork case above — provenance is not
        // what makes it dangerous, so the guard does not require it.
        let mut agent = agent_with_session(Uuid::now_v7());
        agent.session_locator = None;
        assert_dispatch_refused(&agent).await;
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
    fn build_args_prefixes_slash_leading_prompt_for_literal_model_dispatch() {
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let agent = agent_with_session(Uuid::now_v7());

        for (prompt, transported) in [
            ("/plugin", " /plugin"),
            ("/code-review", " /code-review"),
            (
                "/shanekercheval/path/to/something is missing",
                " /shanekercheval/path/to/something is missing",
            ),
        ] {
            let args = build_args(&agent, prompt, project.path(), Some(home.path()));
            assert_eq!(args.last().map(String::as_str), Some(transported));
            assert_eq!(args[args.len() - 2], "--");
        }
    }

    #[test]
    fn build_args_preserves_non_slash_and_already_spaced_prompts() {
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let agent = agent_with_session(Uuid::now_v7());

        for prompt in ["plain message", " /plugin"] {
            let args = build_args(&agent, prompt, project.path(), Some(home.path()));
            assert_eq!(args.last().map(String::as_str), Some(prompt));
        }
    }

    #[test]
    fn build_args_omits_session_flags_when_locator_absent() {
        // Defensive: Claude agents always pre-mint a locator, but `build_args`
        // is a pure function — a `None` locator must omit both session flags
        // rather than emit a flag with no id. Documents, not endorses: this
        // arg shape would let claude mint an untracked session, which is why
        // `dispatch` refuses the record outright before `build_args` runs
        // (see `dispatch_rejects_a_claude_agent_with_no_locator`).
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let agent = AgentRecord {
            session_home: None,
            model: None,
            effort: None,
            profiles: switchboard_core::AgentProfiles::default(),
            forked_from_session: None,
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
    fn encode_cwd_replaces_every_non_alphanumeric_character_with_a_dash() {
        // The dot/slash cases were verified by running `claude` in each cwd
        // shape and inspecting the directory it created under
        // `~/.claude/projects/` — see `docs/research/archive/claude-code-cli-observed.md`.
        // That archived probe predates the underscore/space/astral cases below
        // and records the rule as "`/` and `.`", which is now known to be
        // incomplete; `docs/harness-behavior.md` carries the corrected rule.

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
        // Underscore — the case that stranded `switchboard-mcp_oauth` agents.
        // Dots and slashes are not special; every non-alphanumeric collapses.
        assert_eq!(
            encode_cwd(Path::new("/Users/x/switchboard-mcp_oauth")),
            "-Users-x-switchboard-mcp-oauth"
        );
        // Space, and several distinct separators adjacent in one component.
        assert_eq!(
            encode_cwd(Path::new("/Users/x/api copy")),
            "-Users-x-api-copy"
        );
        assert_eq!(encode_cwd(Path::new("/a-b/c.d_e f")), "-a-b-c-d-e-f");
        // An astral character is two UTF-16 code units, so it collapses to two
        // dashes — `.chars()` would produce one and miss the directory.
        assert_eq!(
            encode_cwd(Path::new("/Users/x/a\u{1F600}b")),
            "-Users-x-a--b"
        );
    }

    /// Expected values produced by running Claude Code's own `gw`/`bes`/`xdt`
    /// functions (lifted verbatim from the 2.1.226 bundle) in node, so this
    /// pins against the CLI's algorithm rather than against a restatement of
    /// our own. A path long enough to truncate is reachable in normal use: the
    /// `.switchboard/projects/<uuid>` cwd shape alone contributes 59 characters.
    #[test]
    fn encode_cwd_truncates_long_names_and_appends_the_cli_hash() {
        let long = format!("/Users/x/{}/proj_name", "verylongsegment".repeat(20));
        let encoded = encode_cwd(Path::new(&long));

        assert_eq!(
            encoded,
            "-Users-x-verylongsegmentverylongsegmentverylongsegmentverylongsegmentverylongsegmentverylongsegmentverylongsegmentverylongsegmentverylongsegmentverylongsegmentverylongsegmentverylongsegmentverylongseg-38h6iz"
        );
        assert_eq!(
            encoded.len(),
            MAX_ENCODED_LEN + 1 + "38h6iz".len(),
            "200 chars, a separator dash, then the base-36 hash"
        );
        // Short names are returned whole — the truncation branch must not fire.
        assert_eq!(encode_cwd(Path::new("/Users/x/repo")), "-Users-x-repo");
    }

    #[test]
    fn java_string_hash_matches_the_cli_for_edge_values() {
        // `Math.abs(xdt(""))` is 0, and `(0).toString(36)` is "0".
        assert_eq!(java_string_hash_base36(""), "0");
        assert_eq!(to_base36(0), "0");
        assert_eq!(to_base36(35), "z");
        assert_eq!(to_base36(36), "10");
        // i32::MIN has no positive counterpart; JS `Math.abs` yields 2147483648
        // because the result is a double. `unsigned_abs` must match, not panic
        // or wrap back to a negative.
        assert_eq!(to_base36(i32::MIN.unsigned_abs()), "zik0zk");
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
    fn synthesized_truncation_carries_dedup_identity_when_present() {
        // A crash mid-turn must still tag its `Failed` TurnEnd with the dedup
        // identity (the first assistant message id seen before the crash) so it
        // collapses against the on-disk copy instead of rendering a duplicate.
        let stderr: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
        let turn_id = Uuid::now_v7();

        let with_id =
            synthesize_truncation_turn_end(turn_id, &stderr, Some("msg_first".to_owned()), None);
        match with_id {
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Failed { .. },
                first_message_id,
                ..
            } => assert_eq!(first_message_id.as_deref(), Some("msg_first")),
            other => panic!("expected Failed TurnEnd, got {other:?}"),
        }

        // No assistant message before the crash → no identity (dedup falls back
        // to turn_id, as before).
        let without_id = synthesize_truncation_turn_end(turn_id, &stderr, None, None);
        match without_id {
            AdapterEvent::TurnEnd {
                first_message_id, ..
            } => assert_eq!(first_message_id, None),
            other => panic!("expected TurnEnd, got {other:?}"),
        }
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
