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
//! - **Server-assigned conversation UUID.** The UUID is minted server-side
//!   and discovered post-spawn by watching for a new
//!   `~/.gemini/antigravity-cli/brain/<uuid>/` directory, then persisted to
//!   the per-agent sidecar (see [`sidecar`]) for resume / hydration.
//! - **Dual-source streaming.** Live assistant text comes from stdout; tool
//!   lifecycle (`ToolStarted` / `ToolCompleted`) and `thinking` come from
//!   tailing the conversation's `transcript.jsonl` (see [`paths`]). The
//!   transcript records the *completed* `PLANNER_RESPONSE` text too, but the
//!   live path does **not** re-emit it — stdout already streamed it.
//! - **Auth fast-fail.** When the keyring token is stale, `agy -p` falls
//!   back to interactive OAuth: it prints `Authentication required...`,
//!   opens a browser, and blocks ~30s before timing out. There is no flag
//!   to suppress this. The adapter detects the `Authentication required`
//!   stdout line, force-kills immediately, and emits `AuthFailure` —
//!   bounding the hang. (The browser tab cannot be prevented; documented as
//!   a known limitation.)
//!
//! Ground-truth reference: `docs/research/antigravity-cli-observed.md`.

pub mod parser;
pub mod paths;
pub mod sidecar;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::Utc;
use switchboard_core::{AgentId, AgentRecord};
use tokio::io::AsyncBufReadExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use uuid::Uuid;

use crate::adapter::{DispatchError, DispatchOptions, EventStream, HarnessAdapter};
use crate::events::{AdapterEvent, ContentKind, FailureKind, McpServerStatus, TurnId, TurnOutcome};

use parser::{
    AntigravityParserState, TranscriptRecord, first_error_line, is_auth_failure_line,
    record_to_live_events,
};
use sidecar::{SessionLinkRecord, append_record, read_latest, sidecar_path};

/// The binary name on PATH. Centralized so the adapter and the pre-adapter
/// binary probe agree on the name.
pub const BINARY_NAME: &str = "agy";

/// Poll interval for UUID-directory discovery and transcript tailing.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long to wait for the conversation directory to appear before giving
/// up. Matches the user-visible "agent starting" pause we tolerate
/// elsewhere; an early child exit short-circuits this.
const UUID_CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

