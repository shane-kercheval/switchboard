//! Antigravity CLI harness module.
//!
//! Antigravity (binary `agy`) is Google's replacement for the Gemini CLI on
//! free / Google AI Pro / Ultra tiers. It is a Go-based client for a
//! server-side agent, with a contract distinct from every other harness:
//!
//! - **No structured stream protocol.** `agy -p` writes the model's final
//!   answer as plain text to stdout (a server-side "drip"), and `Error:` /
//!   `Warning:` / `Authentication required` lines on failure. There is no
//!   `--output-format stream-json`.
//! - **One-shot process = one turn.** `agy -p` runs exactly one turn and
//!   exits. Process exit — not a stream/transcript record — is the
//!   authoritative turn terminator. `agy` exits 0 on essentially every
//!   condition (success, empty prompt, unknown conversation, timeout,
//!   auth failure), so the exit code is useless for outcome detection;
//!   stdout text is the failure signal.
//! - **Server-assigned conversation UUID.** The UUID is minted server-side.
//!   `agy` records it in *this dispatch's* private `--log-file` (a line like
//!   `… server.go:755] Created conversation <uuid>`), which is the primary,
//!   deterministic capture source: read straight from our own log, it is the
//!   conversation this exact invocation used — available in ~seconds
//!   regardless of cold-start latency, and never cross-attributed from a
//!   concurrent or background `agy` (each writes its own log). It is then
//!   emitted as a `SessionLocatorCaptured` event and persisted by the
//!   dispatcher onto the agent's registry record for resume / hydration. A
//!   filesystem fallback (watch `brain/<uuid>/` for a dir whose
//!   transcript echoes the prompt) covers the case where that Google-internal
//!   log line ever moves — see [`capture_conversation_id`].
//! - **Transcript-sourced content; stdout is a control channel.** All
//!   displayed content — assistant text, `thinking`, and tool lifecycle
//!   (`ToolStarted` / `ToolCompleted`) — comes from tailing the conversation's
//!   `transcript.jsonl` (see [`paths`]). stdout is **not** used for content:
//!   on a resume turn `agy` replays the whole conversation's prior answers to
//!   stdout, which would make each turn accumulate every earlier answer. The
//!   transcript records one completed `PLANNER_RESPONSE` per turn and the
//!   resume cursor isolates only the new turn's records, so it is the clean
//!   per-turn source (and matches what hydration reconstructs from disk).
//!   stdout is read only for control signals: the auth-failure fast-fail
//!   below, `Error:` lines, and a "produced output" liveness signal (used to
//!   tell output-without-a-readable-answer apart from no-output — *not* a
//!   success signal; a turn is `Completed` only on a transcript terminal
//!   answer).
//! - **Auth fast-fail.** When the keyring token is stale, `agy -p` falls
//!   back to interactive OAuth: it prints `Authentication required...`,
//!   opens a browser, and blocks ~30s before timing out. There is no flag
//!   to suppress this. The adapter detects the `Authentication required`
//!   stdout line, force-kills immediately, and emits `AuthFailure` —
//!   bounding the hang. (The browser tab cannot be prevented; documented as
//!   a known limitation.)
//!
//! Ground-truth reference: `docs/research/archive/antigravity-cli-observed.md`.

pub mod config;
pub(crate) mod facets;
pub mod parser;
pub mod paths;
pub mod session_file;
pub mod skills;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::Utc;
use switchboard_core::{AgentId, AgentRecord, SessionLocator};
use tokio::io::AsyncBufReadExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::adapter::{DispatchError, DispatchOptions, EventStream, HarnessAdapter};
use crate::events::{AdapterEvent, FailureKind, McpServerStatus, TurnId, TurnOutcome};

use parser::{
    AntigravityParserState, TranscriptRecord, first_error_line, is_auth_failure_line,
    record_to_live_events,
};

/// The binary name on PATH. Centralized so the adapter and the pre-adapter
/// binary probe agree on the name.
pub const BINARY_NAME: &str = "agy";

/// Authored auth-failure message for Antigravity. Reactive-auth posture:
/// the recovery is "sign in, then send again" — never "reload Switchboard"
/// (there's no proactive state to refresh). Used by every Antigravity auth
/// path (stdout fast-fail, `Error: authentication timed out`, etc.) so all
/// surfaces show the same actionable text.
const ANTIGRAVITY_AUTH_MESSAGE: &str =
    "Antigravity authentication required — run `agy` to authenticate";

/// Control-file names for the `fake_agy` test fixture binary, read from the
/// dispatch cwd. Defined in the library (not the bin) so the fixture binary
/// and the integration tests share one source of truth and can't drift.
/// Unused by production dispatch — `agy` ignores them.
pub const FAKE_AGY_SCRIPT_FILE: &str = ".fake_agy.json";
/// Append-only log of each `fake_agy` invocation's argv (one line per spawn),
/// so tests can assert what flags the adapter passed across dispatches (e.g.
/// `--conversation <healed-uuid>` on the post-fork resume).
pub const FAKE_AGY_INVOCATIONS_FILE: &str = ".fake_agy.invocations";

/// Poll interval for log-file UUID discovery and transcript tailing.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Post-exit drain attempts for the terminal answer. The in-loop poll already
/// catches the common case (the answer record lands before `agy` exits); this
/// only covers the narrow window where the terminal `PLANNER_RESPONSE` is
/// flushed just after exit. The answer text is now transcript-sourced and
/// load-bearing for every turn, so a generous-but-bounded retry keeps the
/// "no terminal answer → fail loud" classification from misfiring on flush lag.
/// `attempt 0` reads immediately; the rest are spaced `POLL_INTERVAL` apart.
const FINAL_DRAIN_ATTEMPTS: u32 = 6;

/// Value passed to `agy --print-timeout`. `agy`'s default is `5m`, and it is a
/// whole-turn wall-clock cap (probe-confirmed) — not just an initial-response
/// wait — so a long-but-actively-working turn (e.g. a PR review running a test
/// suite) gets killed mid-flight with `Error: timed out waiting for response`.
/// The dispatcher imposes no turn deadline and the other three harnesses have
/// no built-in cap, so we override `agy`'s default with a generous value to
/// match that posture. `0` is NOT a disable sentinel — it means zero-wait and
/// times out immediately — so a large finite value is used; `24h` never cuts a
/// real turn yet still eventually reaps a truly-wedged `agy` rather than leaking
/// it. (An automatic backstop for a zero-progress wedge, if ever wanted, belongs
/// in the dispatcher for every adapter, not as an Antigravity-only timer.)
const PRINT_TIMEOUT: &str = "24h";

/// Verify a binary name is on PATH, mapping a miss to `BinaryNotFound`.
/// Used by [`AntigravityAdapter::probe`]; factored out so the negative-path
/// test can pass a synthetic name.
fn probe_binary_by_name(name: &str) -> Result<(), DispatchError> {
    crate::subprocess::probe_binary(std::path::Path::new(name))
}

/// Adapter for Antigravity (`agy -p`). See the module docstring for the
/// architecture (dual-source streaming, server-assigned UUID, process-exit
/// terminator). Constructed with `with_binary_and_home` in tests to inject a
/// fixture binary and a staged `$HOME`.
pub struct AntigravityAdapter {
    binary_path: PathBuf,
    home_dir_override: Option<PathBuf>,
    cached_version: OnceLock<String>,
}

impl AntigravityAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            binary_path: PathBuf::from(BINARY_NAME),
            home_dir_override: None,
            cached_version: OnceLock::new(),
        }
    }

    pub fn with_binary_path(path: impl Into<PathBuf>) -> Self {
        Self {
            binary_path: path.into(),
            home_dir_override: None,
            cached_version: OnceLock::new(),
        }
    }

    pub fn with_binary_and_home(
        binary_path: impl Into<PathBuf>,
        home_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            binary_path: binary_path.into(),
            home_dir_override: Some(home_dir.into()),
            cached_version: OnceLock::new(),
        }
    }

    fn resolve_version(&self) -> String {
        self.cached_version
            .get_or_init(|| crate::subprocess::fetch_version(&self.binary_path).unwrap_or_default())
            .clone()
    }
}

impl Default for AntigravityAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HarnessAdapter for AntigravityAdapter {
    fn probe(&self) -> Result<(), DispatchError> {
        probe_binary_by_name(&self.binary_path.to_string_lossy())
    }

    fn version(&self) -> Option<String> {
        crate::subprocess::parse_cli_version(&self.resolve_version())
    }

