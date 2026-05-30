//! Codex adapter — spawns `codex exec --json` and maps the Codex stream-event
//! vocabulary to normalized events.
//!
//! Codex's stream vocabulary differs structurally from Claude Code's:
//! - No `envelope` wrapper around messages; events are flat top-level objects
//!   discriminated by `type`.
//! - Tool calls and text messages share an `item.started` / `item.completed`
//!   shape (Claude uses separate `assistant` / `user` envelopes for `tool_use`
//!   and `tool_result` blocks).
//! - Session id is captured from the first `thread.started` stream event, not
//!   pre-generated at agent registration. The per-agent session-link sidecar
//!   (see `sidecar.rs`) is the system-of-record for the captured `thread_id`.
//!
//! **Resume command-line asymmetry.** `codex exec resume` does **not**
//! accept `-C` / `--cd`; verified against codex-cli 0.130.0 via the
//! `--help` output and a live probe. The first-turn `codex exec` DOES
//! accept `-C`. cwd is set instead via
//! `tokio::process::Command::current_dir(cwd)` for both paths — Codex
//! inherits cwd from the parent process automatically.

pub mod config;
pub mod parser;
pub mod session_file;
pub mod sidecar;
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
use crate::events::{AdapterEvent, FailureKind, TurnId, TurnOutcome, TurnUsage};

use parser::{CodexParserState, parse_line};
use session_file::{Enrichment, TokioSleeper};
use sidecar::{SessionLinkRecord, append_record, read_latest, sidecar_path};

/// Adapter for Codex (`codex exec --json`). Spawns a `codex` subprocess and
/// maps the stream-event output to `AdapterEvent`s. Session continuity is
/// captured from the first `thread.started` stream event and persisted to a
/// per-agent JSONL sidecar (see [`sidecar`]).
///
/// For testing, construct with `with_binary_path(path)` pointing to the
/// `fake_codex` fixture binary — the adapter's behaviour is identical;
/// only the binary changes.
pub struct CodexAdapter {
    codex_binary_path: PathBuf,
    /// Optional override for the user's home directory. Used by tests to
    /// stage temp directories without mutating process-wide `$HOME`. In
    /// production this is `None` and the adapter resolves `$HOME` at
    /// dispatch time (mirrors `claude_code::session_file_exists`'s
    /// pattern).
    home_dir_override: Option<PathBuf>,
    /// Lazily-resolved `codex --version`, cached for the lifetime of the
    /// adapter. Empty string caches a failed/absent probe (version is
    /// display-only). Mirrors the Gemini/Antigravity pattern.
    cached_version: OnceLock<String>,
}

impl CodexAdapter {
    /// Production constructor. Uses `codex` from PATH; reads `$HOME` at
    /// dispatch time for session-file + config lookups.
    #[must_use]
    pub fn new() -> Self {
        Self {
            codex_binary_path: PathBuf::from("codex"),
            home_dir_override: None,
            cached_version: OnceLock::new(),
        }
    }

    /// Override the binary path — used by tests to inject the `fake_codex` fixture binary.
    pub fn with_binary_path(path: impl Into<PathBuf>) -> Self {
        Self {
            codex_binary_path: path.into(),
            home_dir_override: None,
            cached_version: OnceLock::new(),
        }
    }

