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
//!   pre-generated at agent registration. The captured `thread_id` (plus the
//!   local partition-date) is emitted as a `SessionLocatorCaptured` event and
//!   persisted by the dispatcher onto the agent's registry record
//!   (`SessionLocator::Codex`); on resume it's read back from that record. The
//!   `sidecar.rs` module is retained only for the one-time migration of
//!   pre-existing session-link files (see its module doc).
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
use chrono::{NaiveDate, Utc};
use switchboard_core::{AgentId, AgentRecord, SessionLocator};
use tokio::io::AsyncBufReadExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::adapter::{DispatchError, EventStream, HarnessAdapter};
use crate::events::{AdapterEvent, FailureKind, TurnId, TurnOutcome, TurnUsage};

use parser::{CodexParserState, parse_line};
use session_file::{Enrichment, TokioSleeper};

/// Codex's session locator carried through a dispatch: the runtime `thread_id`
/// plus the **local** date its rollout file is partitioned under
/// (`~/.codex/sessions/<YYYY>/<MM>/<DD>/`). On resume it's read from the agent's
/// `SessionLocator::Codex`; on the first dispatch the `thread_id` is captured
/// from `thread.started` and the date is stamped once (never recomputed). Used
/// for both `exec resume` and post-terminal enrichment.
#[derive(Clone)]
struct CodexLocator {
    thread_id: String,
    partition_date: NaiveDate,
}

/// Extract the Codex resume locator from an agent's registry record. `None` on
/// the first dispatch (no locator yet) — the adapter captures it from the
/// stream and emits it for the dispatcher to persist.
fn codex_locator(agent: &AgentRecord) -> Option<CodexLocator> {
    agent
        .session_locator
        .as_ref()
        .and_then(SessionLocator::as_codex)
        .map(|(thread_id, partition_date)| CodexLocator {
            thread_id: thread_id.to_owned(),
            partition_date,
        })
}

