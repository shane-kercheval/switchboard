//! Read the Codex **account's** metered quotas — every limit the logged-in
//! account holds, each named by Codex itself.
//!
//! **Why this exists.** The rollout file the adapter already parses reports
//! *one* quota per turn, carrying `limit_id: "codex"` and `limit_name: null`
//! whether it describes the account's ordinary weekly allowance or a
//! model-specific reserve — byte-identical identifiers for different pools,
//! alternating between turns. A meter built on it can only ever show whichever
//! pool the last turn happened to mention, labelled with a guess. This call
//! returns all of them at once, each with its own identity.
//!
//! **Why it lives in `crates/harness` rather than `crates/app`.** The nearest
//! precedent for a harness question with no turn behind it is
//! `check_codex_auth_impl`, which sits in `crates/app` — but that is a file
//! existence check that knows nothing about Codex beyond a path. This speaks
//! Codex's own JSON-RPC protocol and parses Codex's own wire shapes, which is
//! what this crate is for.
//!
//! **Why it is a free function and not a `HarnessAdapter` method.** The trait
//! is per-turn: it takes an agent, a working directory, a prompt and a turn id,
//! and returns an event stream. This call belongs to no agent, no project and
//! no turn. It holds no session lock, writes no journal entry and never touches
//! the dispatcher's per-agent queue.
//!
//! **Probe results in this module are measured, not sampled.** Every claim here
//! about how the server behaves rests on a repeated trial — 20 runs for the
//! stdin finding, 25 for the latency distribution, 96 for the handshake — after
//! an early two-sample probe produced a latency figure that was off by roughly
//! an order of magnitude at the tail and would have sized the timeout from it.
//! This is an undocumented protocol whose behavior can only be established
//! empirically, and a single observation of it is an anecdote. Re-measure that
//! way before changing any number these comments cite.
//!
//! **Accepted exposure.** `codex app-server` is marked `[experimental]` in the
//! CLI's own help, and the protocol is not covered by the published Codex
//! documentation — the authority is the schema the installed binary emits via
//! `codex app-server generate-json-schema`. Method names and response shapes
//! can therefore move without a version bump. The live test
//! (`live_codex_account_usage_*`) is the detector, which makes this
//! detection-after-the-fact rather than prevention. That is the same posture
//! this repo already takes toward every undocumented harness field it reads,
//! and it is accepted knowingly: the alternative is a meter that is
//! structurally incapable of being right.
//!
//! **Nothing here interprets the payload.** Rust confirms a JSON-RPC success
//! arrived and lifts out the two fields the frontend needs, verbatim. Which
//! buckets to render, how to label them and whether one is exhausted are
//! decided in `src/lib/usageWindows.ts`, which stays the single interpreting
//! layer.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// Production bound for an account usage read.
///
/// **Sized from a measured distribution, not from a best case.** 25 cold-spawn
/// calls on 0.154.0: min 0.51s, median 2.07s, p90 5.63s, max 8.28s. An earlier
/// draft of this constant cited "0.5–0.9s" — that was the fast tail of a
/// two-sample probe, and building a bound on it would have left roughly 1.8x
/// headroom over an already-observed value. The spread is real work: the call
/// reaches `OpenAI`'s backend, so it carries network latency, and the read runs
/// after every Codex turn, which is when the machine is least idle.
///
/// Generous on purpose. Exceeding it costs a refresh that the next turn
/// repeats for free; too tight a bound turns ordinary network variance into a
/// steady stream of "no reading" and a warning to match. Bounded at all for the
/// same reason `VERSION_PROBE_TIMEOUT` is: a hung child would otherwise hold a
/// pipe, and a process, for the life of the app.
///
/// [`read_account_usage`] documents exactly what the bound covers.
pub const ACCOUNT_USAGE_TIMEOUT: Duration = Duration::from_secs(30);