    /// Override both the binary path and the home directory — used by
    /// fixture-driven adapter tests that stage a `TempDir` as `home_dir`
    /// so they can write into `<home>/.codex/sessions/...` without touching
    /// the developer's real `~/.codex/`.
    pub fn with_binary_and_home(
        binary_path: impl Into<PathBuf>,
        home_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            codex_binary_path: binary_path.into(),
            home_dir_override: Some(home_dir.into()),
            cached_version: OnceLock::new(),
        }
    }
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HarnessAdapter for CodexAdapter {
    fn probe(&self) -> Result<(), DispatchError> {
        crate::subprocess::probe_binary(&self.codex_binary_path)
    }

    fn version(&self) -> Option<String> {
        let raw = self.cached_version.get_or_init(|| {
            crate::subprocess::fetch_version(&self.codex_binary_path).unwrap_or_default()
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
        let binary = crate::subprocess::resolve_binary(&self.codex_binary_path)?;
        // Sidecar path: <cwd>/.switchboard/projects/<project-id>/sessions/<agent-id>.jsonl.
        // cwd here is the user's bound working directory (per send_message_impl
        // in crates/app/src/commands.rs), not the per-project metadata directory.
        let sidecar = sidecar_path(cwd, agent.project_id, agent.id);
        // SidecarError::Corrupt is the load-bearing case (fail-loud per
        // AGENTS.md). IO errors (including NotFound, though `read_latest`
        // already maps that to Ok(None)) also surface here as
        // PreStreamRead — uniform pre-stream failure on any read problem.
        let prior =
            read_latest(&sidecar).map_err(|e| DispatchError::PreStreamRead(e.to_string()))?;
        let args = build_args(prompt, prior.as_ref());

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
        crate::subprocess::apply_path_env(&mut command);
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
        let home_dir = resolve_home_dir(self.home_dir_override.as_deref());

        // SessionMeta-emission gate: normally first-dispatch-of-an-agent
        // (no prior sidecar). The attach flow pre-writes a sidecar at
        // attach time, so without this override the first post-attach
        // dispatch would be misclassified as a resume and SessionMeta
        // would never fire for the attached agent's sidebar. Caller signals
        // "treat this as first turn" via DispatchOptions.
        let force_session_meta = options.is_first_dispatch_after_attach;

        tokio::spawn(run_producer(
            child,
            stdout,
            stderr,
            tx,
            turn_id,
            agent_id,
            sidecar,
            prior,
            home_dir,
            cwd.to_owned(),
            force_session_meta,
            options.cancel_token,
        ));

        Ok(Box::pin(UnboundedReceiverStream::new(rx)))
    }
}

/// Resolve the home directory for session-file / config lookups. If the
/// adapter was constructed with an override (test path), use that
/// unconditionally; otherwise read `$HOME`. Returns an empty `PathBuf` if
/// `$HOME` is unset — `locate_session_file` and the config loaders both
/// degrade to empty/None on missing/unreadable inputs.
fn resolve_home_dir(override_path: Option<&Path>) -> PathBuf {
    if let Some(path) = override_path {
        return path.to_owned();
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Build the args for `codex exec [resume <id>]`. Flag set verified against
/// codex-cli 0.130.0; see the module-level docstring and
/// `docs/research/archive/codex-cli-observed.md` for the `-C`-on-resume rejection
/// finding.
fn build_args(prompt: &str, prior: Option<&SessionLinkRecord>) -> Vec<String> {
    if let Some(record) = prior {
        // Resume subcommand — NO `-C` (rejected by codex 0.130.0). cwd is
        // pinned via `Command::current_dir(cwd)` instead.
        vec![
            "exec".to_owned(),
            "resume".to_owned(),
            "--json".to_owned(),
            "--skip-git-repo-check".to_owned(),
            "--dangerously-bypass-approvals-and-sandbox".to_owned(),
            record.session_id.clone(),
            prompt.to_owned(),
        ]
    } else {
        vec![
            "exec".to_owned(),
            "--json".to_owned(),
            "--skip-git-repo-check".to_owned(),
            "--dangerously-bypass-approvals-and-sandbox".to_owned(),
            "-C".to_owned(),
            // The Command's `current_dir(cwd)` already pins the child's
            // process working directory; passing `-C cwd` is redundant but
            // matches the captured invocation shape for the first turn.
            // Using "." rather than the resolved cwd because cwd is already
            // set by Command::current_dir — "." is interpreted relative to
            // the child's pwd, which IS cwd. Avoids encoding the path twice.
            ".".to_owned(),
            prompt.to_owned(),
        ]
    }
}

// Parallel to `ClaudeCodeAdapter::run_producer` (which sits just under the
// `too_many_lines` threshold). The Codex variant adds sidecar persistence,
// EOF synthesis that consumes the buffered stdout error, and post-terminal
// session-file enrichment. Splitting further would fragment the per-line
// control flow without improving readability.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_producer(
    mut child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    tx: tokio::sync::mpsc::UnboundedSender<AdapterEvent>,
    turn_id: TurnId,
    agent_id: AgentId,
    sidecar_file: PathBuf,
    prior: Option<SessionLinkRecord>,
    home_dir: PathBuf,
    cwd: PathBuf,
    force_session_meta: bool,
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
        "codex",
    ));

    let mut terminal_seen = false;
    let mut terminal_was_completed = false;
    // Set when cancellation fires: kill the group and end the stream with NO
    // terminal event. Codex is the load-bearing case for token-driven (not
    // exit-code/terminal-event-driven) cancellation — it exits 0 on SIGTERM
    // and emits no terminal event, so only the dispatcher's synthesized
    // `Cancelled` (from the fired token) is authoritative.
    let mut cancelled = false;
    let mut state = CodexParserState::default();
    // Once we've written the sidecar for this dispatch, ignore any
    // subsequent thread.started events (defensive — Codex emits one per
    // dispatch in observed fixtures).
    let mut sidecar_written = false;
    // Set on any error path that ends the producer loop with the subprocess
    // still potentially running. The child is then killed before awaiting
    // `stderr_task` / `child.wait()` so the stream closes promptly and the
    // dispatcher's `AgentIdleGuard` releases at terminal time, not whenever
    // codex eventually decides to exit on its own.
    let mut force_kill_child = false;

    let mut lines = tokio::io::BufReader::new(stdout).lines();

    'lines: loop {
        // `select!` over the read AND the cancellation token so a parked read
        // still notices a cancel. Codex's two-process tree dies via `killpg`.
        let line = tokio::select! {
            line = lines.next_line() => line,
            () = cancel_token.cancelled() => {
                cancelled = true;
                break 'lines;
            }
        };
        match line {
            Ok(Some(line)) => {
                let outcome = parse_line(&line, turn_id, &mut state);

                // Corrupt thread.started — fail-loud rather than silently
                // creating an unresumable agent. The sidecar invariant
                // requires a valid thread_id; missing/non-string is an
                // upstream contract violation.
                if state.corrupt_thread_started {
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Failed {
                            kind: FailureKind::AdapterFailure,
                            message: "Codex thread.started event missing or non-string thread_id — sidecar unwritable; resume would fail"
                                .to_owned(),
                        },
                        ended_at: Utc::now(),
                        usage: None,
                    });
                    terminal_seen = true;
                    force_kill_child = true;
                    break 'lines;
                }

                if !sidecar_written && let Some(thread_id) = state.pending_thread_id.take() {
                    if let Some(failure) =
                        try_persist_sidecar(&sidecar_file, prior.as_ref(), thread_id, turn_id)
                    {
                        let _ = tx.send(failure);
                        terminal_seen = true;
                        force_kill_child = true;
                        break 'lines;
                    }
                    sidecar_written = true;
                }

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
                        break 'lines;
                    }
                };
                for event in events {
                    match event {
                        AdapterEvent::TurnEnd {
                            turn_id: ev_turn_id,
                            outcome,
                            ended_at,
                            usage,
                        } => {
                            if matches!(outcome, TurnOutcome::Completed) {
                                terminal_was_completed = true;
                            }
                            terminal_seen = true;
                            // First-turn gate. Normal case: no prior sidecar
                            // record. Attach-flow case: prior is Some but the
                            // caller explicitly signals "treat as first turn"
                            // via DispatchOptions, so the sidebar's
                            // MCP/skills/model registry populates on the
                            // first post-attach dispatch.
                            let is_first_turn = prior.is_none() || force_session_meta;
                            emit_terminal_with_enrichment(
                                &tx,
                                &sidecar_file,
                                &home_dir,
                                &cwd,
                                agent_id,
                                ev_turn_id,
                                outcome,
                                ended_at,
                                usage,
                                is_first_turn,
                            )
                            .await;
                        }
                        other => {
                            let _ = tx.send(other);
                        }
                    }
                }
                if terminal_seen {
                    break 'lines;
                }
            }
            Ok(None) => break, // stdout EOF — child has closed stdout; natural shutdown.
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
        // Cancellation path: kill the (two-process) group and end the stream
        // with NO terminal event — the dispatcher synthesizes `Cancelled`.
        // Kill before awaiting the stderr drain (a parked subprocess holds
        // stderr open) and skip the truncation synthesis + exit reconciliation
        // (Codex exits 0 on SIGTERM, so neither would be meaningful here).
        crate::subprocess::terminate_then_kill(&mut child).await;
        let _ = stderr_task.await;
        return;
    }

    if force_kill_child {
        crate::subprocess::terminate_then_kill(&mut child).await;
    }

    let _ = stderr_task.await;

    // Synthesize terminal event if the stream ended without one. Consumes
    // the buffered stdout-error from `state.last_error` (the most recent
    // `{type: "error"}` JSON payload, e.g., a "Reconnecting... 5/5" retry
    // line) as the primary diagnostic source; stderr tail is added as
    // secondary context.
    if !terminal_seen {
        let _ = tx.send(synthesize_truncation_turn_end(
            turn_id,
            &stderr_tail,
            state.last_error.take(),
        ));
    }

    match child.wait().await {
        Ok(status) if !status.success() && terminal_was_completed => {
            tracing::warn!(
                %turn_id,
                agent_id = %agent_id,
                exit_code = ?status.code(),
                "codex emitted turn.completed but subprocess exited non-zero — log-only (Codex exits 0 even on SIGTERM)"
            );
        }
        Err(e) => {
            tracing::warn!(
                %turn_id,
                agent_id = %agent_id,
                error = %e,
                "failed to wait on codex subprocess"
            );
        }
        _ => {}
    }
}