    async fn dispatch(
        &self,
        agent: &AgentRecord,
        cwd: &Path,
        prompt: &str,
        turn_id: TurnId,
        options: DispatchOptions,
    ) -> Result<EventStream, DispatchError> {
        if prompt.trim().is_empty() {
            // `agy -p ""` exits 0 with "Error: empty prompt" — pre-validate
            // so the failure is a recognizable input error, not a turn that
            // silently produces nothing.
            return Err(DispatchError::InvalidPrompt(
                "prompt is empty or whitespace-only".to_owned(),
            ));
        }

        let binary = crate::subprocess::resolve_binary(&self.binary_path)?;
        let home_dir = resolve_home_dir(self.home_dir_override.as_deref());
        let harness_version = self.resolve_version();

        // Display-only registries, read fresh per dispatch (config can change
        // between turns). User scope only — `cwd` is passed for the future
        // workspace scope but unused by the loaders today.
        let mcp_servers = config::load_mcp_servers(&home_dir, cwd);
        let skills = skills::load_skills(&home_dir, cwd);

        // Resume target: the conversation UUID captured on a prior dispatch,
        // now carried on the agent's registry record (`session_locator`) and
        // passed in as dispatch input — like Claude/Gemini. `None` is the
        // legitimate never-dispatched case; a `Codex` locator can't appear on
        // an Antigravity agent (the registry rejects the mismatch), so a
        // non-`Uuid` locator degrades to `None`.
        let resume_id = agent
            .session_locator
            .as_ref()
            .and_then(SessionLocator::as_uuid);

        let log_file_path = build_log_file_path(turn_id);
        let args = build_args(prompt, cwd, resume_id, &log_file_path);

        let mut command = tokio::process::Command::new(&binary);
        command
            .args(&args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Null stdin is load-bearing here, not hygiene: `agy -p` has no stdin
            // timeout and hangs forever on an open stdin (see
            // docs/research/archive/antigravity-cli-observed.md).
            .stdin(Stdio::null())
            .kill_on_drop(true);
        crate::subprocess::apply_path_env(&mut command);
        // When a home override is set (tests), pass it to the child as `HOME`
        // so the conversation directory the child writes
        // (`$HOME/.gemini/antigravity-cli/brain/<uuid>/`) and the directory the
        // producer watches resolve to the *same* path. Without this they'd
        // diverge — the producer would watch the override while `agy` wrote to
        // the real `$HOME` — and capture would silently never bind. Production
        // never sets the override (`new()` reads the real `$HOME`), so this is
        // a no-op there.
        if let Some(home) = &self.home_dir_override {
            command.env("HOME", home);
        }
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

        tokio::spawn(run_producer(ProducerCtx {
            child,
            stdout,
            stderr,
            tx,
            turn_id,
            agent_id: agent.id,
            home_dir,
            resume_id,
            harness_version,
            mcp_servers,
            skills,
            prompt: prompt.to_owned(),
            log_file_path,
            cancel_token: options.cancel_token,
        }));

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

/// Build the `agy` invocation.
///
/// `--add-dir <cwd>` establishes the **workspace** so the model's file and
/// command tools operate in the project directory. This is load-bearing and
/// non-obvious: setting the child's `current_dir(cwd)` alone is *not* enough —
/// without `--add-dir`, `agy` reports "no active workspace set" and runs tools
/// against `$HOME` (verified live: a file written into the dispatch cwd is
/// invisible to the model, and `run_command` executes with `Cwd=$HOME`). Passed
/// on every dispatch (`agy` is one-shot, so the workspace must be re-established
/// each turn). `agy` also discovers a `.gemini/config/projects/<id>.json` from
/// the cwd regardless, but that "project" is distinct from the active workspace
/// the tools use — only `--add-dir` sets the latter.
///
/// `--dangerously-skip-permissions` mirrors Gemini's `--yolo`: the bound cwd is
/// the user's own workspace, so per-tool approval prompts (which would block a
/// headless dispatch) must be auto-approved. Resume passes the captured UUID via
/// `--conversation`; first turn omits it and lets `agy` mint a new one.
///
/// `log_file` isolates this dispatch's CLI log so the no-answer-branch outcome
/// scan (and the conversation-id capture) read only this turn's output — never
/// another concurrent `agy`'s log.
///
/// No per-agent model or effort flag: Antigravity's model is harness-owned
/// global config (set inside Antigravity, off-limits to us) and its effort is
/// folded into that model's display name — there is no `agy` flag for either.
/// An Antigravity agent therefore never carries a model/effort (registration
/// forbids it), so this takes no `&AgentRecord` and emits neither flag.
fn build_args(prompt: &str, cwd: &Path, resume_id: Option<Uuid>, log_file: &Path) -> Vec<String> {
    let mut args = vec!["-p".to_owned(), prompt.to_owned()];
    args.push("--add-dir".to_owned());
    args.push(cwd.to_string_lossy().into_owned());
    if let Some(uuid) = resume_id {
        args.push("--conversation".to_owned());
        args.push(uuid.to_string());
    }
    args.push("--dangerously-skip-permissions".to_owned());
    args.push("--print-timeout".to_owned());
    args.push(PRINT_TIMEOUT.to_owned());
    args.push("--log-file".to_owned());
    args.push(log_file.to_string_lossy().into_owned());
    args
}

/// Per-dispatch CLI log path under the system temp dir. `turn_id` is unique
/// per dispatch so concurrent agents can't collide; missing/unreadable later
/// degrades to the generic fallback (best-effort scan).
fn build_log_file_path(turn_id: TurnId) -> PathBuf {
    std::env::temp_dir().join(format!("switchboard-agy-{turn_id}.log"))
}

/// Arguments to [`run_producer`]. A struct rather than a long parameter list
/// (the producer already carries the adapter's worst clippy-arg-count).
struct ProducerCtx {
    child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    tx: tokio::sync::mpsc::UnboundedSender<AdapterEvent>,
    turn_id: TurnId,
    agent_id: AgentId,
    home_dir: PathBuf,
    resume_id: Option<Uuid>,
    harness_version: String,
    /// MCP-server registry resolved at dispatch time (display-only). Carried
    /// into the post-`TurnEnd` `SessionMeta` event.
    mcp_servers: Vec<McpServerStatus>,
    /// Skills registry (`<plugin>/<skill>` names) resolved at dispatch time.
    skills: Vec<String>,
    /// The dispatch prompt — used to correlate the captured conversation
    /// directory to *this* dispatch (its `USER_INPUT` record echoes the
    /// prompt), so concurrent same-cwd dispatches can't bind each other's
    /// conversation.
    prompt: String,
    /// Per-dispatch CLI log path. Passed to `agy` via `--log-file` so this
    /// turn's log is isolated; scanned on the no-answer branch for the
    /// underlying RPC error (the only place quota / network failures appear —
    /// `agy` exits 0 with empty stdout/stderr on them). Cleaned up best-effort
    /// after the scan.
    log_file_path: PathBuf,
    /// Fired by the dispatcher to cancel the turn. Watched as an arm of the
    /// producer's existing `select!`.
    cancel_token: CancellationToken,
}

/// Drive a single Antigravity turn to completion, emitting normalized
/// events. See the module docstring for the dual-source design. The loop
/// terminates on process exit (the authoritative turn boundary); outcome is
/// derived from the stdout error scan + whether the transcript recorded a
/// terminal answer.
#[allow(clippy::too_many_lines)]
async fn run_producer(ctx: ProducerCtx) {
    let ProducerCtx {
        mut child,
        stdout,
        stderr,
        tx,
        turn_id,
        agent_id,
        home_dir,
        resume_id,
        harness_version,
        mcp_servers,
        skills,
        prompt,
        log_file_path,
        cancel_token,
    } = ctx;

    let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::with_capacity(
        crate::subprocess::STDERR_TAIL_CAPACITY,
    )));
    let stderr_task = tokio::spawn(crate::subprocess::drain_stderr(
        stderr,
        agent_id,
        turn_id,
        Arc::clone(&stderr_tail),
        "antigravity",
    ));

    let is_resume = resume_id.is_some();
    // A plain resume re-emits nothing: the locator already lives on the registry
    // record (it's how `resume_id` got here), so there's no new identity to
    // persist. The capture event fires only when the locator is newly learned
    // (first dispatch) or changes (fork-and-heal) — see the capture sites below.

    let spawn_time = SystemTime::now();
    let mut conversation_id = resume_id;
    let mut transcript_path = conversation_id.map(|u| paths::transcript_path(&home_dir, u));
    // Resume: skip records already on disk from prior turns so only the new
    // turn's records emit. First turn: cursor 0, transcript not yet created.
    let mut cursor = transcript_path.as_deref().map_or(0, count_complete_lines);

    let mut parser_state = AntigravityParserState::default();
    let mut stdout_lines = tokio::io::BufReader::new(stdout).lines();
    let mut stdout_buf: Vec<String> = Vec::new();
    // Seed the carry-forward model from prior turns so an unchanged resume's
    // live `TurnEnd.model` matches the hydrator (Antigravity re-announces only
    // on change). This dispatch's drain overwrites it last-wins if this turn
    // announces a new model.
    let mut model: Option<String> = if is_resume {
        transcript_path
            .as_deref()
            .and_then(|p| seed_carry_forward_model(p, cursor))
    } else {
        None
    };
    let mut saw_terminal_answer = false;
    let mut saw_stdout_content = false;
    let mut auth_failed = false;
    let mut ambiguous_capture = false;
    // Set post-loop when a required (re)capture could not locate the
    // conversation directory — the agent would be unresumable, so the turn
    // must fail loudly rather than silently complete on the stdout answer.
    let mut unresumable = false;
    let mut stdout_eof = false;
    // Set when cancellation fires: kill the group and end the stream with NO
    // terminal event (the dispatcher synthesizes `Cancelled`). Unlike the
    // stdout-draining adapters, Antigravity already uses `select!` to tail the
    // transcript on a tick — cancellation is just one more arm.
    let mut cancelled = false;
    let mut exit_status: Option<std::process::ExitStatus> = None;

    let mut poll = tokio::time::interval(POLL_INTERVAL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            maybe_line = stdout_lines.next_line(), if !stdout_eof => {
                match maybe_line {
                    Ok(Some(line)) => {
                        if is_auth_failure_line(&line) {
                            // Fast-fail: the OAuth fallback is mid-flight (browser
                            // open, ~30s block ahead). Stop now and force-kill.
                            auth_failed = true;
                            break;
                        }
                        let trimmed = line.trim();
                        if trimmed.starts_with("Error:") || trimmed.starts_with("Warning:") {
                            stdout_buf.push(line);
                        } else if !trimmed.is_empty() {
                            // Normal drip text is **not** emitted as content: on a
                            // resume turn `agy` replays the whole conversation's
                            // prior answers to stdout, which would make each turn's
                            // bubble accumulate every earlier answer. The answer
                            // text comes from the transcript's per-turn
                            // `PLANNER_RESPONSE` record instead (see
                            // `record_to_live_events`). We still note that output
                            // appeared — a "produced output" signal `classify_outcome`
                            // uses to distinguish output-without-a-readable-answer
                            // (fail loud) from no-output. It is **not** a success
                            // signal: completion requires a transcript terminal answer.
                            saw_stdout_content = true;
                        }
                    }
                    Ok(None) | Err(_) => stdout_eof = true,
                }
            }
            _ = poll.tick() => {
                if conversation_id.is_none() {
                    match capture_conversation_id(&log_file_path, &home_dir, spawn_time, &prompt) {
                        CaptureOutcome::Bound(uuid) => {
                            conversation_id = Some(uuid);
                            transcript_path = Some(paths::transcript_path(&home_dir, uuid));
                            cursor = 0;
                            // First-turn capture: emit the locator so the
                            // dispatcher persists it to the registry. The
                            // persist is load-bearing (a lost locator leaves the
                            // agent unresumable) but its fatality now lives in
                            // the dispatcher sink, not here.
                            emit_locator_captured(&tx, uuid);
                        }
                        CaptureOutcome::Ambiguous => ambiguous_capture = true,
                        CaptureOutcome::NotYet => {}
                    }
                }
                if let Some(path) = &transcript_path {
                    drain_transcript(
                        path,
                        &mut cursor,
                        turn_id,
                        &mut parser_state,
                        &mut saw_terminal_answer,
                        &mut model,
                        &tx,
                    );
                }
                if exit_status.is_none()
                    && let Ok(Some(status)) = child.try_wait()
                {
                    exit_status = Some(status);
                }
            }
            () = cancel_token.cancelled() => {
                // Cancellation: break the loop and let the post-loop
                // `if cancelled` handler kill the group and end the stream with
                // NO terminal event (the dispatcher synthesizes `Cancelled`).
                cancelled = true;
                break;
            }
        }

        // Abort paths: stop immediately and force-kill below.
        if auth_failed || ambiguous_capture {
            break;
        }
        // Terminator: the process has exited AND stdout drained. The
        // transcript is fully flushed by now; a final drain happens below.
        if stdout_eof && exit_status.is_some() {
            break;
        }
        // First-turn UUID never appeared and the child has exited (one-shot
        // turn done, log + transcript flushed) — nothing more will be written,
        // so stop. **No wall-clock capture deadline**: the conversation id is
        // read from `agy`'s own log file, which lands in ~seconds independent of
        // server-response latency, so there is no slow-cold-start to race, and
        // the terminator is process exit (a real event) plus cancellation — the
        // *same* terminators the Claude/Codex/Gemini producers use; none of them
        // has a capture/run timeout and the dispatcher has no turn deadline, so
        // removing this one aligns Antigravity with that pattern (it was the odd
        // one out, and the old 5s deadline actively false-failed healthy ~10s
        // cold starts). The residual gap — an `agy` that spawns, emits nothing,
        // and never exits pins the turn until manual cancel — is systemic across
        // all adapters, not introduced here; an automatic backstop for a
        // zero-output wedge, if ever wanted, belongs in the dispatcher for every
        // adapter, not as an Antigravity-only timeout that would reintroduce the
        // cold-start false-fail.
        if conversation_id.is_none() && exit_status.is_some() {
            break;
        }
    }