/// The account's quota state, as Codex reports it.
///
/// Both fields are passed through verbatim from the JSON-RPC result. The
/// per-bucket value is [`serde_json::Value`] rather than a typed snapshot on
/// purpose: typing it here would mean this crate deciding which of Codex's
/// fields matter, which is the frontend's job, and would make every additive
/// field `OpenAI` ships a Rust change before it could be rendered.
///
/// Serialized with the vendor's own key names (`ordinaryUsageAllowed`,
/// `rateLimitsByLimitId`) rather than this repo's usual `snake_case` wire
/// convention, because the frontend stores this object as an opaque payload and
/// reads it beside the vendor-shaped bucket bodies it contains. One casing
/// throughout the payload is less confusing than a `snake_case` lid on a
/// camelCase box.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAccountUsage {
    /// Whether the backend currently permits ordinary included usage.
    ///
    /// `None` means the backend did not say. The schema is explicit that
    /// clients "must not infer recovery from percentages or reset times", so an
    /// absent value is absence, never a healthy default.
    pub ordinary_usage_allowed: Option<bool>,
    /// Every metered quota, keyed by Codex's `limit_id` (`codex`,
    /// `base_model_inference`, …).
    ///
    /// **The observed keys are deliberately not hardcoded anywhere.** They are
    /// plan-dependent — a different plan can carry a different set — so this is
    /// whatever map arrived. A `BTreeMap` rather than `serde_json::Map` so the
    /// iteration order is stable, which keeps tests and logs deterministic.
    pub rate_limits_by_limit_id: BTreeMap<String, serde_json::Value>,
}

/// Why an account usage read produced no reading.
///
/// Every variant is a "keep showing the last known reading" outcome, never
/// something to put in front of the user: the call is a background refresh of a
/// meter, so its failure is the absence of a newer number, not a problem the
/// user can act on.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AccountUsageError {
    #[error("codex binary not found")]
    BinaryNotFound,
    #[error("failed to spawn `codex app-server`: {0}")]
    Spawn(std::io::Error),
    /// No matching response within the caller's bound. The child is killed
    /// before this is returned.
    #[error("`codex app-server` did not answer within {bound:?}{}", stderr_suffix(.stderr))]
    Timeout {
        bound: Duration,
        /// The server's own last words, when it managed any. A wedged child
        /// that printed a reason before hanging is the common shape, and
        /// dropping the reason leaves the one time-bounded failure as the least
        /// diagnosable one.
        stderr: String,
    },
    /// The child closed stdout (or its output became unreadable) without ever
    /// answering.
    #[error("`codex app-server` produced no answer{}{}", detail_suffix(.detail), stderr_suffix(.stderr))]
    NoResponse {
        /// How the read ended, when that is more than "cleanly". Kept
        /// *alongside* the stderr tail rather than replacing it: a severed pipe
        /// and an orderly exit are different failures and would otherwise log
        /// identically.
        detail: String,
        stderr: String,
    },
    /// Codex rejected the `initialize` handshake.
    ///
    /// Separated from [`Self::Rpc`] because the causes are opposite: this is
    /// *our* request being malformed against a protocol that moved, while
    /// `Rpc` is Codex declining a request it understood. Keeping them apart is
    /// also what surfaces the real reason — Codex answers the *usage* request
    /// with a bare "Not initialized", so folding the two together reports the
    /// consequence and discards the cause sitting in the handshake's reply.
    #[error("`codex app-server` rejected the handshake: {0}")]
    Handshake(String),
    /// Codex answered, and the answer was a JSON-RPC error.
    #[error("`codex app-server` returned an error: {0}")]
    Rpc(String),
    /// Codex answered with something that is neither a result nor an error, or
    /// whose `result` is not the documented shape. Distinct from
    /// [`Self::Rpc`]: that is Codex declining, this is the protocol having
    /// moved under us — the drift the live test exists to catch.
    #[error("`codex app-server` returned an unreadable response: {0}")]
    Malformed(String),
}

fn stderr_suffix(tail: &str) -> String {
    if tail.is_empty() {
        String::new()
    } else {
        format!(" (stderr: {tail})")
    }
}

fn detail_suffix(detail: &str) -> String {
    if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    }
}

/// JSON-RPC id for the `initialize` handshake.
const INITIALIZE_ID: i64 = 0;
/// JSON-RPC id for the usage read. Responses are matched on this rather than on
/// line position: the server interleaves unsolicited notifications (a
/// `remoteControl/status/changed` was observed arriving between the two
/// responses), so "the second object" is not the answer.
const RATE_LIMITS_ID: i64 = 1;