/// Run the post-terminal enrichment cycle for a parser-emitted `TurnEnd`:
///
/// 1. Re-read the sidecar (single source of truth for `session_id` +
///    `session_partition_date` — never recompute the date from
///    `Utc::today()` at enrichment time; see [`session_file`] module docs).
/// 2. Read the Codex session file with the 200ms + 200ms retry policy.
/// 3. Emit the (now-enriched) `TurnEnd`. The enriched `context_window`
///    overlays `usage.context_window` if `usage` was `Some`; if `usage` was
///    `None` we don't fabricate a `TurnUsage` just to carry a context-window
///    (preserves the strict "None means unparseable" contract).
/// 4. Emit `RateLimitEvent` if rate-limit info was extracted.
/// 5. Emit `SessionMeta` if this is the first turn AND the enrichment
///    yielded a model or `cli_version`.
///
/// All steps degrade gracefully — sidecar read failure or session-file
/// absence emits a non-enriched `TurnEnd` only, and the post-terminal
/// derived events are simply skipped.
///
/// `is_first_turn` is computed by the caller as `prior.is_none() ||
/// options.is_first_dispatch_after_attach` — the attach flow pre-writes
/// a sidecar at attach time, so the `prior.is_none()` heuristic alone
/// would misclassify a post-attach dispatch as a resume and skip the
/// load-bearing `SessionMeta` emission that populates the sidebar.
#[allow(clippy::too_many_arguments)]
async fn emit_terminal_with_enrichment(
    tx: &tokio::sync::mpsc::UnboundedSender<AdapterEvent>,
    sidecar_file: &Path,
    home_dir: &Path,
    cwd: &Path,
    agent_id: AgentId,
    turn_id: TurnId,
    outcome: TurnOutcome,
    ended_at: chrono::DateTime<Utc>,
    usage: Option<TurnUsage>,
    is_first_turn: bool,
) {
    // Step 1: re-read sidecar to get the canonical date/session_id pair.
    // If unreadable, skip enrichment entirely and emit a plain TurnEnd.
    let enrichment = match read_latest(sidecar_file) {
        Ok(Some(record)) => {
            session_file::load_with_retry(
                home_dir,
                record.session_partition_date,
                &record.session_id,
                &TokioSleeper,
            )
            .await
        }
        Ok(None) => {
            tracing::warn!(
                agent_id = %agent_id,
                %turn_id,
                "Codex enrichment: sidecar absent at terminal-event time; emitting TurnEnd without enrichment"
            );
            Enrichment::default()
        }
        Err(e) => {
            tracing::warn!(
                agent_id = %agent_id,
                %turn_id,
                error = %e,
                "Codex enrichment: sidecar re-read failed; emitting TurnEnd without enrichment"
            );
            Enrichment::default()
        }
    };

    // Step 3: emit the enriched TurnEnd.
    let enriched_usage = apply_context_window(usage, enrichment.context_window);
    let _ = tx.send(AdapterEvent::TurnEnd {
        turn_id,
        outcome,
        ended_at,
        usage: enriched_usage,
    });

    // Step 4: emit RateLimitEvent if rate-limit info was found. Codex's
    // rate-limit is read from its own session file at turn-end (class B) —
    // already durable on disk, so it's marked `SessionFileBacked` and the
    // dispatcher does NOT re-persist it to the metadata sidecar.
    if let Some(rate_limits) = enrichment.rate_limits.clone() {
        let _ = tx.send(AdapterEvent::RateLimitEvent {
            agent_id,
            info: rate_limits,
            source: crate::events::RateLimitSource::SessionFileBacked,
        });
    }

    // Step 5: emit SessionMeta (first turn only). Loads MCP + skills
    // registries fresh on every emission per the plan's "no caching layer"
    // policy.
    if is_first_turn {
        // Loads both ~/.codex/config.toml and <cwd>/.codex/config.toml
        // unconditionally — Codex's trust-list gate is deliberately
        // skipped; see `config.rs` module doc for rationale (display-only
        // surface, not a security boundary).
        let mcp_servers = config::load_mcp_servers(home_dir, cwd);
        let skills_list = skills::load_skills(home_dir, cwd);
        if let Some(fields) =
            session_file::build_session_meta_fields(&enrichment, mcp_servers, skills_list)
        {
            let _ = tx.send(AdapterEvent::SessionMeta {
                agent_id,
                model: fields.model,
                harness_version: fields.harness_version,
                tools: Vec::new(),
                mcp_servers: fields.mcp_servers,
                skills: fields.skills,
                raw: fields.raw,
            });
        }
    }
}