    if cancelled {
        // Cancellation path: kill the group and end the stream with NO
        // terminal event (the dispatcher synthesizes `Cancelled`). Skip the
        // post-exit recapture / `classify_outcome` / `TurnEnd` / `SessionMeta`
        // below. Kill before awaiting the stderr drain — a parked subprocess
        // holds stderr open.
        crate::subprocess::terminate_then_kill(&mut child).await;
        let _ = stderr_task.await;
        let _ = std::fs::remove_file(&log_file_path);
        return;
    }

    // Force-kill if we broke out while the child may still be running (auth
    // fast-fail, or capture timeout with the process hung on OAuth).
    // `terminate_then_kill` reaps the child itself, so no trailing `wait`.
    if exit_status.is_none() {
        crate::subprocess::terminate_then_kill(&mut child).await;
    }
    // Drain stderr before scanning it for the resume-not-found signal below.
    let _ = stderr_task.await;

    // Post-exit (re)capture. Handles two cases the in-loop polling can miss,
    // both now that the process is reaped and the transcript is fully flushed:
    //
    // 1. **First-turn missed capture** — a very fast turn can break on
    //    `stdout_eof && exited` before a correlate tick ran. Without this the
    //    UUID is never persisted and the agent is silently unresumable.
    // 2. **Stale-resume fork-and-heal** — when a resume's `--conversation`
    //    UUID no longer exists server-side, `agy` prints "conversation not
    //    found", mints a *fresh* conversation under a new `brain/<uuid>/`, and
    //    answers. The in-loop tail was pointed at the stale dir (no records),
    //    so we re-correlate the new dir (the stale one is filtered by the
    //    spawn-time window), persist it (so future turns continue the healed
    //    conversation), and re-point the transcript so the final drain
    //    surfaces this turn's tools/thinking.
    //
    // Prior context is genuinely lost on the fork (the server forgot it); we
    // log loudly. Surfacing that to the user would need a non-fatal warning
    // event the wire vocabulary doesn't have — out of scope here. The turn
    // still completed (a real answer streamed), so it is not failed.
    let resume_forked = is_resume && conversation_not_found(&stdout_buf, &stderr_tail);
    if conversation_id.is_none() || resume_forked {
        match capture_conversation_id(&log_file_path, &home_dir, spawn_time, &prompt) {
            CaptureOutcome::Bound(uuid) => {
                if resume_forked {
                    tracing::warn!(
                        %turn_id, agent_id = %agent_id, new_conversation = %uuid,
                        "antigravity: resumed conversation no longer exists server-side; \
                         agy forked a fresh conversation. Healing the registry locator to the \
                         new id; this turn's prior context was lost."
                    );
                }
                // Only `transcript_path` (for the final drain) and the emitted
                // locator matter from here — `conversation_id` itself isn't read
                // again, so it isn't reassigned. Emit the (possibly forked) id so
                // the dispatcher heals the registry locator; the next turn then
                // resumes the new conversation instead of re-forking the stale
                // one every turn.
                transcript_path = Some(paths::transcript_path(&home_dir, uuid));
                cursor = 0;
                emit_locator_captured(&tx, uuid);
            }
            CaptureOutcome::Ambiguous => ambiguous_capture = true,
            CaptureOutcome::NotYet => {
                // Recapture was required (first turn never bound, or a resume
                // forked) but neither the log file nor the brain-dir fallback
                // yielded this dispatch's conversation id — most likely the
                // `Created conversation <uuid>` log line moved *and* the
                // `.system_generated/` transcript path changed (or `agy` didn't
                // echo the prompt verbatim). The agent can't be made resumable;
                // fail loud rather than silently complete on the stdout answer
                // (which would leave it amnesiac every turn).
                unresumable = true;
                tracing::warn!(
                    %turn_id, agent_id = %agent_id, resume_forked,
                    "antigravity: produced output but could not identify the conversation \
                     (no id in the CLI log, no matching brain dir); the agent cannot be resumed"
                );
            }
        }
    }

    // Final transcript drain: catch records flushed between the last poll tick
    // and process exit, on the (possibly just re-pointed) transcript. The
    // answer text is transcript-sourced now (stdout replays history on resume),
    // so this read is load-bearing for every turn's visible content — retry it
    // a bounded number of times to cover the window where `agy` flushes the
    // terminal record just after exiting. Stop as soon as the answer lands, or
    // if no output appeared at all (nothing to wait for).
    if let Some(path) = &transcript_path {
        for attempt in 0..FINAL_DRAIN_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            drain_transcript(
                path,
                &mut cursor,
                turn_id,
                &mut parser_state,
                &mut saw_terminal_answer,
                &mut model,
                &tx,
            );
            if saw_terminal_answer || !saw_stdout_content {
                break;
            }
        }
    }
    let unmatched_tool_result_steps = parser_state.unmatched_tool_result_steps();
    if !unmatched_tool_result_steps.is_empty() {
        tracing::warn!(
            %turn_id,
            agent_id = %agent_id,
            unmatched_steps = ?unmatched_tool_result_steps,
            "antigravity: transcript had tool result records that never matched a tool call"
        );
    }

    // Scan the per-dispatch CLI log only when the turn produced no answer —
    // a successful turn has nothing in the log we'd want to surface. Best-
    // effort: an unreadable / missing log falls back to the generic message.
    // Load-bearing gate: `find_rpc_error_marker` matches broadly (any
    // `(code N)` status line), so this scan MUST stay gated on
    // `!saw_terminal_answer` — otherwise a stray log match could fail a turn
    // that actually produced an answer. Preserve this ordering if refactored.
    let log_error = if saw_terminal_answer {
        None
    } else {
        scan_agy_log_for_error(&log_file_path)
    };
    // Per-dispatch log cleanup. Best-effort — leaving a few KB temp file
    // around isn't load-bearing, but normal operation shouldn't leak.
    let _ = std::fs::remove_file(&log_file_path);
    let outcome = classify_outcome(
        &OutcomeSignals {
            auth_failed,
            ambiguous_capture,
            unresumable,
            saw_terminal_answer,
            saw_stdout_content,
            log_error,
        },
        &stdout_buf,
        &stderr_tail,
    );
    let _ = tx.send(AdapterEvent::TurnEnd {
        turn_id,
        outcome,
        ended_at: Utc::now(),
        usage: None,
        // Antigravity reports no token/window data — nothing to persist.
        context_window_source: None,
        stable_message_id: None,
        first_message_id: None,
        spend: None,
        // Per-turn model: this dispatch's announcement if any, else the
        // carry-forward seeded from prior turns on resume — so an unchanged
        // resume's footer matches the hydrator live, not only on reopen. `None`
        // only when no announcement has ever appeared (a truncated attach). The
        // model name embeds the effort tier, so there's no separate effort axis.
        model: model.clone(),
        effort: None,
    });

    // Post-terminal SessionMeta (mirrors Codex's enrichment ordering: flows
    // between TurnEnd and AgentIdle). Model is the dispatch's announcement or
    // the resume seed; empty only when no announcement has ever appeared — the
    // reducer's empty-model-keeps-prior rule still covers that edge so the
    // sidebar isn't blanked. MCP / skills are the display-only registries
    // loaded at dispatch time.
    let _ = tx.send(AdapterEvent::SessionMeta {
        agent_id,
        model: model.unwrap_or_default(),
        harness_version,
        tools: Vec::new(),
        mcp_servers,
        skills,
        raw: serde_json::Value::Null,
    });
}

/// Read records appended past `*cursor`, emit their live events, advance the
/// cursor, and update the terminal-answer / model side-channels. Shared
/// between the poll-tick tail and the post-exit final drain.
fn drain_transcript(
    path: &Path,
    cursor: &mut usize,
    turn_id: TurnId,
    parser_state: &mut AntigravityParserState,
    saw_terminal_answer: &mut bool,
    model: &mut Option<String>,
    tx: &tokio::sync::mpsc::UnboundedSender<AdapterEvent>,
) {
    let (records, new_cursor) = read_records_past_cursor(path, *cursor);
    *cursor = new_cursor;
    for rec in &records {
        if rec.is_terminal_answer() {
            *saw_terminal_answer = true;
        }
        // Last-wins: a new announcement this turn overwrites the resume seed
        // (and any earlier announcement in the same drain).
        if let Some(m) = extract_model_from_record(rec) {
            *model = Some(m);
        }
        for event in record_to_live_events(rec, turn_id, parser_state) {
            let _ = tx.send(event);
        }
    }
}

/// Emit the captured conversation UUID as a normalized capture event. The
/// dispatcher persists it to the running turn's agent registry record
/// (load-bearing — a persist failure fails the turn). Emitted only when the
/// locator is newly learned (first dispatch) or changes (fork-and-heal), never
/// on a plain resume.
fn emit_locator_captured(tx: &tokio::sync::mpsc::UnboundedSender<AdapterEvent>, uuid: Uuid) {
    let _ = tx.send(AdapterEvent::SessionLocatorCaptured {
        locator: SessionLocator::Uuid(uuid),
    });
}

/// Outcome of correlating a freshly-spawned dispatch to its conversation
/// directory.
#[derive(Debug)]
enum CaptureOutcome {
    /// The conversation id isn't readable yet — neither in the CLI log nor (in
    /// the fallback) a matching brain dir. Keep polling; the child may not have
    /// logged it yet.
    NotYet,
    /// The conversation id for this dispatch (from the log, or unambiguously
    /// from the brain-dir fallback).
    Bound(Uuid),
    /// **Fallback path only.** Two or more in-window conversations echo this
    /// prompt (e.g. identical concurrent prompts in the same cwd). Binding
    /// either would risk attaching to the wrong conversation, so fail loud
    /// instead of guessing. The primary log-file path can't produce this — the
    /// id is read from this dispatch's own private log.
    Ambiguous,
}