/// Adapter for Codex (`codex exec --json`). Spawns a `codex` subprocess and
/// maps the stream-event output to `AdapterEvent`s. Session continuity is
/// captured from the first `thread.started` stream event and emitted as a
/// `SessionLocatorCaptured` event the dispatcher persists to the agent's
/// registry record; on resume the locator is read back from that record.
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
        // Resume locator: the `thread_id` + partition-date captured on a prior
        // dispatch, now carried on the agent's registry record (`session_locator`)
        // and passed in as dispatch input — like Claude/Gemini. `None` is the
        // first-dispatch case; the adapter captures it from the stream below.
        let prior = codex_locator(agent);
        let args = build_args(prompt, prior.as_ref().map(|l| l.thread_id.as_str()));

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
        // (no prior locator). The attach flow writes the locator onto the
        // record at attach time, so without this override the first
        // post-attach dispatch would be misclassified as a resume and
        // SessionMeta would never fire for the attached agent's sidebar.
        // Caller signals "treat this as first turn" via DispatchOptions.
        let force_session_meta = options.is_first_dispatch_after_attach;

        tokio::spawn(run_producer(
            child,
            stdout,
            stderr,
            tx,
            turn_id,
            agent_id,
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
fn build_args(prompt: &str, resume_thread_id: Option<&str>) -> Vec<String> {
    if let Some(thread_id) = resume_thread_id {
        // Resume subcommand — NO `-C` (rejected by codex 0.130.0). cwd is
        // pinned via `Command::current_dir(cwd)` instead.
        vec![
            "exec".to_owned(),
            "resume".to_owned(),
            "--json".to_owned(),
            "--skip-git-repo-check".to_owned(),
            "--dangerously-bypass-approvals-and-sandbox".to_owned(),
            thread_id.to_owned(),
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
    prior: Option<CodexLocator>,
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
    // The locator captured from the first `thread.started` of a first dispatch
    // (None on resume — `prior` already holds it). Once set, later
    // thread.started events are ignored (defensive — Codex emits one per
    // dispatch). Drives post-terminal enrichment when `prior` is absent.
    let mut captured: Option<CodexLocator> = None;
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

                // Corrupt thread.started on a **first** dispatch — fail-loud
                // rather than silently creating an unresumable agent. We need a
                // valid thread_id to capture the locator; missing/non-string is
                // an upstream contract violation. On a resume the locator is
                // already on the record, so a corrupt thread.started is harmless
                // and ignored.
                if prior.is_none() && state.corrupt_thread_started {
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Failed {
                            kind: FailureKind::AdapterFailure,
                            message: "Codex thread.started event missing or non-string thread_id — cannot capture session locator; resume would fail"
                                .to_owned(),
                        },
                        ended_at: Utc::now(),
                        usage: None,
                    });
                    terminal_seen = true;
                    force_kill_child = true;
                    break 'lines;
                }

                // First-dispatch capture: stamp the partition-date once (local
                // date — Codex partitions rollout files by local date and never
                // re-partitions on resume) and emit the locator for the
                // dispatcher to persist (load-bearing; a persist failure fails
                // the turn there). On resume `prior` is set, so we never capture.
                if prior.is_none()
                    && captured.is_none()
                    && let Some(thread_id) = state.pending_thread_id.take()
                {
                    let locator = CodexLocator {
                        thread_id,
                        partition_date: chrono::Local::now().date_naive(),
                    };
                    let _ = tx.send(AdapterEvent::SessionLocatorCaptured {
                        locator: SessionLocator::Codex {
                            thread_id: locator.thread_id.clone(),
                            partition_date: locator.partition_date,
                        },
                    });
                    captured = Some(locator);
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
                            terminal_seen = true;
                            // A first dispatch that *completes* without ever
                            // capturing a locator (the stream omitted
                            // thread.started) would leave the agent unresumable:
                            // the next dispatch starts a fresh session and
                            // silently loses context. Fail loud instead —
                            // mirroring the corrupt-thread.started guard above
                            // and Antigravity's unresumable path. A *failed*
                            // first turn has nothing to resume, so only
                            // Completed is converted.
                            if prior.is_none()
                                && captured.is_none()
                                && matches!(outcome, TurnOutcome::Completed)
                            {
                                let _ = tx.send(AdapterEvent::TurnEnd {
                                    turn_id: ev_turn_id,
                                    outcome: TurnOutcome::Failed {
                                        kind: FailureKind::AdapterFailure,
                                        message: "Codex completed a first turn without a thread.started event — no session locator captured; resume would lose context"
                                            .to_owned(),
                                    },
                                    ended_at,
                                    usage: None,
                                });
                                force_kill_child = true;
                                break 'lines;
                            }
                            if matches!(outcome, TurnOutcome::Completed) {
                                terminal_was_completed = true;
                            }
                            // First-turn gate. Normal case: no prior locator.
                            // Attach-flow case: prior is Some but the caller
                            // explicitly signals "treat as first turn" via
                            // DispatchOptions, so the sidebar's MCP/skills/model
                            // registry populates on the first post-attach dispatch.
                            let is_first_turn = prior.is_none() || force_session_meta;
                            // Enrichment locates the rollout file from the
                            // effective locator: the resume locator (`prior`) or
                            // the one just captured this dispatch.
                            let locator = prior.as_ref().or(captured.as_ref());
                            emit_terminal_with_enrichment(
                                &tx,
                                locator,
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
/// 1. Locate the rollout file from the passed-in locator (`thread_id` +
///    `partition_date` — never recompute the date from `Utc::today()` at
///    enrichment time; see [`session_file`] module docs).
/// 2. Read the Codex session file with the 200ms + 200ms retry policy.
/// 3. Emit the (now-enriched) `TurnEnd`. The enriched `context_window`
///    overlays `usage.context_window` if `usage` was `Some`; if `usage` was
///    `None` we don't fabricate a `TurnUsage` just to carry a context-window
///    (preserves the strict "None means unparseable" contract).
/// 4. Emit `RateLimitEvent` if rate-limit info was extracted.
/// 5. Emit `SessionMeta` if this is the first turn AND the enrichment
///    yielded a model or `cli_version`.
///
/// All steps degrade gracefully — a missing locator or session-file absence
/// emits a non-enriched `TurnEnd` only, and the post-terminal derived events
/// are simply skipped.
///
/// `is_first_turn` is computed by the caller as `prior.is_none() ||
/// options.is_first_dispatch_after_attach` — the attach flow writes the locator
/// onto the record at attach time, so the `prior.is_none()` heuristic alone
/// would misclassify a post-attach dispatch as a resume and skip the
/// load-bearing `SessionMeta` emission that populates the sidebar.
#[allow(clippy::too_many_arguments)]
async fn emit_terminal_with_enrichment(
    tx: &tokio::sync::mpsc::UnboundedSender<AdapterEvent>,
    locator: Option<&CodexLocator>,
    home_dir: &Path,
    cwd: &Path,
    agent_id: AgentId,
    turn_id: TurnId,
    outcome: TurnOutcome,
    ended_at: chrono::DateTime<Utc>,
    usage: Option<TurnUsage>,
    is_first_turn: bool,
) {
    // Step 1: locate the rollout file from the locator (resume locator, or the
    // one captured this dispatch). Absent only if capture failed/never
    // happened — then skip enrichment and emit a plain TurnEnd.
    let enrichment = if let Some(loc) = locator {
        session_file::load_with_retry(home_dir, loc.partition_date, &loc.thread_id, &TokioSleeper)
            .await
    } else {
        tracing::warn!(
            agent_id = %agent_id,
            %turn_id,
            "Codex enrichment: no session locator at terminal-event time; emitting TurnEnd without enrichment"
        );
        Enrichment::default()
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

    const RESUME_THREAD_ID: &str = "019e2c5f-aaaa-7000-8000-000000000001";

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
        let args = build_args("hello again", Some(RESUME_THREAD_ID));
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

        // thread_id is the second-to-last arg (last is prompt)
        assert_eq!(args[args.len() - 2], RESUME_THREAD_ID);
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