/// Ask Codex for the account's quota state.
///
/// Costs no quota and makes no model call, so it is safe to run on every turn
/// end; it also succeeds while the account is rate-limited, which is the state
/// it most needs to describe.
///
/// **Cancellation-safe by construction.** The work runs on a task that owns the
/// child, and dropping this future *signals* that task to stop waiting and tear
/// the child down. Both halves are needed and neither alone is enough: a
/// detached task alone would clean up only once its own bound expired, leaving
/// a server alive for up to `timeout` after the caller walked away, while a
/// cancellation signal without the detached task would abandon the teardown
/// midway. The shape this guards against is a coalescing refresh superseding an
/// in-flight read, or a panel closing, once per Codex turn — and `kill_on_drop`
/// is not a substitute, because by this module's own spawn comment it cannot
/// reach a descendant.
///
/// **What `timeout` bounds, precisely:** binary resolution, spawn, the
/// handshake, and the wait for the answer. It does **not** bound teardown,
/// which is deliberate — `terminate_then_kill` carries its own grace window
/// and an unconditional `SIGKILL`, and interrupting it is what would leak the
/// process this bound exists to reclaim. It also cannot interrupt the
/// synchronous prelude inside its own region: [`crate::subprocess::resolve_binary`]
/// stats every `PATH` entry for a relative name, so a dead network mount on
/// `PATH` hangs *inside* the bound and out of its reach. That is a pre-existing
/// property of every dispatch in this crate, named here rather than papered
/// over.
///
/// A parameter rather than a constant read inside, so the kill-on-timeout path
/// is testable without a fifteen-second test; production passes
/// [`ACCOUNT_USAGE_TIMEOUT`].
///
/// # Errors
///
/// Returns [`AccountUsageError`] for every failure mode — a missing binary, a
/// failed spawn, a rejected handshake, a timeout, a closed stream, a JSON-RPC
/// error, or a response whose shape has moved. Callers hold their previous
/// reading rather than surfacing any of them.
pub async fn read_account_usage(
    binary: &Path,
    timeout: Duration,
) -> Result<CodexAccountUsage, AccountUsageError> {
    let binary = binary.to_owned();
    let cancel = CancellationToken::new();
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move { read_on_task(&binary, timeout, &cancel).await }
    });
    // Dropping this future drops the guard, which cancels the token; the task
    // is already detached from it, so it wakes, skips the wait and runs its
    // teardown to completion. On the ordinary path the guard drops after the
    // task has already finished, and cancelling a finished token is a no-op.
    let _guard = cancel.drop_guard();
    match task.await {
        Ok(result) => result,
        // A panic in the task, which is a defect here rather than an
        // environment — logged at `error` so it cannot hide inside a refresh
        // that runs after every turn, then collapsed like any other failure so
        // it still degrades to "no reading" instead of propagating.
        Err(e) => {
            tracing::error!(error = %e, "codex account usage task did not complete");
            Err(AccountUsageError::NoResponse {
                detail: e.to_string(),
                stderr: String::new(),
            })
        }
    }
}

/// The body of [`read_account_usage`], running where a dropped caller cannot
/// interrupt it.
async fn read_on_task(
    binary: &Path,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<CodexAccountUsage, AccountUsageError> {
    // Anchored before the synchronous prelude so the bound covers everything
    // the doc claims, rather than starting once the interesting part begins.
    let deadline = tokio::time::Instant::now() + timeout;

    let resolved =
        crate::subprocess::resolve_binary(binary).map_err(|_| AccountUsageError::BinaryNotFound)?;

    let mut command = tokio::process::Command::new(&resolved);
    command
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::subprocess::apply_path_env(&mut command);
    // Own process group, like a dispatched turn, so teardown can signal the
    // whole tree rather than just the process we spawned. The probe saw no
    // descendants, but "saw none once" is not a guarantee for a server process,
    // and the cost of being wrong is an orphan holding a pipe.
    #[cfg(unix)]
    command.process_group(0);

    // No `current_dir`: this call belongs to no project, so there is no working
    // directory that would be correct to impose. The child inherits the app's.
    //
    // That absence is also why `NotFound` is unambiguous here and
    // `subprocess::map_spawn_error` is not used: it exists to tell a missing
    // binary apart from a working directory that no longer exists, and this
    // spawn sets no working directory. `resolve_binary` trusts an absolute path
    // without probing it, so this is the branch that catches one.
    let mut child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AccountUsageError::BinaryNotFound
        } else {
            AccountUsageError::Spawn(e)
        }
    })?;

    let result = tokio::select! {
        result = converse(&mut child, deadline, timeout) => result,
        () = cancel.cancelled() => Err(AccountUsageError::NoResponse {
            detail: "the caller abandoned the read".to_owned(),
            stderr: String::new(),
        }),
    };
    // Kill on *every* exit path, success included. The server holds stdio open
    // and waits for more requests; without this it outlives the answer it gave
    // us. Deliberately outside the deadline — see the bound's scope above.
    crate::subprocess::terminate_then_kill(&mut child).await;
    result
}