/// Capture this dispatch's server-assigned conversation id.
///
/// **Primary: read it from `agy`'s own log file.** `agy` mints the
/// conversation server-side and logs the id to the `--log-file` we passed
/// (unique per dispatch). Reading it from *our* log is deterministic, lands in
/// ~seconds regardless of server-response/cold-start latency, and is immune to
/// cross-attribution: a concurrent or background `agy` writes its *own* log,
/// never ours, so there is no shared-resource race and no "two identical
/// prompts" ambiguity. This replaced a filesystem race that watched the shared
/// `brain/` dir and matched the prompt — which both raced the (slow, ~10s on a
/// cold start) transcript write and could collide on identical concurrent
/// prompts.
///
/// **Fallback: the prompt-correlation watch.** If the log yields nothing — the
/// `Created conversation <uuid>` line is a Google-internal debug string that
/// could move on a CLI bump — degrade to [`correlate_conversation_dir`] so
/// turns keep working (with the older concurrency caveat) instead of
/// hard-failing every turn. A CLI bump that moves the log line is caught by the
/// live test before users see it.
fn capture_conversation_id(
    log_path: &Path,
    home_dir: &Path,
    since: SystemTime,
    prompt: &str,
) -> CaptureOutcome {
    if let Some(uuid) = conversation_id_from_log(log_path) {
        return CaptureOutcome::Bound(uuid);
    }
    correlate_conversation_dir(home_dir, since, prompt)
}

/// The conversation id `agy` recorded in this dispatch's private CLI log.
///
/// `agy` logs the id it uses, e.g. (real `agy` 1.0.3, glog format):
/// - `… server.go:755] Created conversation <uuid>` — a freshly minted one
/// - `… printmode.go:130] Print mode: conversation=<uuid>, sending message` —
///   the one the message is actually sent to (first turn, resume, or the new
///   id after a fork-and-heal).
///
/// We key on either marker and return the **last** id seen, so a fork's fresh
/// conversation (logged after the stale resume id) wins. Matching is anchored
/// to the marker text — `conversationID="…"` (the unused "starting" line) does
/// not contain `conversation=` and so never false-matches.
fn conversation_id_from_log(log_path: &Path) -> Option<Uuid> {
    let content = std::fs::read_to_string(log_path).ok()?;
    let mut found: Option<Uuid> = None;
    for line in content.lines() {
        for marker in ["Created conversation ", "conversation="] {
            if let Some((_, rest)) = line.split_once(marker)
                && let Some(uuid) = uuid_prefix(rest)
            {
                found = Some(uuid);
            }
        }
    }
    found
}

/// Parse a UUID from the start of `s` — the 36-char canonical form, however it
/// is delimited afterward (`,`, space, `"`, end-of-line).
fn uuid_prefix(s: &str) -> Option<Uuid> {
    let token: String = s.chars().take(36).collect();
    Uuid::parse_str(&token).ok()
}

/// Correlate this dispatch to its server-assigned conversation by matching
/// the prompt against candidate transcripts.
///
/// `agy` mints the conversation UUID server-side and creates
/// `brain/<uuid>/`; we can't pre-mint or pass it in. Picking "newest dir" is
/// unsafe under Switchboard's concurrent multi-agent model — two same-cwd
/// dispatches could bind each other's conversation. Instead we read each
/// recent candidate's transcript and bind the one whose `USER_INPUT` echoes
/// *this* dispatch's prompt. Candidates are also mtime-windowed (created
/// at/after spawn, with slack for clock granularity) so a stale conversation
/// that happens to share prompt text can't false-match.
fn correlate_conversation_dir(home_dir: &Path, since: SystemTime, prompt: &str) -> CaptureOutcome {
    let Ok(entries) = std::fs::read_dir(paths::brain_root(home_dir)) else {
        return CaptureOutcome::NotYet;
    };
    let mut matches: Vec<Uuid> = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Some(uuid) = entry
            .file_name()
            .to_str()
            .and_then(|n| Uuid::parse_str(n).ok())
        else {
            continue;
        };
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        // Slack: a dir created microseconds before our captured `since`
        // (clock granularity) should still count.
        if mtime + Duration::from_millis(500) < since {
            continue;
        }
        if transcript_echoes_prompt(&paths::transcript_path(home_dir, uuid), prompt) {
            matches.push(uuid);
        }
    }
    match matches.len() {
        0 => CaptureOutcome::NotYet,
        1 => CaptureOutcome::Bound(matches[0]),
        _ => CaptureOutcome::Ambiguous,
    }
}

/// True if the transcript has a `USER_INPUT` record whose `<USER_REQUEST>`
/// body **exactly equals** `prompt` (surrounding whitespace trimmed).
///
/// Exact-match, not substring: `agy` embeds the prompt verbatim between
/// `<USER_REQUEST>` and `</USER_REQUEST>`, and a substring check would
/// mis-correlate when one concurrent dispatch's prompt is a substring of
/// another's (e.g. "summarize" vs "summarize the file") — reopening the
/// wrong-binding hole this correlation exists to close.
///
/// **Load-bearing assumption: `agy` echoes the dispatched prompt verbatim.**
/// Capture is now fatal on a miss (a failed correlation → `unresumable` →
/// the turn fails), so if `agy` ever reformats the prompt (reindents
/// multi-line text, escapes, truncates) every turn of every Antigravity
/// agent would fail. The verbatim assumption is verified for non-trivial
/// prompts by the `live_antigravity_adversarial_prompt_*` live test (a
/// multi-line / quoted / unicode prompt that only completes if correlation
/// matched). If that test ever shows reformatting, the fix is to compare on
/// whitespace-normalized bodies (still fail-safe for concurrency: prompts
/// differing only in whitespace collapse to `Ambiguous`, never a wrong
/// bind) — do that only with evidence, not speculatively.
fn transcript_echoes_prompt(path: &Path, prompt: &str) -> bool {
    let needle = prompt.trim();
    if needle.is_empty() {
        return false;
    }
    let (records, _) = read_records_past_cursor(path, 0);
    records.iter().any(|rec| {
        rec.record_type == "USER_INPUT"
            && rec
                .content
                .as_deref()
                .and_then(user_request_body)
                .is_some_and(|body| body == needle)
    })
}

/// Extract the text between `<USER_REQUEST>` and `</USER_REQUEST>` (trimmed)
/// from a `USER_INPUT` record's content envelope. `None` if the markers are
/// absent.
fn user_request_body(content: &str) -> Option<&str> {
    const OPEN: &str = "<USER_REQUEST>";
    const CLOSE: &str = "</USER_REQUEST>";
    let start = content.find(OPEN)? + OPEN.len();
    let rest = &content[start..];
    let end = rest.find(CLOSE)?;
    Some(rest[..end].trim())
}

/// Read complete (newline-terminated) lines past `cursor` line index, parse
/// each into a [`TranscriptRecord`], and return the records plus the new
/// cursor (total complete-line count). A trailing line not yet terminated by
/// `\n` is treated as a partial write and excluded — it'll be picked up once
/// `agy` finishes writing it. Per-line parse failures are logged and skipped
/// (the `type` vocabulary grows; we don't fail the turn on an unknown shape).
fn read_records_past_cursor(path: &Path, cursor: usize) -> (Vec<TranscriptRecord>, usize) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return (Vec::new(), cursor);
    };
    // Only consider content up to the last newline (complete lines).
    let complete = match content.rfind('\n') {
        Some(idx) => &content[..=idx],
        None => return (Vec::new(), cursor),
    };
    let lines: Vec<&str> = complete.lines().collect();
    let total = lines.len();
    if total <= cursor {
        return (Vec::new(), total);
    }
    let mut records = Vec::new();
    for line in &lines[cursor..] {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<TranscriptRecord>(line) {
            Ok(rec) => records.push(rec),
            Err(e) => {
                tracing::debug!(error = %e, "antigravity: skipping unparseable transcript line");
            }
        }
    }
    (records, total)
}

/// Seed the live carry-forward model on **resume**: fold `extract_model_from_record`
/// last-wins over the records that predate this dispatch (the first `cursor`
/// complete lines — the same boundary [`read_records_past_cursor`] advances
/// from). Antigravity only re-announces the model when it *changes*, so an
/// unchanged resume's own records carry no announcement; without this seed the
/// live `TurnEnd.model` would be `None` until reopen, disagreeing with the
/// hydrator (which walks the same prefix). Bounding by `cursor` keeps live and
/// hydrate reading the identical prefix, so they yield byte-identical models.
/// `None` when no prior announcement exists (e.g. a truncated attach) — which
/// already matches the hydrator. This dispatch's own drain overwrites it
/// last-wins if a new announcement appears this turn.
fn seed_carry_forward_model(path: &Path, cursor: usize) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let complete = &content[..=content.rfind('\n')?];
    let mut model = None;
    for line in complete.lines().take(cursor) {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<TranscriptRecord>(line)
            && let Some(m) = extract_model_from_record(&rec)
        {
            model = Some(m);
        }
    }
    model
}

/// Best-effort model extraction from a `USER_INPUT` record's
/// `<USER_SETTINGS_CHANGE>` envelope, e.g. "...changed setting
/// `Model Selection` from None to Gemini 3.5 Flash (High). ...".
///
/// **The envelope only appears when the selection changed** — which includes
/// the first turn of a fresh conversation (None → default is recorded), but
/// **not** resume turns where the model is unchanged. So the common resume
/// path yields `None` (empty model), not just parse failures. That's why the
/// emission path relies on the reducer's empty-model-keeps-prior rule rather
/// than treating empty as authoritative. This string parse is also fragile
/// to any rewording/localization of that sentence; it degrades to `None`
/// (display-only, no dispatch impact). Prefer a structured source if one
/// ever surfaces.
fn extract_model_from_record(rec: &TranscriptRecord) -> Option<String> {
    if rec.record_type != "USER_INPUT" {
        return None;
    }
    let content = rec.content.as_deref()?;
    let after = content.split("Model Selection` from ").nth(1)?;
    let to = after.split(" to ").nth(1)?;
    // Trim, in order: the " (tier)" parenthetical, a trailing newline, and
    // the sentence boundary. Splitting on ". " (period+space) — not bare
    // "." — keeps version numbers like "3.5" intact.
    let raw = to.split(" (").next().unwrap_or(to);
    let raw = raw.split('\n').next().unwrap_or(raw);
    let raw = raw.split(". ").next().unwrap_or(raw);
    let raw = raw.trim().trim_end_matches('.').trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("None") {
        None
    } else {
        Some(raw.to_owned())
    }
}

/// Post-exit signals fed to [`classify_outcome`]. Bundled into a struct so
/// the classifier stays a small-arity pure function (testable without
/// spawning `agy`).
// Independent booleans, each a distinct terminal condition the producer
// observed. Modeling them as two-variant enums or a state machine would
// obscure rather than clarify — they're orthogonal flags, not states.
#[allow(clippy::struct_excessive_bools)]
struct OutcomeSignals {
    auth_failed: bool,
    ambiguous_capture: bool,
    unresumable: bool,
    saw_terminal_answer: bool,
    saw_stdout_content: bool,
    /// Human-readable message derived from scanning this dispatch's CLI log
    /// for an `rpc error: code = …` line when no transcript terminal answer
    /// was produced. `None` when the scan was skipped (success), the file
    /// was unreadable, or no matching line was found.
    log_error: Option<String>,
}