/// Verify a binary name is on PATH, mapping a miss to `BinaryNotFound`.
/// Used by [`AntigravityAdapter::probe`]; factored out so the negative-path
/// test can pass a synthetic name.
fn probe_binary_by_name(name: &str) -> Result<(), DispatchError> {
    which::which(name)
        .map(|_| ())
        .map_err(|_| DispatchError::BinaryNotFound)
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
            .get_or_init(|| fetch_version(&self.binary_path))
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

    async fn dispatch(
        &self,
        agent: &AgentRecord,
        cwd: &Path,
        prompt: &str,
        turn_id: TurnId,
        _options: DispatchOptions,
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

        // Resume target: the conversation UUID captured on a prior dispatch,
        // read from the per-agent sidecar. Corrupt sidecar is fail-loud
        // (PreStreamRead) per the AGENTS.md Switchboard-owned-JSONL
        // invariant; `Ok(None)` is the legitimate never-dispatched case.
        let sidecar_file = sidecar_path(cwd, agent.project_id, agent.id);
        let prior =
            read_latest(&sidecar_file).map_err(|e| DispatchError::PreStreamRead(e.to_string()))?;
        let resume_id = prior.as_ref().map(|r| r.conversation_id);

        let args = build_args(prompt, resume_id);

        let mut command = tokio::process::Command::new(&binary);
        command
            .args(&args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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

        tokio::spawn(run_producer(ProducerCtx {
            child,
            stdout,
            stderr,
            tx,
            turn_id,
            agent_id: agent.id,
            home_dir,
            sidecar_file,
            resume_id,
            harness_version,
            prompt: prompt.to_owned(),
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

/// Build the `agy` invocation. `--dangerously-skip-permissions` mirrors
/// Gemini's `--yolo`: Switchboard's bound cwd is the user's own workspace,
/// so per-tool approval prompts (which would block a headless dispatch)
/// must be auto-approved. Resume passes the captured UUID via
/// `--conversation`; first turn omits it and lets `agy` mint a new one.
fn build_args(prompt: &str, resume_id: Option<Uuid>) -> Vec<String> {
    let mut args = vec!["-p".to_owned(), prompt.to_owned()];
    if let Some(uuid) = resume_id {
        args.push("--conversation".to_owned());
        args.push(uuid.to_string());
    }
    args.push("--dangerously-skip-permissions".to_owned());
    args
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
    sidecar_file: PathBuf,
    resume_id: Option<Uuid>,
    harness_version: String,
    /// The dispatch prompt — used to correlate the captured conversation
    /// directory to *this* dispatch (its `USER_INPUT` record echoes the
    /// prompt), so concurrent same-cwd dispatches can't bind each other's
    /// conversation.
    prompt: String,
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
        sidecar_file,
        resume_id,
        harness_version,
        prompt,
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
    // Resume re-append: non-fatal. The prior record already holds this UUID
    // (resume passes it via `--conversation`), so a failed re-append loses
    // only debug history, not resumability. (First-turn capture, below, is
    // the load-bearing write and is fatal on failure.)
    if let Some(uuid) = resume_id
        && let Err(e) = persist_sidecar(&sidecar_file, uuid)
    {
        tracing::warn!(
            %turn_id, agent_id = %agent_id, error = %e,
            "antigravity: resume sidecar re-append failed (non-fatal; prior record holds the UUID)"
        );
    }

    let spawn_time = SystemTime::now();
    let mut conversation_id = resume_id;
    let mut transcript_path = conversation_id.map(|u| paths::transcript_path(&home_dir, u));
    // Resume: skip records already on disk from prior turns so only the new
    // turn's records emit. First turn: cursor 0, transcript not yet created.
    let mut cursor = transcript_path.as_deref().map_or(0, count_complete_lines);

    let mut parser_state = AntigravityParserState::default();
    let mut stdout_lines = tokio::io::BufReader::new(stdout).lines();
    let mut stdout_buf: Vec<String> = Vec::new();
    let mut model: Option<String> = None;
    let mut saw_terminal_answer = false;
    let mut saw_stdout_content = false;
    let mut auth_failed = false;
    let mut ambiguous_capture = false;
    let mut sidecar_write_failed = false;
    // Set post-loop when a required (re)capture could not locate the
    // conversation directory — the agent would be unresumable, so the turn
    // must fail loudly rather than silently complete on the stdout answer.
    let mut unresumable = false;
    let mut stdout_eof = false;
    let mut exit_status: Option<std::process::ExitStatus> = None;
    let capture_deadline = spawn_time + UUID_CAPTURE_TIMEOUT;

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
                            // Normal drip text → live assistant content. Also a
                            // success signal: it means agy produced a visible
                            // answer even if the transcript (the brittle
                            // `.system_generated/` path) can't be read.
                            saw_stdout_content = true;
                            let _ = tx.send(AdapterEvent::ContentChunk {
                                turn_id,
                                kind: ContentKind::Text,
                                text: format!("{line}\n"),
                            });
                        }
                    }
                    Ok(None) | Err(_) => stdout_eof = true,
                }
            }
            _ = poll.tick() => {
                if conversation_id.is_none() {
                    match correlate_conversation_dir(&home_dir, spawn_time, &prompt) {
                        CaptureOutcome::Bound(uuid) => {
                            conversation_id = Some(uuid);
                            transcript_path = Some(paths::transcript_path(&home_dir, uuid));
                            cursor = 0;
                            // First-turn capture is the load-bearing write: if
                            // it fails, the UUID exists nowhere and the agent is
                            // silently unresumable. Fail the turn loudly.
                            if persist_sidecar(&sidecar_file, uuid).is_err() {
                                sidecar_write_failed = true;
                            }
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
        }

        // Abort paths: stop immediately and force-kill below.
        if auth_failed || ambiguous_capture || sidecar_write_failed {
            break;
        }
        // Terminator: the process has exited AND stdout drained. The
        // transcript is fully flushed by now; a final drain happens below.
        if stdout_eof && exit_status.is_some() {
            break;
        }
        // First-turn UUID never appeared and the process is gone / timed out:
        // nothing more will be written. Stop waiting.
        if conversation_id.is_none()
            && (exit_status.is_some() || SystemTime::now() >= capture_deadline)
        {
            break;
        }
    }

    // Force-kill if we broke out while the child may still be running (auth
    // fast-fail, or capture timeout with the process hung on OAuth).
    if exit_status.is_none() {
        crate::subprocess::kill_subprocess_group(&mut child).await;
        let _ = child.wait().await;
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
        match correlate_conversation_dir(&home_dir, spawn_time, &prompt) {
            CaptureOutcome::Bound(uuid) => {
                if resume_forked {
                    tracing::warn!(
                        %turn_id, agent_id = %agent_id, new_conversation = %uuid,
                        "antigravity: resumed conversation no longer exists server-side; \
                         agy forked a fresh conversation. Healing the sidecar to the new id; \
                         this turn's prior context was lost."
                    );
                }
                // Only `transcript_path` (for the final drain) and the
                // persisted sidecar matter from here — `conversation_id`
                // itself isn't read again, so it isn't reassigned.
                transcript_path = Some(paths::transcript_path(&home_dir, uuid));
                cursor = 0;
                if persist_sidecar(&sidecar_file, uuid).is_err() {
                    sidecar_write_failed = true;
                }
            }
            CaptureOutcome::Ambiguous => ambiguous_capture = true,
            CaptureOutcome::NotYet => {
                // Recapture was required (first turn never bound, or a resume
                // forked) but no conversation directory matched this dispatch
                // — most likely the `.system_generated/` transcript path
                // changed, or agy didn't echo the prompt verbatim. The agent
                // can't be made resumable; fail loud rather than silently
                // complete on the stdout answer (which would leave it amnesiac
                // every turn).
                unresumable = true;
                tracing::warn!(
                    %turn_id, agent_id = %agent_id, resume_forked,
                    "antigravity: produced output but could not locate the conversation \
                     directory; the agent cannot be resumed"
                );
            }
        }
    }

    // Final transcript drain: catch any records flushed between the last poll
    // tick and process exit, on the (possibly just re-pointed) transcript.
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

    let outcome = classify_outcome(
        &OutcomeSignals {
            auth_failed,
            ambiguous_capture,
            sidecar_write_failed,
            unresumable,
            saw_terminal_answer,
            saw_stdout_content,
        },
        &stdout_buf,
        &stderr_tail,
    );
    let _ = tx.send(AdapterEvent::TurnEnd {
        turn_id,
        outcome,
        ended_at: Utc::now(),
        usage: None,
    });

    // Post-terminal SessionMeta (mirrors Codex's enrichment ordering: flows
    // between TurnEnd and AgentIdle). Model is best-effort from the user
    // record's settings envelope and is empty on resume turns that didn't
    // change the model — the reducer's empty-model-keeps-prior rule prevents
    // that from blanking the sidebar. MCP / skills are not populated by this
    // adapter yet (no loaders wired); empty lists here.
    let _ = tx.send(AdapterEvent::SessionMeta {
        agent_id,
        model: model.unwrap_or_default(),
        harness_version,
        tools: Vec::new(),
        mcp_servers: Vec::<McpServerStatus>::new(),
        skills: Vec::new(),
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
        if model.is_none()
            && let Some(m) = extract_model_from_record(rec)
        {
            *model = Some(m);
        }
        for event in record_to_live_events(rec, turn_id, parser_state) {
            let _ = tx.send(event);
        }
    }
}

/// Append a conversation-UUID record to the per-agent sidecar. Returns the
/// result so the caller decides fatality (first-turn capture is fatal;
/// resume re-append is not — see [`run_producer`]).
fn persist_sidecar(sidecar_file: &Path, uuid: Uuid) -> Result<(), sidecar::SidecarError> {
    append_record(
        sidecar_file,
        &SessionLinkRecord {
            conversation_id: uuid,
            captured_at: Utc::now(),
        },
    )
}

/// Outcome of correlating a freshly-spawned dispatch to its conversation
/// directory.
#[derive(Debug)]
enum CaptureOutcome {
    /// No candidate transcript yet contains this dispatch's prompt — keep
    /// polling (the dir/transcript may not be written yet).
    NotYet,
    /// Exactly one in-window conversation echoes this dispatch's prompt.
    Bound(Uuid),
    /// Two or more in-window conversations echo this prompt (e.g. identical
    /// concurrent prompts in the same cwd). Binding either would risk
    /// attaching to the wrong conversation, so fail loud instead of guessing.
    Ambiguous,
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
/// the classifier stays a 3-arg pure function (testable without spawning
/// `agy`).
// Five independent booleans, each a distinct terminal condition the producer
// observed. Modeling them as two-variant enums or a state machine would
// obscure rather than clarify — they're orthogonal flags, not states.
#[allow(clippy::struct_excessive_bools)]
struct OutcomeSignals {
    auth_failed: bool,
    ambiguous_capture: bool,
    sidecar_write_failed: bool,
    unresumable: bool,
    saw_terminal_answer: bool,
    saw_stdout_content: bool,
}

/// True if a line reports the resume target conversation was not found
/// (`Warning: conversation "<uuid>" not found.`). Drives the producer's
/// fork-and-heal recapture path on resume — it does **not** fail the turn
/// (`agy` produced a real answer in a fresh conversation; we heal the
/// sidecar to the new id so future turns continue it).
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

/// Build the terminal `TurnOutcome` from the post-exit signals. Pure so the
/// classification logic is unit-tested without spawning `agy`.
///
/// Precedence: auth fast-fail → ambiguous capture → sidecar write failure →
/// stdout `Error:` line → unresumable-with-a-streamed-reply (required
/// recapture failed) → a visible answer streamed (transcript terminal answer
/// OR stdout content) → no answer (adapter failure). A concrete `Error:` line
/// wins over the generic unresumable classification. `agy` exits 0
/// universally, so neither the exit code nor a transcript `status` drives
/// this.
///
/// The stdout-content success fallback is load-bearing: the transcript path
/// (`.system_generated/…`) is the most brittle contract in the adapter, and
/// the answer streams independently over stdout. Without the fallback, a
/// transcript-path break would misreport every working turn as failed.
///
/// Stale-resume (conversation-not-found) is intentionally **not** a failure
/// here — the producer heals it (recaptures the forked conversation) and the
/// turn completes with a real answer.
fn classify_outcome(
    sig: &OutcomeSignals,
    stdout_lines: &[String],
    stderr_tail: &Mutex<VecDeque<String>>,
) -> TurnOutcome {
    if sig.auth_failed {
        return TurnOutcome::Failed {
            kind: FailureKind::AuthFailure,
            message: "Antigravity authentication required — sign in via the Antigravity desktop app and reload Switchboard".to_owned(),
        };
    }
    if sig.ambiguous_capture {
        return TurnOutcome::Failed {
            kind: FailureKind::AdapterFailure,
            message: "could not unambiguously identify this Antigravity conversation (concurrent same-directory dispatch) — retry".to_owned(),
        };
    }
    if sig.sidecar_write_failed {
        return TurnOutcome::Failed {
            kind: FailureKind::AdapterFailure,
            message: "failed to persist the Antigravity conversation id; the agent would be unresumable — check .switchboard/ write permissions and retry".to_owned(),
        };
    }
    // A concrete `Error:` line is the real root cause and must win over the
    // generic "unresumable" classification below — e.g. a first turn that
    // timed out and so never created a conversation dir should surface the
    // timeout, not "conversation could not be located."
    if let Some(error) = first_error_line(stdout_lines) {
        let kind = if is_auth_failure_line(&error) {
            FailureKind::AuthFailure
        } else {
            FailureKind::HarnessError
        };
        return TurnOutcome::Failed {
            kind,
            message: error,
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
    if sig.saw_terminal_answer || sig.saw_stdout_content {
        return TurnOutcome::Completed;
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
    fn build_args_first_turn_omits_conversation() {
        let args = build_args("hello", None);
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "hello");
        assert!(!args.contains(&"--conversation".to_owned()));
        assert!(args.contains(&"--dangerously-skip-permissions".to_owned()));
    }

    #[test]
    fn build_args_resume_passes_conversation_uuid() {
        let uuid = Uuid::new_v4();
        let args = build_args("hi", Some(uuid));
        let idx = args.iter().position(|a| a == "--conversation").unwrap();
        assert_eq!(args[idx + 1], uuid.to_string());
    }

    #[tokio::test]
    async fn dispatch_rejects_empty_prompt_before_spawn() {
        let adapter = AntigravityAdapter::with_binary_path("/nonexistent");
        let agent = AgentRecord {
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            name: "a".to_owned(),
            harness: switchboard_core::HarnessKind::Antigravity,
            session_id: None,
            created_at: Utc::now(),
        };
        let cwd = TempDir::new().unwrap();
        let result = adapter
            .dispatch(
                &agent,
                cwd.path(),
                "   ",
                Uuid::now_v7(),
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
            sidecar_write_failed: false,
            unresumable: false,
            saw_terminal_answer: true,
            saw_stdout_content: true,
        }
    }

    #[test]
    fn classify_outcome_auth_failed_is_auth_failure() {
        let sig = OutcomeSignals {
            auth_failed: true,
            ..ok_signals()
        };
        assert!(matches!(
            classify_outcome(&sig, &[], &empty_tail()),
            TurnOutcome::Failed {
                kind: FailureKind::AuthFailure,
                ..
            }
        ));
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
    fn classify_outcome_sidecar_write_failed_is_adapter_failure() {
        let sig = OutcomeSignals {
            sidecar_write_failed: true,
            ..ok_signals()
        };
        match classify_outcome(&sig, &[], &empty_tail()) {
            TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message,
            } => assert!(message.contains("unresumable")),
            other => panic!("expected AdapterFailure, got {other:?}"),
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
        assert!(matches!(
            classify_outcome(&sig, &lines, &empty_tail()),
            TurnOutcome::Failed {
                kind: FailureKind::AuthFailure,
                ..
            }
        ));
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
    fn classify_outcome_stdout_content_only_is_completed() {
        // The load-bearing fallback: transcript unreadable (saw_terminal_answer
        // false) but stdout streamed a clean answer → Completed, not failure.
        // Guards against the `.system_generated/` path break misreporting
        // every working turn as failed.
        let sig = OutcomeSignals {
            saw_terminal_answer: false,
            saw_stdout_content: true,
            ..ok_signals()
        };
        assert!(matches!(
            classify_outcome(&sig, &[], &empty_tail()),
            TurnOutcome::Completed
        ));
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