/// The handshake-and-read, factored out so the caller can kill the child on
/// every return path with one call rather than at each `?`.
///
/// `bound` is carried alongside `deadline` only so a timeout can report the
/// budget it exhausted; the deadline is what actually governs.
async fn converse(
    child: &mut tokio::process::Child,
    deadline: tokio::time::Instant,
    bound: Duration,
) -> Result<CodexAccountUsage, AccountUsageError> {
    let missing = || AccountUsageError::NoResponse {
        detail: "the child exposed no pipes".to_owned(),
        stderr: String::new(),
    };
    let stdout = child.stdout.take().ok_or_else(missing)?;
    let stderr = child.stderr.take();
    let mut stdin = child.stdin.take().ok_or_else(missing)?;

    let tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
    let draining = stderr.map(|stderr| {
        let tail = Arc::clone(&tail);
        // Drained rather than nulled so a spawn that fails *after* exec (a
        // panicking binary, a shim that errors) leaves a reason in the log.
        // Drained rather than left unread so a chatty build cannot fill the
        // pipe and wedge the child mid-answer. The loop itself is the shared
        // one: a private copy of it is how this module lost the read-error
        // branch and the tail's length bound the first time around.
        tokio::spawn(async move {
            if let Err(e) = crate::subprocess::drain_stderr_into_tail(stderr, tail, |line| {
                tracing::debug!("codex app-server stderr: {line}");
            })
            .await
            {
                tracing::warn!(error = %e, "codex app-server stderr read error");
            }
        })
    });

    let requests = format!("{}\n{}\n", initialize_request(), rate_limits_request());
    // A write failure means the child is already gone; read stdout anyway
    // rather than returning here, so whatever it managed to say still reaches
    // the error message.
    let _ = stdin.write_all(requests.as_bytes()).await;
    let _ = stdin.flush().await;

    // **stdin stays open until the answer arrives**, and this is load-bearing
    // rather than incidental tidiness. Closing it after the write — the obvious
    // move, since we send nothing else — makes the server shut down *before*
    // answering: on 0.154.0, 20 runs closing stdin produced no `id:1` response
    // and 20 runs leaving it open answered every time. The child is reaped by
    // the caller's kill, not by EOF on its input.
    let answer = match tokio::time::timeout_at(deadline, read_response(stdout)).await {
        Ok(result) => result,
        Err(_) => Err(AccountUsageError::Timeout {
            bound,
            stderr: String::new(),
        }),
    };
    drop(stdin);

    // Both failure shapes want the server's own last words, and neither can
    // read them until the drain has caught up: stdout ending says nothing about
    // whether the stderr task is done, so an unsettled tail is a coin flip —
    // worse than no tail, because an intermittently empty reason reads as "there
    // was no reason." Bounded, because the child need not have closed stderr.
    let needs_reason = matches!(
        answer,
        Err(AccountUsageError::Timeout { .. } | AccountUsageError::NoResponse { .. })
    );
    if needs_reason && let Some(draining) = draining {
        let _ = tokio::time::timeout(STDERR_SETTLE, draining).await;
    }
    match answer {
        Err(AccountUsageError::Timeout { bound, .. }) => Err(AccountUsageError::Timeout {
            bound,
            stderr: crate::subprocess::format_stderr_tail(&tail),
        }),
        Err(AccountUsageError::NoResponse { detail, .. }) => Err(AccountUsageError::NoResponse {
            detail,
            stderr: crate::subprocess::format_stderr_tail(&tail),
        }),
        other => other,
    }
}

/// How long to wait for the stderr drain to finish once the read has ended.
/// Reached only on the failure paths, and only to make a log line complete.
const STDERR_SETTLE: Duration = Duration::from_millis(500);