/// True if a line reports the resume target conversation was not found
/// (`Warning: conversation "<uuid>" not found.`). Drives the producer's
/// fork-and-heal recapture path on resume — it does **not** fail the turn
/// (`agy` produced a real answer in a fresh conversation; we heal the
/// registry locator to the new id so future turns continue it).
fn is_conversation_not_found(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.contains("conversation") && l.contains("not found")
}

/// Scan both output surfaces for the conversation-not-found signal. The
/// warning has been observed on stderr, but `agy` is loose about which
/// stream carries `Warning:` lines, so we check both (the stdout buffer
/// retains `Warning:` lines, and the stderr tail is the bounded drain).
fn conversation_not_found(stdout_lines: &[String], stderr_tail: &Mutex<VecDeque<String>>) -> bool {
    if stdout_lines.iter().any(|l| is_conversation_not_found(l)) {
        return true;
    }
    stderr_tail
        .lock()
        .is_ok_and(|buf| buf.iter().any(|l| is_conversation_not_found(l)))
}

/// Scan a per-dispatch `agy` CLI log for the first RPC/status error line —
/// both the old `rpc error: code = …` gRPC shape and agy 1.0.4's
/// `<STATUS> (code N): …` shape — and return a human-readable message, or
/// `None` if the file is missing/unreadable or contains no matching line.
///
/// Why this exists: `agy` exits 0 with empty stdout/stderr on
/// `RESOURCE_EXHAUSTED` (quota) and similar RPC failures — the *only* place
/// the underlying cause appears is the per-invocation CLI log. Per-dispatch
/// log isolation (we pass `--log-file <our-path>`) means a positive match
/// always belongs to *this* turn, never a concurrent `agy`.
///
/// Known limitation: a deliberate display-only surface. The log line for
/// `RESOURCE_EXHAUSTED` actually includes a `Resets in 143h34m25s` suffix
/// (verified against captured logs); we carry that text through as part of
/// the displayed message but do **not** parse it into structured retry
/// metadata or schedule any auto-retry — those are explicit non-goals (see
/// the failure-message plan's "Out of scope" section).
fn scan_agy_log_for_error(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        let Some(idx) = find_rpc_error_marker(line) else {
            continue;
        };
        // Trim `agy`'s leading log preamble (timestamp + goroutine + file:line)
        // so the displayed message is the RPC text, not a Go log header.
        let detail = line[idx..].trim_start();
        return Some(rpc_error_to_message(detail));
    }
    None
}

/// Find the byte index where a gRPC status error begins in an `agy` log line,
/// or `None` if the line carries no recognizable RPC error.
///
/// Two `agy` formats are matched:
/// - Old gRPC plumbing: `… rpc error: code = <Code> desc = …`.
/// - agy 1.0.4: `… <STATUS> (code <NNN>): <message>` (e.g.
///   `RESOURCE_EXHAUSTED (code 429): Individual quota reached…`) — the
///   `rpc error: code = ` / `desc = ` wrapper was dropped, so anchor on the
///   ` (code <digit>` token and back up to the start of the `<STATUS>` word so
///   the surfaced detail begins at the status, not the Go log preamble.
fn find_rpc_error_marker(line: &str) -> Option<usize> {
    let lower = line.to_ascii_lowercase();
    if let Some(idx) = lower.find("rpc error: code = ") {
        return Some(idx);
    }
    let paren = lower.match_indices(" (code ").find_map(|(i, _)| {
        lower[i + " (code ".len()..]
            .starts_with(|c: char| c.is_ascii_digit())
            .then_some(i)
    })?;
    let start = line[..paren]
        .rfind(char::is_whitespace)
        .map_or(0, |s| s + 1);
    Some(start)
}

/// Map an RPC error detail line into a user-facing failure message.
///
/// Known codes get an authored prefix; the log's own descriptive tail is
/// appended verbatim because for `RESOURCE_EXHAUSTED` it carries the
/// `Resets in <duration>` info that the user wants — but as plain text
/// only, never parsed into a retry schedule. Unknown codes pass through as
/// `Antigravity error: <line>` so a never-seen-before RPC failure still
/// surfaces a meaningful root cause instead of "agy exited without
/// producing an answer".
fn rpc_error_to_message(detail: &str) -> String {
    // `agy` has been observed emitting both `ResourceExhausted` (camel-case;
    // recent CLI logs) and `RESOURCE_EXHAUSTED` (screaming-snake; older
    // captures). Normalize for matching.
    let lower = detail.to_ascii_lowercase().replace('_', "");
    if lower.contains("resourceexhausted") {
        let tail = extract_rpc_descriptive_tail(detail);
        let mut msg =
            "Antigravity quota exhausted — Google Cloud individual quota reached.".to_owned();
        if !tail.is_empty() {
            msg.push(' ');
            msg.push_str(&tail);
        }
        return msg;
    }
    // Unknown/never-observed code: run it through the same descriptive-tail
    // extraction as the quota branch — strips the `rpc error: code = … desc = `
    // plumbing or agy 1.0.4's `<STATUS> (code N): ` prefix and collapses the
    // doubled message — so the user sees the cause rather than Go boilerplate.
    format!(
        "Antigravity error: {}",
        extract_rpc_descriptive_tail(detail)
    )
}

/// Strip the `rpc error: code = <CODE> desc = ` prefix and the duplicated
/// trailing repetition `agy` logs (`"<msg>: <msg>"`), keeping only the
/// human-readable descriptive sentence. Falls back to the raw detail when
/// the shape doesn't match — the caller doesn't depend on the strip
/// succeeding, just on getting a reasonable string back.
fn extract_rpc_descriptive_tail(detail: &str) -> String {
    let after_desc = detail
        .split_once("desc = ")
        .map_or(detail, |(_, rest)| rest);
    let trimmed = after_desc.trim().trim_end_matches('.').trim();
    // `agy` logs the message doubled (`"<msg>: <msg>"`). The duplication colon
    // is the `": "` whose two period-stripped halves are equal — try every
    // boundary, because the message itself can contain a colon (agy 1.0.4's
    // `RESOURCE_EXHAUSTED (code 429): …` shape has one, so splitting on the
    // first `": "` would cut in the wrong place).
    let halved = trimmed
        .match_indices(": ")
        .find_map(|(i, _)| {
            let first = trimmed[..i].trim_end_matches('.').trim();
            let second = trimmed[i + 2..].trim_end_matches('.').trim();
            (first == second).then_some(second)
        })
        .unwrap_or(trimmed);
    // agy 1.0.4 prefixes the human message with `<STATUS> (code <N>): `; strip
    // it so the surfaced tail is just the message (the old `desc = ` form had
    // no such prefix).
    halved
        .split_once("): ")
        .filter(|(prefix, _)| prefix.contains("(code "))
        .map_or(halved, |(_, rest)| rest)
        .to_owned()
}

/// Build the terminal `TurnOutcome` from the post-exit signals. Pure so the
/// classification logic is unit-tested without spawning `agy`.
///
/// Precedence: auth fast-fail → ambiguous capture →
/// stdout `Error:` line → log-derived RPC error → unresumable-with-a-streamed-reply
/// (required recapture failed) → **transcript terminal answer → Completed** →
/// output-but-no-answer (adapter failure) → no output (adapter failure). A
/// concrete stdout `Error:` line wins over a log-derived RPC error (more direct
/// signal); the log-derived RPC error wins over the diagnostic
/// "transcript-path-may-have-changed" branch (the RPC error is the real root
/// cause). `agy` exits 0 universally, so neither the exit code nor a transcript
/// `status` drives this.
///
/// **Completed requires a transcript-derived terminal answer** (`saw_terminal_answer`).
/// stdout is no longer the displayed answer — `agy` replays the whole
/// conversation to stdout on resume, so we render only the transcript's
/// per-turn `PLANNER_RESPONSE` record (see [`record_to_live_events`]). That
/// makes `saw_stdout_content` mean only "agy produced *some* output," not "the
/// user saw an answer." So if `agy` produced output but no terminal answer
/// could be read from the transcript (e.g. the brittle `.system_generated/`
/// path changed, or the answer record never landed), we fail **loud** with an
/// actionable message rather than silently completing a blank turn — consistent
/// with the adapter's fail-loud posture elsewhere (unresumable, ambiguous,
/// fork-can't-heal). The producer gives the post-exit drain a bounded retry
/// first, so this can't misfire on a normal flush lag.
///
/// Stale-resume (conversation-not-found) is intentionally **not** a failure
/// here — the producer heals it (recaptures the forked conversation, whose
/// transcript carries the terminal answer) and the turn completes.
fn classify_outcome(
    sig: &OutcomeSignals,
    stdout_lines: &[String],
    stderr_tail: &Mutex<VecDeque<String>>,
) -> TurnOutcome {
    if sig.auth_failed {
        return TurnOutcome::Failed {
            kind: FailureKind::AuthFailure,
            message: ANTIGRAVITY_AUTH_MESSAGE.to_owned(),
        };
    }
    if sig.ambiguous_capture {
        return TurnOutcome::Failed {
            kind: FailureKind::AdapterFailure,
            message: "could not unambiguously identify this Antigravity conversation (concurrent same-directory dispatch) — retry".to_owned(),
        };
    }
    // Note: persisting the captured locator is now the dispatcher sink's job;
    // a persist failure fails the turn there, not here. The adapter only
    // classifies what it observed about the `agy` run itself.
    // A concrete `Error:` line is the real root cause and must win over the
    // generic "unresumable" classification below — e.g. a first turn that
    // timed out and so never created a conversation dir should surface the
    // timeout, not "conversation could not be located."
    if let Some(error) = first_error_line(stdout_lines) {
        let (kind, message) = if is_auth_failure_line(&error) {
            (
                FailureKind::AuthFailure,
                ANTIGRAVITY_AUTH_MESSAGE.to_owned(),
            )
        } else {
            (FailureKind::HarnessError, error)
        };
        return TurnOutcome::Failed { kind, message };
    }
    // Log-derived RPC error: the only place `RESOURCE_EXHAUSTED` / network
    // failures surface (stdout/stderr stay empty and `agy` exits 0). Wins
    // over the diagnostic "transcript-path-may-have-changed" branches below
    // because it names the real root cause; loses to a stdout `Error:` line
    // above because that's a more direct user-visible signal.
    if let Some(log_message) = sig.log_error.as_deref() {
        return TurnOutcome::Failed {
            kind: FailureKind::HarnessError,
            message: log_message.to_owned(),
        };
    }
    // Unresumable only matters when a reply actually streamed: a required
    // (re)capture couldn't locate the conversation, so completing on that
    // stdout answer would leave an agent that loses context every turn. Fail
    // loud. (When no reply streamed, fall through to the "no answer" case —
    // claiming "produced a reply" would be inaccurate.)
    if sig.unresumable && sig.saw_stdout_content {
        return TurnOutcome::Failed {
            kind: FailureKind::AdapterFailure,
            message: "Antigravity produced a reply but its conversation could not be located, so the agent cannot be resumed — retry; if it persists, the adapter's transcript path may need updating".to_owned(),
        };
    }
    if sig.saw_terminal_answer {
        return TurnOutcome::Completed;
    }
    // Output appeared on stdout but no terminal answer was readable from the
    // transcript. Since stdout is no longer rendered, completing here would
    // show the user a blank "successful" turn — fail loud instead so the cause
    // (likely a `.system_generated/` transcript-path change) is visible.
    if sig.saw_stdout_content {
        return TurnOutcome::Failed {
            kind: FailureKind::AdapterFailure,
            message: "Antigravity produced output but Switchboard could not read the answer from the conversation transcript — the .system_generated/ transcript path may have changed (see docs/research/archive/antigravity-cli-observed.md); retry".to_owned(),
        };
    }
    let tail = crate::subprocess::format_stderr_tail(stderr_tail);
    let message = if tail.is_empty() {
        "agy exited without producing an answer".to_owned()
    } else {
        format!("agy exited without producing an answer; stderr: {tail}")
    };
    TurnOutcome::Failed {
        kind: FailureKind::AdapterFailure,
        message,
    }
}