/// Overlay the enriched `context_window` onto an existing `TurnUsage`.
///
/// **Precedence: stream wins.** If the stream-parsed usage already carries
/// `Some(window)`, enrichment is ignored — the stream is the canonical
/// source for the field. Today Codex's stream never emits a `context_window`
/// (the post-terminal enrichment exists precisely to fill that gap), so
/// the overlay always fires; a future Codex CLI variant that does
/// populate the field would render this overlay a no-op rather than
/// silently overwrite.
///
/// Returns `None` if `usage` was `None` and the enrichment carried no
/// `context_window` — fabricating a `Some` with all-zero tokens just to
/// carry the window would corrupt the "None means unparseable" contract
/// elsewhere in the stack.
fn apply_context_window(
    usage: Option<TurnUsage>,
    enriched_window: Option<u32>,
) -> Option<TurnUsage> {
    match (usage, enriched_window) {
        (Some(mut u), window) => {
            if u.context_window.is_none() {
                u.context_window = window;
            }
            Some(u)
        }
        (None, _) => None,
    }
}

/// Write a session-link record on the first `thread.started` of the
/// dispatch. Returns `None` on success, or `Some(TurnEnd{AdapterFailure})`
/// to emit and terminate the stream on failure. Sidecar persistence is
/// load-bearing for resume and post-terminal enrichment; silent swallow
/// would create an unresumable agent, so continuing is worse than stopping.
fn try_persist_sidecar(
    path: &Path,
    prior: Option<&SessionLinkRecord>,
    thread_id: String,
    turn_id: TurnId,
) -> Option<AdapterEvent> {
    // Codex partitions session files by **local date** under
    // `~/.codex/sessions/<YYYY>/<MM>/<DD>/`. Capture
    // `chrono::Local::now().date_naive()` once on the first dispatch and
    // copy it verbatim on every resume — never recompute. The sidecar's
    // `session_partition_date` is the authoritative directory key for
    // locating the rollout file at enrichment time, even if local date
    // changes between dispatches (Codex keeps appending to the original
    // file). If a future Codex CLI release switches partition behavior
    // (e.g., to UTC), the right fix is to derive the date from the actual
    // rollout file path rather than re-guess from a clock.
    let record = SessionLinkRecord {
        session_id: thread_id,
        session_partition_date: prior.map_or_else(
            || chrono::Local::now().date_naive(),
            |r| r.session_partition_date,
        ),
        started_at: Utc::now(),
    };
    match append_record(path, &record) {
        Ok(()) => None,
        Err(e) => Some(AdapterEvent::TurnEnd {
            turn_id,
            outcome: TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message: format!("sidecar write failed: {e}"),
            },
            ended_at: Utc::now(),
            usage: None,
        }),
    }
}