/// Read stdout until one of the two requests is answered.
///
/// **Both ids are watched, not just the usage read's.** A rejected handshake is
/// answered on [`INITIALIZE_ID`] with the actual reason — a missing or renamed
/// field — while the usage request is answered with a bare "Not initialized"
/// that names only the consequence. Watching the handshake is what puts the
/// cause in the log, and it is also the only thing that ends the call promptly
/// if a future server ever stops answering the second request at all.
///
/// The stderr tail is attached by the caller, which is where the drain can be
/// settled first.
async fn read_response(
    stdout: tokio::process::ChildStdout,
) -> Result<CodexAccountUsage, AccountUsageError> {
    // Byte-oriented, then lossy. `lines()` aborts the whole read on invalid
    // UTF-8, which would contradict `classify_line`'s deliberate tolerance of
    // arbitrary non-JSON on this stream one function down — a channel we share
    // with whatever the binary (or a shim around it) prints. Tolerating garbage
    // text but not garbage bytes has no principle behind it, and the cost of
    // the strict version is losing a valid answer that arrives after the noise.
    let mut chunks = tokio::io::BufReader::new(stdout).split(b'\n');
    loop {
        match chunks.next_segment().await {
            Ok(Some(chunk)) => {
                let line = String::from_utf8_lossy(&chunk);
                if let Some(verdict) = classify_line(&line) {
                    return verdict;
                }
            }
            Ok(None) => {
                return Err(AccountUsageError::NoResponse {
                    detail: String::new(),
                    stderr: String::new(),
                });
            }
            Err(e) => {
                return Err(AccountUsageError::NoResponse {
                    detail: e.to_string(),
                    stderr: String::new(),
                });
            }
        }
    }
}

/// The `initialize` handshake. Required before any other method; the response
/// is not read for content, only skipped on the way to ours.
///
/// **The `initialized` notification that a full client would send next is
/// deliberately omitted** — probed, the usage read succeeds without it. Nothing
/// here opens a thread or a session, so there is no negotiated state to
/// complete.
fn initialize_request() -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": INITIALIZE_ID,
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "switchboard",
                "title": "Switchboard",
                "version": env!("CARGO_PKG_VERSION"),
            }
        }
    })
    .to_string()
}

/// The usage read.
///
/// `excludeResetCreditDetails` is set because Switchboard renders no reset
/// credits: the schema describes the flag as the one background usage polls
/// should use, and setting it skips a separate backend lookup whose result we
/// would discard. The available *count* still arrives either way.
///
/// **`supportsLunaReserve` is deliberately not set.** It declares that the
/// client implements automatic fallback to the reserve model and lets the
/// backend record experiment exposure against the account. Switchboard does no
/// such fallback, so claiming it would be a false statement about this client
/// with a side effect on the user's account.
fn rate_limits_request() -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": RATE_LIMITS_ID,
        "method": "account/rateLimits/read",
        "params": { "excludeResetCreditDetails": true }
    })
    .to_string()
}

/// Decide what one line of the server's stdout means.
///
/// `None` is "not our answer — keep reading", which covers a *successful*
/// handshake reply, unsolicited notifications, and any non-JSON the process
/// prints. Non-JSON is skipped rather than failed on because stdout is a
/// channel we share with whatever the binary (or a wrapper around it) decides
/// to print; the id match is the real discriminator.
///
/// A *failed* handshake is an answer, on the other id. See [`read_response`].
fn classify_line(line: &str) -> Option<Result<CodexAccountUsage, AccountUsageError>> {
    let message: serde_json::Value = serde_json::from_str(line).ok()?;
    let id = message.get("id")?.as_i64()?;
    if id == INITIALIZE_ID {
        // Only the failure is interesting. The successful reply carries the
        // server's user agent and home directory, neither of which we use.
        return message
            .get("error")
            .map(|error| Err(AccountUsageError::Handshake(error.to_string())));
    }
    if id != RATE_LIMITS_ID {
        return None;
    }
    if let Some(error) = message.get("error") {
        return Some(Err(AccountUsageError::Rpc(error.to_string())));
    }
    let Some(result) = message.get("result") else {
        return Some(Err(AccountUsageError::Malformed(
            "response carried neither `result` nor `error`".to_owned(),
        )));
    };
    Some(lift_usage(result))
}