fn count_complete_lines(path: &Path) -> usize {
    std::fs::read_to_string(path).map_or(0, |c| match c.rfind('\n') {
        Some(idx) => c[..=idx].lines().count(),
        None => 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn probe_binary_returns_binary_not_found_for_nonexistent_name() {
        let result = probe_binary_by_name(
            "switchboard-antigravity-probe-binary-that-definitely-does-not-exist",
        );
        assert!(matches!(result, Err(DispatchError::BinaryNotFound)));
    }

    #[test]
    fn seed_carry_forward_model_returns_last_prefix_announcement() {
        // The resume seed for the live carry-forward: an announcement on turn 1,
        // an unchanged turn 2 → seed returns the carried (tier-stripped) value.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let content = [
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:00:00Z","content":"<USER_REQUEST>\none\n</USER_REQUEST>\n<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from None to Gemini 3.1 Pro (High).</USER_SETTINGS_CHANGE>"}"#,
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:00:01Z","content":"a"}"#,
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:01:00Z","content":"<USER_REQUEST>\ntwo\n</USER_REQUEST>"}"#,
            "",
        ]
        .join("\n");
        std::fs::write(&path, &content).unwrap();
        let cursor = count_complete_lines(&path);
        assert_eq!(
            seed_carry_forward_model(&path, cursor),
            Some("Gemini 3.1 Pro".to_owned())
        );
    }

    #[test]
    fn seed_carry_forward_model_is_none_without_announcement() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let content = concat!(
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:00:00Z","content":"<USER_REQUEST>\nhi\n</USER_REQUEST>"}"#,
            "\n",
        );
        std::fs::write(&path, content).unwrap();
        let cursor = count_complete_lines(&path);
        assert_eq!(seed_carry_forward_model(&path, cursor), None);
    }

    #[test]
    fn build_args_first_turn_omits_conversation() {
        let log = PathBuf::from("/tmp/x.log");
        let args = build_args("hello", Path::new("/work/proj"), None, &log);
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "hello");
        assert!(!args.contains(&"--conversation".to_owned()));
        assert!(args.contains(&"--dangerously-skip-permissions".to_owned()));
        let idx = args.iter().position(|a| a == "--log-file").unwrap();
        assert_eq!(args[idx + 1], "/tmp/x.log");
    }

    #[test]
    fn build_args_passes_generous_print_timeout() {
        // agy's --print-timeout defaults to 5m and is a whole-turn wall-clock
        // cap; we override it so long turns aren't cut while actively working.
        let log = PathBuf::from("/tmp/x.log");
        for resume in [None, Some(Uuid::new_v4())] {
            let args = build_args("hello", Path::new("/work/proj"), resume, &log);
            let idx = args
                .iter()
                .position(|a| a == "--print-timeout")
                .expect("must pass --print-timeout");
            assert_eq!(args[idx + 1], "24h");
        }
    }

    #[test]
    fn build_args_establishes_the_workspace_via_add_dir() {
        // `--add-dir <cwd>` is load-bearing: without it `agy` runs the model's
        // file/command tools against $HOME, not the project dir.
        let log = PathBuf::from("/tmp/x.log");
        let args = build_args("hello", Path::new("/work/proj"), None, &log);
        let idx = args
            .iter()
            .position(|a| a == "--add-dir")
            .expect("must pass --add-dir");
        assert_eq!(args[idx + 1], "/work/proj");
    }

    #[test]
    fn build_args_resume_passes_conversation_uuid() {
        let uuid = Uuid::new_v4();
        let log = PathBuf::from("/tmp/y.log");
        let args = build_args("hi", Path::new("/work/proj"), Some(uuid), &log);
        let idx = args.iter().position(|a| a == "--conversation").unwrap();
        assert_eq!(args[idx + 1], uuid.to_string());
        // Workspace is re-established on resume too (agy is one-shot).
        assert!(args.contains(&"--add-dir".to_owned()));
    }

    #[test]
    fn build_args_never_emits_model_or_effort_flags() {
        // Antigravity has no per-invocation model/effort control — neither flag
        // should ever appear, on first turn or resume.
        let log = PathBuf::from("/tmp/x.log");
        for resume in [None, Some(Uuid::new_v4())] {
            let args = build_args("hello", Path::new("/work/proj"), resume, &log);
            assert!(
                !args.iter().any(|a| a == "-m" || a == "--model"),
                "{args:?}"
            );
            assert!(!args.iter().any(|a| a == "--effort"), "{args:?}");
            assert!(
                !args.iter().any(|a| a.contains("reasoning")),
                "no reasoning/effort config: {args:?}"
            );
        }
    }

    #[test]
    fn build_log_file_path_is_unique_per_turn() {
        let a = build_log_file_path(Uuid::new_v4());
        let b = build_log_file_path(Uuid::new_v4());
        assert_ne!(a, b, "different turn ids must yield different log paths");
        assert!(
            a.starts_with(std::env::temp_dir()),
            "log path lives under temp dir"
        );
    }

    #[tokio::test]
    async fn dispatch_rejects_empty_prompt_before_spawn() {
        let adapter = AntigravityAdapter::with_binary_path("/nonexistent");
        let agent = AgentRecord {
            model: None,
            effort: None,
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            name: "a".to_owned(),
            harness: switchboard_core::HarnessKind::Antigravity,
            session_locator: None,
            created_at: Utc::now(),
        };
        let cwd = TempDir::new().unwrap();
        let result = adapter
            .dispatch(
                &agent,
                cwd.path(),
                "   ",
                Uuid::new_v4(),
                DispatchOptions::default(),
            )
            .await;
        assert!(matches!(result, Err(DispatchError::InvalidPrompt(_))));
    }

    /// Stage a `brain/<uuid>/.system_generated/logs/transcript.jsonl` whose
    /// first record is a `USER_INPUT` echoing `prompt`.
    fn stage_conversation(home: &Path, uuid: Uuid, prompt: &str) {
        let path = paths::transcript_path(home, uuid);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let record = format!(
            r#"{{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","content":"<USER_REQUEST>\n{prompt}\n</USER_REQUEST>"}}"#
        );
        std::fs::write(&path, format!("{record}\n")).unwrap();
    }

    #[test]
    fn correlate_binds_the_conversation_echoing_this_prompt() {
        let home = TempDir::new().unwrap();
        let since = SystemTime::now() - Duration::from_secs(1);
        let mine = Uuid::new_v4();
        let sibling = Uuid::new_v4();
        // A concurrent same-cwd dispatch's conversation, with a different
        // prompt — must NOT be bound.
        stage_conversation(home.path(), sibling, "some other agent's prompt");
        stage_conversation(home.path(), mine, "remember the word mango");

        match correlate_conversation_dir(home.path(), since, "remember the word mango") {
            CaptureOutcome::Bound(uuid) => assert_eq!(uuid, mine),
            other => panic!("expected Bound(mine), got {other:?}"),
        }
    }

    #[test]
    fn correlate_not_yet_when_no_transcript_matches() {
        let home = TempDir::new().unwrap();
        let since = SystemTime::now() - Duration::from_secs(1);
        // Brain root exists but no conversation echoes our prompt yet.
        std::fs::create_dir_all(paths::brain_root(home.path())).unwrap();
        stage_conversation(home.path(), Uuid::new_v4(), "a different prompt");
        assert!(matches!(
            correlate_conversation_dir(home.path(), since, "our prompt"),
            CaptureOutcome::NotYet
        ));
    }

    #[test]
    fn correlate_ambiguous_when_two_conversations_echo_the_same_prompt() {
        let home = TempDir::new().unwrap();
        let since = SystemTime::now() - Duration::from_secs(1);
        // Identical concurrent prompts in the same cwd — unbindable, fail loud.
        stage_conversation(home.path(), Uuid::new_v4(), "identical prompt");
        stage_conversation(home.path(), Uuid::new_v4(), "identical prompt");
        assert!(matches!(
            correlate_conversation_dir(home.path(), since, "identical prompt"),
            CaptureOutcome::Ambiguous
        ));
    }

    #[test]
    fn correlate_exact_match_does_not_bind_on_prompt_substring() {
        // A concurrent dispatch whose prompt is a *superset* of ours must not
        // be mis-bound: "summarize" must not match a transcript echoing
        // "summarize the file". Exact-match on the <USER_REQUEST> body, not
        // substring, is what closes this hole.
        let home = TempDir::new().unwrap();
        let since = SystemTime::now() - Duration::from_secs(1);
        stage_conversation(home.path(), Uuid::new_v4(), "summarize the file");
        assert!(matches!(
            correlate_conversation_dir(home.path(), since, "summarize"),
            CaptureOutcome::NotYet
        ));
    }

    /// Write a realistic `agy` CLI log naming the conversation it created.
    fn write_log(path: &Path, uuid: Uuid) {
        std::fs::write(
            path,
            format!(
                "I0529 server.go:734] Conversation using project ID: proj\n\
                 I0529 server.go:755] Created conversation {uuid}\n\
                 I0529 printmode.go:130] Print mode: conversation={uuid}, sending message\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn conversation_id_read_from_log_is_the_logged_uuid() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("agy.log");
        let uuid = Uuid::new_v4();
        write_log(&log, uuid);
        assert_eq!(conversation_id_from_log(&log), Some(uuid));
    }

    #[test]
    fn conversation_id_from_log_ignores_the_empty_starting_line() {
        // The first-turn "starting" line carries `conversationID=""` (note the
        // `ID=`, not `conversation=`) — it must not be mistaken for an id.
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("agy.log");
        let uuid = Uuid::new_v4();
        std::fs::write(
            &log,
            format!(
                "I0529 printmode.go:71] Print mode: starting (promptLength=22, conversationID=\"\")\n\
                 I0529 server.go:755] Created conversation {uuid}\n"
            ),
        )
        .unwrap();
        assert_eq!(conversation_id_from_log(&log), Some(uuid));
    }

    #[test]
    fn conversation_id_from_log_returns_last_so_a_fork_wins() {
        // A fork logs the stale resume id, then mints + logs a fresh one; the
        // fresh (last) id must win so we heal to the conversation that actually
        // carries this turn's answer.
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("agy.log");
        let stale = Uuid::new_v4();
        let fresh = Uuid::new_v4();
        std::fs::write(
            &log,
            format!(
                "I0529 printmode.go:130] Print mode: conversation={stale}, sending message\n\
                 I0529 server.go] Warning: conversation \"{stale}\" not found.\n\
                 I0529 server.go:755] Created conversation {fresh}\n\
                 I0529 printmode.go:130] Print mode: conversation={fresh}, sending message\n"
            ),
        )
        .unwrap();
        assert_eq!(conversation_id_from_log(&log), Some(fresh));
    }

    #[test]
    fn conversation_id_from_log_ignores_a_half_written_final_line() {
        // The producer reads the log *while* `agy` is writing it (every poll
        // tick). A partially-flushed final id line yields fewer than 36 chars,
        // so `uuid_prefix` fails to parse and we return None and keep polling —
        // never binding a truncated-but-wrong id. Pins the "safe to read
        // mid-write" property the polling design relies on.
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("agy.log");
        // No trailing newline; the uuid is cut short mid-flush.
        std::fs::write(
            &log,
            "I0529 server.go:755] Created conversation 7b1024a8-f1df-",
        )
        .unwrap();
        assert_eq!(conversation_id_from_log(&log), None);
    }

    #[test]
    fn conversation_id_from_log_none_when_absent_or_unreadable() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("agy.log");
        std::fs::write(&log, "I0529 server.go] nothing relevant here\n").unwrap();
        assert_eq!(conversation_id_from_log(&log), None);
        // Missing file → None (not an error).
        assert_eq!(
            conversation_id_from_log(&dir.path().join("missing.log")),
            None
        );
    }

    #[test]
    fn capture_prefers_the_log_and_is_immune_to_a_same_prompt_sibling() {
        // The log names *our* conversation directly. Even with a sibling
        // conversation echoing the identical prompt on disk (which would make
        // the brain-dir fallback `Ambiguous`), capture binds our logged id with
        // no ambiguity — the concurrency-safety the log path buys.
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let log = dir.path().join("agy.log");
        let mine = Uuid::new_v4();
        write_log(&log, mine);
        let since = SystemTime::now() - Duration::from_secs(1);
        stage_conversation(home.path(), Uuid::new_v4(), "identical prompt");
        stage_conversation(home.path(), mine, "identical prompt");
        match capture_conversation_id(&log, home.path(), since, "identical prompt") {
            CaptureOutcome::Bound(uuid) => assert_eq!(uuid, mine),
            other => panic!("expected Bound(mine) from the log, got {other:?}"),
        }
    }

    #[test]
    fn capture_falls_back_to_brain_dir_when_the_log_has_no_id() {
        // If the log line ever moves (no id parseable), capture degrades to the
        // prompt-correlation watch rather than hard-failing.
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let log = dir.path().join("agy.log");
        std::fs::write(&log, "I0529 some.go] format changed, no id here\n").unwrap();
        let mine = Uuid::new_v4();
        let since = SystemTime::now() - Duration::from_secs(1);
        stage_conversation(home.path(), mine, "remember mango");
        match capture_conversation_id(&log, home.path(), since, "remember mango") {
            CaptureOutcome::Bound(uuid) => assert_eq!(uuid, mine),
            other => panic!("expected fallback Bound(mine), got {other:?}"),
        }
    }

    #[test]
    fn user_request_body_extracts_trimmed_inner_text() {
        assert_eq!(
            user_request_body("<USER_REQUEST>\nhello world\n</USER_REQUEST>\n<META>x</META>"),
            Some("hello world")
        );
        assert_eq!(user_request_body("no envelope here"), None);
    }

    #[test]
    fn is_conversation_not_found_matches_warning_case_insensitively() {
        assert!(is_conversation_not_found(
            "Warning: conversation \"abc\" not found."
        ));
        assert!(is_conversation_not_found("CONVERSATION NOT FOUND"));
        assert!(!is_conversation_not_found("ack"));
        assert!(!is_conversation_not_found("Error: empty prompt."));
    }

    #[test]
    fn conversation_not_found_scans_both_surfaces() {
        // stdout-only
        let stdout = vec!["Warning: conversation \"x\" not found.".to_owned()];
        assert!(conversation_not_found(
            &stdout,
            &Mutex::new(VecDeque::new())
        ));
        // stderr-only
        let stderr: Mutex<VecDeque<String>> =
            Mutex::new(["Warning: conversation \"y\" not found.".to_owned()].into());
        assert!(conversation_not_found(&[], &stderr));
        // neither
        assert!(!conversation_not_found(&[], &Mutex::new(VecDeque::new())));
    }

    #[test]
    fn correlate_ignores_conversations_older_than_since() {
        let home = TempDir::new().unwrap();
        let uuid = Uuid::new_v4();
        stage_conversation(home.path(), uuid, "our prompt");
        // `since` far in the future → the just-created dir is "too old".
        let since = SystemTime::now() + Duration::from_mins(1);
        assert!(matches!(
            correlate_conversation_dir(home.path(), since, "our prompt"),
            CaptureOutcome::NotYet
        ));
    }

    #[test]
    fn correlate_missing_brain_root_returns_not_yet() {
        let home = TempDir::new().unwrap();
        // No brain/ dir created at all.
        assert!(matches!(
            correlate_conversation_dir(home.path(), SystemTime::now(), "p"),
            CaptureOutcome::NotYet
        ));
    }

    #[test]
    fn read_records_past_cursor_skips_consumed_and_partial_lines() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("transcript.jsonl");
        // Two complete lines + a partial (no trailing newline).
        std::fs::write(
            &path,
            "{\"step_index\":0,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\"}\n\
             {\"step_index\":2,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"content\":\"ack\"}\n\
             {\"step_index\":3,\"source\":\"MODEL\",\"type\":\"RUN_COMMAND\"",
        )
        .unwrap();
        let (records, cursor) = read_records_past_cursor(&path, 0);
        assert_eq!(records.len(), 2, "partial third line excluded");
        assert_eq!(cursor, 2);
        // Re-read past the new cursor: nothing new (partial still partial).
        let (more, cursor2) = read_records_past_cursor(&path, cursor);
        assert!(more.is_empty());
        assert_eq!(cursor2, 2);
    }

    #[test]
    fn read_records_past_cursor_missing_file_returns_cursor_unchanged() {
        let tmp = TempDir::new().unwrap();
        let (records, cursor) = read_records_past_cursor(&tmp.path().join("absent.jsonl"), 7);
        assert!(records.is_empty());
        assert_eq!(cursor, 7);
    }

    #[test]
    fn extract_model_from_user_settings_envelope() {
        let rec: TranscriptRecord = serde_json::from_str(
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","content":"<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from None to Gemini 3.5 Flash (High). Done.</USER_SETTINGS_CHANGE>"}"#,
        )
        .unwrap();
        assert_eq!(
            extract_model_from_record(&rec).as_deref(),
            Some("Gemini 3.5 Flash")
        );
    }

    #[test]
    fn extract_model_returns_none_for_non_user_input() {
        let rec: TranscriptRecord = serde_json::from_str(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","content":"ack"}"#,
        )
        .unwrap();
        assert_eq!(extract_model_from_record(&rec), None);
    }

    fn empty_tail() -> Mutex<VecDeque<String>> {
        Mutex::new(VecDeque::new())
    }

    /// Baseline signals: a clean turn that streamed an answer and saw no
    /// failure flags. Individual tests flip one field.
    fn ok_signals() -> OutcomeSignals {
        OutcomeSignals {
            auth_failed: false,
            ambiguous_capture: false,
            unresumable: false,
            saw_terminal_answer: true,
            saw_stdout_content: true,
            log_error: None,
        }
    }

    #[test]
    fn classify_outcome_auth_failed_is_auth_failure() {
        let sig = OutcomeSignals {
            auth_failed: true,
            ..ok_signals()
        };
        match classify_outcome(&sig, &[], &empty_tail()) {
            TurnOutcome::Failed {
                kind: FailureKind::AuthFailure,
                message,
            } => {
                assert!(
                    message.contains("Antigravity authentication required"),
                    "names the harness: {message}"
                );
                assert!(message.contains("run `agy`"), "names the fix: {message}");
                assert!(
                    !message.contains("reload Switchboard"),
                    "must not advise reload (reactive auth): {message}"
                );
            }
            other => panic!("expected AuthFailure, got {other:?}"),
        }
    }

    #[test]
    fn classify_outcome_ambiguous_capture_is_adapter_failure() {
        let sig = OutcomeSignals {
            ambiguous_capture: true,
            ..ok_signals()
        };
        match classify_outcome(&sig, &[], &empty_tail()) {
            TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message,
            } => assert!(message.contains("concurrent")),
            other => panic!("expected AdapterFailure, got {other:?}"),
        }
    }

    #[test]
    fn classify_outcome_unresumable_fails_even_with_stdout_answer() {
        // The key fix: a visible stdout answer does NOT make an unresumable
        // turn (required recapture failed) a success — it would leave the
        // agent amnesiac every turn. Must fail loud.
        let sig = OutcomeSignals {
            unresumable: true,
            saw_terminal_answer: false,
            saw_stdout_content: true,
            ..ok_signals()
        };
        match classify_outcome(&sig, &[], &empty_tail()) {
            TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message,
            } => assert!(message.contains("cannot be resumed")),
            other => panic!("expected AdapterFailure, got {other:?}"),
        }
    }

    #[test]
    fn classify_outcome_error_line_wins_over_unresumable() {
        // A first turn that timed out (hard Error:) and never created a
        // conversation dir must surface the timeout, not the generic
        // "conversation could not be located".
        let sig = OutcomeSignals {
            unresumable: true,
            saw_terminal_answer: false,
            saw_stdout_content: true,
            ..ok_signals()
        };
        let lines = vec!["Error: timed out waiting for response".to_owned()];
        match classify_outcome(&sig, &lines, &empty_tail()) {
            TurnOutcome::Failed {
                kind: FailureKind::HarnessError,
                message,
            } => assert!(message.contains("timed out")),
            other => panic!("expected HarnessError (the real cause), got {other:?}"),
        }
    }

    #[test]
    fn classify_outcome_unresumable_without_stdout_is_no_answer() {
        // unresumable but nothing streamed → "produced a reply" would be a
        // lie; fall through to the honest no-answer failure.
        let sig = OutcomeSignals {
            unresumable: true,
            saw_terminal_answer: false,
            saw_stdout_content: false,
            ..ok_signals()
        };
        match classify_outcome(&sig, &[], &empty_tail()) {
            TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message,
            } => assert!(message.contains("without producing an answer")),
            other => panic!("expected no-answer AdapterFailure, got {other:?}"),
        }
    }

    #[test]
    fn classify_outcome_conversation_not_found_does_not_fail_the_turn() {
        // Stale-resume is healed by the producer (recapture), not failed by
        // the classifier. A "conversation not found" line plus a streamed
        // answer still classifies Completed.
        let tail = Mutex::new(VecDeque::from([
            "Warning: conversation \"abc\" not found.".to_owned()
        ]));
        assert!(matches!(
            classify_outcome(&ok_signals(), &[], &tail),
            TurnOutcome::Completed
        ));
    }

    #[test]
    fn classify_outcome_error_line_is_harness_error() {
        let lines = vec!["Error: timed out waiting for response".to_owned()];
        let sig = ok_signals();
        match classify_outcome(&sig, &lines, &empty_tail()) {
            TurnOutcome::Failed {
                kind: FailureKind::HarnessError,
                message,
            } => assert!(message.contains("timed out")),
            other => panic!("expected HarnessError, got {other:?}"),
        }
    }

    #[test]
    fn classify_outcome_auth_error_line_is_auth_failure() {
        let lines = vec!["Error: authentication timed out.".to_owned()];
        let sig = ok_signals();
        match classify_outcome(&sig, &lines, &empty_tail()) {
            TurnOutcome::Failed {
                kind: FailureKind::AuthFailure,
                message,
            } => {
                // Even when the raw stdout `Error:` line is the trigger, the
                // authored message wins so the user gets uniform actionable
                // text.
                assert!(message.contains("run `agy`"));
                assert!(!message.contains("reload Switchboard"));
            }
            other => panic!("expected AuthFailure, got {other:?}"),
        }
    }

    #[test]
    fn classify_outcome_log_error_surfaces_as_harness_error() {
        // RESOURCE_EXHAUSTED log line scanned by the producer reaches
        // classify_outcome via OutcomeSignals.log_error.
        let sig = OutcomeSignals {
            saw_terminal_answer: false,
            saw_stdout_content: false,
            log_error: Some("Antigravity quota exhausted — Google Cloud individual quota reached. Resets in 143h34m25s.".to_owned()),
            ..ok_signals()
        };
        match classify_outcome(&sig, &[], &empty_tail()) {
            TurnOutcome::Failed {
                kind: FailureKind::HarnessError,
                message,
            } => {
                assert!(message.contains("quota exhausted"));
                assert!(message.contains("Resets in"));
            }
            other => panic!("expected HarnessError from log_error, got {other:?}"),
        }
    }

    #[test]
    fn classify_outcome_log_error_wins_over_unresumable() {
        // A real RPC failure is the root cause; the diagnostic
        // "transcript path may have changed" branch must not mask it.
        let sig = OutcomeSignals {
            saw_terminal_answer: false,
            saw_stdout_content: true,
            unresumable: true,
            log_error: Some("Antigravity quota exhausted.".to_owned()),
            ..ok_signals()
        };
        match classify_outcome(&sig, &[], &empty_tail()) {
            TurnOutcome::Failed {
                kind: FailureKind::HarnessError,
                message,
            } => assert!(message.contains("quota exhausted")),
            other => panic!("expected HarnessError, got {other:?}"),
        }
    }

    #[test]
    fn classify_outcome_stdout_error_wins_over_log_error() {
        // A stdout `Error:` line is more direct (the user-visible signal `agy`
        // itself printed); it should win over a log-derived RPC message.
        let sig = OutcomeSignals {
            saw_terminal_answer: false,
            saw_stdout_content: true,
            log_error: Some("rpc error noise".to_owned()),
            ..ok_signals()
        };
        let stdout = vec!["Error: timed out waiting for response".to_owned()];
        match classify_outcome(&sig, &stdout, &empty_tail()) {
            TurnOutcome::Failed {
                kind: FailureKind::HarnessError,
                message,
            } => assert!(message.contains("timed out")),
            other => panic!("expected stdout-derived HarnessError, got {other:?}"),
        }
    }

    #[test]
    fn scan_agy_log_resource_exhausted_returns_authored_message_with_log_tail() {
        let tmp = TempDir::new().unwrap();
        let log = tmp.path().join("agy.log");
        std::fs::write(
            &log,
            "E0526 13:59:05.636054  1754 log.go:398] agent executor error: rpc error: code = ResourceExhausted desc = Individual quota reached. Contact your administrator to enable overages. Resets in 143h34m25s.: Individual quota reached. Contact your administrator to enable overages. Resets in 143h34m25s.\n",
        )
        .unwrap();
        let msg = scan_agy_log_for_error(&log).expect("log scan must find the error");
        assert!(
            msg.starts_with("Antigravity quota exhausted"),
            "authored prefix: {msg}"
        );
        assert!(
            msg.contains("Resets in 143h34m25s"),
            "carries the reset-time tail from the log line: {msg}"
        );
        assert!(
            !msg.contains("rpc error: code"),
            "Go log preamble stripped: {msg}"
        );
        // The `agy` log doubles the descriptive sentence on either side of a
        // `: ` separator. The dedup must collapse it so the user sees one
        // copy, not a run-on.
        assert_eq!(
            msg.matches("Individual quota reached").count(),
            1,
            "duplicate descriptive sentence must be collapsed: {msg}"
        );
    }

    #[test]
    fn scan_agy_log_resource_exhausted_new_1_0_4_format_returns_authored_message() {
        // agy 1.0.4 dropped the gRPC `rpc error: code = ResourceExhausted desc
        // = …` plumbing for `RESOURCE_EXHAUSTED (code 429): …`. Without matching
        // the new shape the quota error went undetected and surfaced as the
        // generic "transcript path may have changed" failure. This is the real
        // captured line (Resets value redacted to a fixed duration).
        let tmp = TempDir::new().unwrap();
        let log = tmp.path().join("agy.log");
        std::fs::write(
            &log,
            "E0602 20:37:59.639860 15879 log.go:398] agent executor error: RESOURCE_EXHAUSTED (code 429): Individual quota reached. Contact your administrator to enable overages. Resets in 46h52m7s.: RESOURCE_EXHAUSTED (code 429): Individual quota reached. Contact your administrator to enable overages. Resets in 46h52m7s.\n",
        )
        .unwrap();
        let msg = scan_agy_log_for_error(&log).expect("new-format log scan must find the error");
        assert!(
            msg.starts_with("Antigravity quota exhausted"),
            "authored prefix: {msg}"
        );
        assert!(
            msg.contains("Resets in 46h52m7s"),
            "carries the reset-time tail: {msg}"
        );
        assert!(
            !msg.contains("(code 429)"),
            "the status-code prefix is stripped from the surfaced tail: {msg}"
        );
        assert_eq!(
            msg.matches("Individual quota reached").count(),
            1,
            "doubled descriptive sentence must be collapsed: {msg}"
        );
    }

    #[test]
    fn scan_agy_log_unknown_code_passes_through_as_authored_prefix() {
        let tmp = TempDir::new().unwrap();
        let log = tmp.path().join("agy.log");
        std::fs::write(
            &log,
            "E0526 13:59:05.636054  1754 log.go:398] rpc error: code = Internal desc = backend exploded\n",
        )
        .unwrap();
        let msg = scan_agy_log_for_error(&log).expect("scan must surface unknown codes");
        assert!(
            msg.starts_with("Antigravity error: "),
            "unknown code passes through with authored prefix: {msg}"
        );
        assert!(
            msg.contains("backend exploded"),
            "carries the descriptive tail: {msg}"
        );
        assert!(
            !msg.contains("rpc error: code = "),
            "Go RPC boilerplate is stripped: {msg}"
        );
    }

    #[test]
    fn scan_agy_log_unknown_code_new_1_0_4_format_passes_through_as_authored_prefix() {
        // Non-quota RPC failures (network/backend) in agy 1.0.4's new format
        // must also surface their cause, not the generic transcript-path error.
        let tmp = TempDir::new().unwrap();
        let log = tmp.path().join("agy.log");
        std::fs::write(
            &log,
            "E0602 20:37:59.639860 15879 log.go:398] agent executor error: UNAVAILABLE (code 503): backend exploded: UNAVAILABLE (code 503): backend exploded\n",
        )
        .unwrap();
        let msg = scan_agy_log_for_error(&log).expect("scan must surface unknown new-format codes");
        assert!(
            msg.starts_with("Antigravity error: "),
            "unknown code passes through with authored prefix: {msg}"
        );
        assert!(
            msg.contains("backend exploded"),
            "carries the descriptive tail: {msg}"
        );
        assert!(
            !msg.contains("(code 503)"),
            "the status-code prefix is stripped: {msg}"
        );
        assert_eq!(
            msg.matches("backend exploded").count(),
            1,
            "doubled descriptive sentence must be collapsed: {msg}"
        );
    }

    #[test]
    fn scan_agy_log_missing_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(scan_agy_log_for_error(&tmp.path().join("absent.log")).is_none());
    }

    #[test]
    fn scan_agy_log_no_rpc_error_returns_none() {
        let tmp = TempDir::new().unwrap();
        let log = tmp.path().join("agy.log");
        std::fs::write(&log, "I0526 13:59:05.636054 log.go:398] normal startup\n").unwrap();
        assert!(scan_agy_log_for_error(&log).is_none());
    }

    #[test]
    fn classify_outcome_terminal_answer_is_completed() {
        let sig = OutcomeSignals {
            saw_terminal_answer: true,
            saw_stdout_content: false,
            ..ok_signals()
        };
        assert!(matches!(
            classify_outcome(&sig, &[], &empty_tail()),
            TurnOutcome::Completed
        ));
    }

    #[test]
    fn classify_outcome_stdout_content_without_terminal_answer_fails_loud() {
        // stdout is no longer the displayed answer (it replays prior answers on
        // resume), so "agy produced output but no terminal answer was readable
        // from the transcript" is a render failure, not a success — fail loud
        // with an actionable message rather than completing a blank turn.
        let sig = OutcomeSignals {
            saw_terminal_answer: false,
            saw_stdout_content: true,
            ..ok_signals()
        };
        match classify_outcome(&sig, &[], &empty_tail()) {
            TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message,
            } => assert!(message.contains("could not read the answer")),
            other => panic!("expected AdapterFailure, got {other:?}"),
        }
    }

    #[test]
    fn classify_outcome_no_answer_no_error_is_adapter_failure() {
        let sig = OutcomeSignals {
            saw_terminal_answer: false,
            saw_stdout_content: false,
            ..ok_signals()
        };
        assert!(matches!(
            classify_outcome(&sig, &[], &empty_tail()),
            TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                ..
            }
        ));
    }
}