/// Build the synthesized `TurnEnd(Failed)` event emitted when stdout EOFs
/// without a terminal event. `buffered_error` is the most-recent `{type:
/// "error"}` stdout payload (after JSON unwrap), used as the primary
/// diagnostic message. Stderr tail is appended as secondary context.
fn synthesize_truncation_turn_end(
    turn_id: TurnId,
    stderr_tail: &Mutex<VecDeque<String>>,
    buffered_error: Option<String>,
) -> AdapterEvent {
    let stderr_msg = crate::subprocess::format_stderr_tail(stderr_tail);
    let message = match (buffered_error, stderr_msg.is_empty()) {
        (Some(err), true) => {
            let unwrapped = parser::unwrap_error_message(&err);
            format!("{unwrapped} (stream ended without turn.failed)")
        }
        (Some(err), false) => {
            let unwrapped = parser::unwrap_error_message(&err);
            format!("{unwrapped} (stream ended without turn.failed; stderr: {stderr_msg})")
        }
        (None, true) => {
            "harness exited without terminal stream event (no stderr captured)".to_owned()
        }
        (None, false) => {
            format!("harness exited without terminal stream event; stderr: {stderr_msg}")
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use uuid::Uuid;

    fn fresh_prior_record() -> SessionLinkRecord {
        SessionLinkRecord {
            session_id: "019e2c5f-aaaa-7000-8000-000000000001".to_owned(),
            session_partition_date: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
            started_at: Utc::now(),
        }
    }

    #[test]
    fn build_args_first_turn_omits_resume_and_session_id_and_includes_dash_c() {
        let args = build_args("hello", None);
        assert!(args.contains(&"exec".to_owned()));
        assert!(
            !args.contains(&"resume".to_owned()),
            "first turn has no resume subcommand"
        );
        assert!(args.contains(&"--json".to_owned()));
        assert!(args.contains(&"--skip-git-repo-check".to_owned()));
        assert!(args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_owned()));
        assert!(
            args.iter().any(|a| a == "-C"),
            "first turn includes -C (accepted by `codex exec`)"
        );
        assert_eq!(
            args.last(),
            Some(&"hello".to_owned()),
            "prompt is the last positional"
        );
    }

    #[test]
    fn build_args_resume_includes_session_id_and_omits_dash_c() {
        // Verified against codex-cli 0.130.0: `codex exec resume` rejects -C.
        // This test pins the behaviour against regression.
        let prior = fresh_prior_record();
        let args = build_args("hello again", Some(&prior));
        // resume subcommand
        let exec_idx = args
            .iter()
            .position(|a| a == "exec")
            .expect("exec subcommand");
        let resume_idx = args
            .iter()
            .position(|a| a == "resume")
            .expect("resume subcommand");
        assert!(resume_idx > exec_idx, "resume comes after exec");

        // session_id is the second-to-last arg (last is prompt)
        assert_eq!(args[args.len() - 2], prior.session_id);
        assert_eq!(args.last(), Some(&"hello again".to_owned()));

        // Critical: no -C / --cd on resume
        assert!(
            !args.iter().any(|a| a == "-C" || a == "--cd"),
            "resume must NOT include -C — rejected by codex 0.130.0; got args: {args:?}"
        );

        // Flags that ARE accepted by resume
        assert!(args.contains(&"--json".to_owned()));
        assert!(args.contains(&"--skip-git-repo-check".to_owned()));
        assert!(args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_owned()));
    }

    #[test]
    fn probe_reports_missing_binary_for_absolute_path() {
        let adapter = CodexAdapter::with_binary_path("/nonexistent/path/to/codex");
        assert!(matches!(
            adapter.probe(),
            Err(DispatchError::BinaryNotFound)
        ));
    }

    #[test]
    fn probe_reports_missing_binary_for_relative_name() {
        let adapter = CodexAdapter::with_binary_path("this-binary-does-not-exist-on-PATH-xyz123");
        assert!(matches!(
            adapter.probe(),
            Err(DispatchError::BinaryNotFound)
        ));
    }

    #[tokio::test]
    async fn dispatch_with_corrupt_sidecar_returns_pre_stream_read_error() {
        // Write a malformed sidecar before dispatch. The adapter must
        // surface SidecarError::Corrupt as DispatchError::PreStreamRead
        // (per the AGENTS.md invariant: corruption in Switchboard-owned
        // JSONL is fail-loud).
        let tmp = tempfile::TempDir::new().unwrap();
        let agent = AgentRecord {
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            name: "test".to_owned(),
            harness: switchboard_core::HarnessKind::Codex,
            session_id: None,
            created_at: Utc::now(),
        };
        let path = sidecar_path(tmp.path(), agent.project_id, agent.id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not valid json\n").unwrap();

        // Use any binary path — dispatch fails on sidecar read before spawn.
        let adapter = CodexAdapter::with_binary_path("/nonexistent");
        match adapter
            .dispatch(
                &agent,
                tmp.path(),
                "hi",
                Uuid::now_v7(),
                crate::DispatchOptions::default(),
            )
            .await
        {
            Err(DispatchError::PreStreamRead(_)) => {}
            Err(other) => panic!("expected PreStreamRead, got {other:?}"),
            Ok(_) => panic!("dispatch must fail on corrupt sidecar"),
        }
    }

    fn usage_with_window(window: Option<u32>) -> TurnUsage {
        TurnUsage {
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: None,
            reasoning_output_tokens: None,
            context_window: window,
            total_cost_usd: None,
        }
    }

    #[test]
    fn apply_context_window_overlays_when_stream_field_is_none() {
        let usage = usage_with_window(None);
        let result = apply_context_window(Some(usage), Some(258_400));
        assert_eq!(
            result.and_then(|u| u.context_window),
            Some(258_400),
            "enrichment fills the gap when stream omits the field"
        );
    }

    #[test]
    fn apply_context_window_preserves_stream_value_over_enrichment() {
        // Stream wins. A future Codex CLI variant that emits context_window
        // directly must not be silently overwritten by the enrichment path.
        let usage = usage_with_window(Some(123_456));
        let result = apply_context_window(Some(usage), Some(999_999));
        assert_eq!(
            result.and_then(|u| u.context_window),
            Some(123_456),
            "stream-emitted context_window wins over enrichment"
        );
    }

    #[test]
    fn apply_context_window_returns_none_when_usage_is_none() {
        // Strict "None means unparseable usage" — never fabricate a Some
        // just to carry an enrichment-derived window.
        let result = apply_context_window(None, Some(258_400));
        assert!(result.is_none());
    }
}