/// Lift the two fields we carry out of a `GetAccountRateLimitsResponse`.
///
/// **An absent bucket map beside a present legacy one is drift, not an empty
/// account** — and getting this wrong recreates the bug this module exists to
/// fix. The schema marks `rateLimitsByLimitId` optional and requires only
/// `rateLimits`, the legacy single-bucket view. So reading "field absent" as
/// "no quotas" would make the Codex meters vanish silently the day the field is
/// renamed, which is verbatim the user report that started this work. The
/// legacy field is the discriminator: when it answers and the multi-bucket view
/// does not, the protocol moved.
///
/// Rejecting that shape is also the better outcome. It collapses to "no
/// reading" at the command boundary, so the card holds its previous numbers
/// instead of clearing, and the reason reaches the log.
///
/// **The legacy field is never a data source, only a signal.** It carries one
/// unnamed quota of several under identifiers shared across different pools —
/// the exact defect this module replaces — so falling back to it would trade a
/// missing meter for a confidently mislabelled one. A Codex CLI old enough to
/// predate the multi-bucket view therefore shows no Codex meter at all. That is
/// a deliberate floor, not an oversight; see the harness limitations in
/// `README.md`.
///
/// Absent with no legacy sibling either is a genuine empty reading. A
/// `rateLimitsByLimitId` that is present and not an object is drift too.
fn lift_usage(result: &serde_json::Value) -> Result<CodexAccountUsage, AccountUsageError> {
    if !result.is_object() {
        return Err(AccountUsageError::Malformed(format!(
            "`result` is not an object: {result}"
        )));
    }
    let legacy_answered = result
        .get("rateLimits")
        .is_some_and(|legacy| !legacy.is_null());
    let buckets = match result.get("rateLimitsByLimitId") {
        None | Some(serde_json::Value::Null) if legacy_answered => {
            return Err(AccountUsageError::Malformed(
                "`rateLimitsByLimitId` is absent while the legacy `rateLimits` view answered — \
                 the multi-bucket view has moved or this Codex predates it"
                    .to_owned(),
            ));
        }
        None | Some(serde_json::Value::Null) => BTreeMap::new(),
        Some(serde_json::Value::Object(map)) => map
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        Some(other) => {
            return Err(AccountUsageError::Malformed(format!(
                "`rateLimitsByLimitId` is not an object: {other}"
            )));
        }
    };
    Ok(CodexAccountUsage {
        // `as_bool` collapses both "absent" and "null" to `None`, which is the
        // schema's own meaning for the null case: unavailable.
        ordinary_usage_allowed: result
            .get("ordinaryUsageAllowed")
            .and_then(serde_json::Value::as_bool),
        rate_limits_by_limit_id: buckets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real response captured from `codex app-server` 0.154.0 against an
    /// account whose ordinary weekly quota was spent. Trimmed of the fields
    /// this module does not carry, and with the account id replaced.
    const CAPTURED: &str = include_str!("../../tests/fixtures/codex/account-rate-limits.jsonl");

    /// Drive `classify_line` over a whole recorded stream the way
    /// `read_response` does, so the fixture tests exercise the production
    /// classification rather than a parallel copy of it.
    fn read_all(stream: &str) -> Option<Result<CodexAccountUsage, AccountUsageError>> {
        stream.lines().find_map(classify_line)
    }

    #[test]
    fn captured_response_yields_both_account_buckets() {
        let usage = read_all(CAPTURED).expect("the stream answers").unwrap();
        assert_eq!(usage.ordinary_usage_allowed, Some(false));
        assert_eq!(
            usage.rate_limits_by_limit_id.keys().collect::<Vec<_>>(),
            vec!["base_model_inference", "codex"]
        );
    }

    #[test]
    fn captured_buckets_are_passed_through_verbatim() {
        // The point of the opaque payload: fields this crate has no opinion
        // about still reach the frontend. `normalModelSlug` is the one M2
        // filters on, and nothing in Rust names it.
        let usage = read_all(CAPTURED).expect("the stream answers").unwrap();
        let reserve = &usage.rate_limits_by_limit_id["base_model_inference"];
        assert_eq!(reserve["normalModelSlug"], "gpt-5.6-luna");
        assert_eq!(reserve["limitName"], "gpt-reserve");
        assert_eq!(reserve["primary"]["usedPercent"], 5);
        assert_eq!(
            usage.rate_limits_by_limit_id["codex"]["normalModelSlug"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn unrelated_traffic_before_the_answer_is_skipped() {
        // The captured stream already contains the `initialize` response and an
        // unsolicited `remoteControl/status/changed` notification ahead of the
        // answer; assert that explicitly so a fixture edit can't quietly remove
        // the case this test exists for.
        let before = CAPTURED
            .lines()
            .take_while(|line| classify_line(line).is_none())
            .collect::<Vec<_>>();
        assert!(
            before.iter().any(|l| l.contains("\"id\":0")),
            "fixture should carry the initialize response ahead of the answer"
        );
        assert!(
            before
                .iter()
                .any(|l| l.contains("remoteControl/status/changed")),
            "fixture should carry an unsolicited notification ahead of the answer"
        );
    }

    #[test]
    fn a_response_with_no_buckets_is_an_empty_reading_not_an_error() {
        // Legitimate per the schema: only the legacy `rateLimits` field is
        // required. The frontend renders nothing; it is not a drift signal.
        let stream = r#"{"id":1,"result":{"ordinaryUsageAllowed":true}}"#;
        let usage = read_all(stream).expect("the stream answers").unwrap();
        assert_eq!(usage.ordinary_usage_allowed, Some(true));
        assert!(usage.rate_limits_by_limit_id.is_empty());
    }

    #[test]
    fn an_explicitly_null_bucket_map_is_an_empty_reading() {
        let stream = r#"{"id":1,"result":{"rateLimitsByLimitId":null}}"#;
        let usage = read_all(stream).expect("the stream answers").unwrap();
        assert!(usage.rate_limits_by_limit_id.is_empty());
        assert_eq!(usage.ordinary_usage_allowed, None);
    }

    #[test]
    fn a_null_ordinary_usage_flag_is_absence_not_a_healthy_default() {
        let stream = r#"{"id":1,"result":{"ordinaryUsageAllowed":null,"rateLimitsByLimitId":{}}}"#;
        let usage = read_all(stream).expect("the stream answers").unwrap();
        assert_eq!(usage.ordinary_usage_allowed, None);
    }

    #[test]
    fn a_vanished_bucket_map_beside_a_live_legacy_view_is_drift_not_an_empty_account() {
        // The regression that matters most in this module: reading this shape
        // as "no quotas" empties the Codex section silently, which is the
        // original user report reproduced by its own fix. `Malformed` collapses
        // to "no reading" at the command, so the card holds its last numbers.
        let stream = r#"{"id":1,"result":{"ordinaryUsageAllowed":false,"rateLimits":{"limitId":"codex","primary":{"usedPercent":100}}}}"#;
        let error = read_all(stream).expect("the stream answers").unwrap_err();
        assert!(
            matches!(error, AccountUsageError::Malformed(ref d) if d.contains("rateLimitsByLimitId")),
            "got {error:?}"
        );
    }

    #[test]
    fn a_null_legacy_view_does_not_make_an_absent_bucket_map_drift() {
        // `rateLimits` present-but-null is not the legacy view answering, so
        // there is nothing to contradict — this stays an empty reading.
        let stream = r#"{"id":1,"result":{"rateLimits":null}}"#;
        let usage = read_all(stream).expect("the stream answers").unwrap();
        assert!(usage.rate_limits_by_limit_id.is_empty());
    }

    #[test]
    fn a_rejected_handshake_reports_its_own_reason_not_the_downstream_complaint() {
        // Codex answers the *usage* request with a bare "Not initialized" after
        // a rejected handshake. Reporting that would name the consequence and
        // discard the cause sitting one line up.
        let stream = concat!(
            r#"{"error":{"code":-32600,"message":"Invalid request: missing field `clientInfo`"},"id":0}"#,
            "\n",
            r#"{"error":{"code":-32600,"message":"Not initialized"},"id":1}"#,
        );
        let error = read_all(stream).expect("the stream answers").unwrap_err();
        assert!(
            matches!(error, AccountUsageError::Handshake(ref d) if d.contains("clientInfo")),
            "got {error:?}"
        );
    }

    #[test]
    fn a_rejected_handshake_answers_even_when_nothing_follows_it() {
        // Measured against 0.154.0 the server does answer the usage request
        // after a rejected handshake — 96 trials, every one inside 0.16s,
        // including under twelve-way concurrency. This case covers the server
        // that stops doing so: without watching the handshake id, the call
        // would run out the full bound and report a wedged server instead of a
        // rejected request.
        //
        // Deliberately fixture-only. A live test on this shape would have to
        // provoke a protocol violation and assert on how the server reacts,
        // which is neither ours to promise nor stable enough to pin.
        let stream = r#"{"error":{"code":-32600,"message":"Invalid request: missing field `version`"},"id":0}"#;
        let error = read_all(stream).expect("the stream answers").unwrap_err();
        assert!(
            matches!(error, AccountUsageError::Handshake(_)),
            "got {error:?}"
        );
    }

    #[test]
    fn a_successful_handshake_is_skipped_rather_than_answered() {
        let stream = concat!(
            r#"{"id":0,"result":{"userAgent":"switchboard/0.154.0","codexHome":"/home/example/.codex"}}"#,
            "\n",
            r#"{"id":1,"result":{"rateLimitsByLimitId":{}}}"#,
        );
        assert!(read_all(stream).expect("the stream answers").is_ok());
    }

    #[test]
    fn a_jsonrpc_error_is_reported_as_rpc() {
        let stream = r#"{"id":1,"error":{"code":-32601,"message":"Method not found"}}"#;
        let error = read_all(stream).expect("the stream answers").unwrap_err();
        assert!(
            matches!(error, AccountUsageError::Rpc(ref detail) if detail.contains("Method not found")),
            "expected Rpc, got {error:?}"
        );
    }

    #[test]
    fn a_response_with_neither_result_nor_error_is_malformed() {
        let stream = r#"{"id":1,"jsonrpc":"2.0"}"#;
        let error = read_all(stream).expect("the stream answers").unwrap_err();
        assert!(
            matches!(error, AccountUsageError::Malformed(_)),
            "got {error:?}"
        );
    }

    #[test]
    fn a_non_object_bucket_map_is_malformed_rather_than_empty() {
        // The drift case the live test is meant to catch first: a shape change
        // must not read as "the account has no quotas."
        let stream = r#"{"id":1,"result":{"rateLimitsByLimitId":[]}}"#;
        let error = read_all(stream).expect("the stream answers").unwrap_err();
        assert!(
            matches!(error, AccountUsageError::Malformed(_)),
            "got {error:?}"
        );
    }

    #[test]
    fn a_non_object_result_is_malformed() {
        let stream = r#"{"id":1,"result":"ok"}"#;
        let error = read_all(stream).expect("the stream answers").unwrap_err();
        assert!(
            matches!(error, AccountUsageError::Malformed(_)),
            "got {error:?}"
        );
    }

    #[test]
    fn malformed_json_lines_are_skipped_rather_than_failed_on() {
        // stdout is shared with anything the binary (or a shim) prints. Only
        // the id match decides, so noise ahead of the answer is not a failure.
        let stream = concat!(
            "not json at all\n",
            "{ truncated\n",
            r#"{"id":1,"result":{"rateLimitsByLimitId":{}}}"#,
        );
        assert!(read_all(stream).expect("the stream answers").is_ok());
    }

    #[test]
    fn a_stream_that_never_answers_yields_no_verdict() {
        let stream = concat!(
            r#"{"id":0,"result":{}}"#,
            "\n",
            r#"{"method":"remoteControl/status/changed","params":{}}"#,
        );
        assert!(read_all(stream).is_none());
    }

    #[test]
    fn the_read_request_declares_no_luna_reserve_support() {
        // Setting `supportsLunaReserve` would tell the backend this client
        // implements automatic reserve fallback — it does not — and lets the
        // backend record experiment exposure on the user's account.
        let request: serde_json::Value = serde_json::from_str(&rate_limits_request()).unwrap();
        assert_eq!(request["method"], "account/rateLimits/read");
        assert_eq!(request["params"]["excludeResetCreditDetails"], true);
        assert!(request["params"].get("supportsLunaReserve").is_none());
    }

    #[test]
    fn usage_serializes_with_the_vendors_key_names() {
        // The frontend stores this object as the reading's payload and reads
        // these keys directly; snake_casing the lid would break that.
        let usage = CodexAccountUsage {
            ordinary_usage_allowed: Some(false),
            rate_limits_by_limit_id: BTreeMap::from([(
                "codex".to_owned(),
                serde_json::json!({"limitName": null}),
            )]),
        };
        let json = serde_json::to_value(&usage).unwrap();
        assert_eq!(json["ordinaryUsageAllowed"], false);
        assert_eq!(
            json["rateLimitsByLimitId"]["codex"]["limitName"],
            serde_json::Value::Null
        );
    }
}
