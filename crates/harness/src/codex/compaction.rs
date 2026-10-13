//! Manual compaction of a Codex conversation, through a short-lived `codex
//! app-server` ([`super::app_server`]).
//!
//! `codex exec`, which runs every other Codex turn, cannot compact: sent as a
//! prompt, `/compact` is answered by the model claiming success while nothing
//! compacts (`docs/harness-behavior.md` §3.9, "The fake-success hazard"). The
//! app-server's `thread/compact/start` is the real mechanism, and it needs the
//! thread loaded first. So the adapter starts a bare server, writes three
//! requests in one write (`initialize`, `thread/resume`,
//! `thread/compact/start`), follows the compaction turn Codex then runs, and
//! kills the server.
//!
//! Verified against codex-cli 0.162.1 (§3.9, the 2026-10-11 re-probe, and the
//! 2026-10-12 adapter probe): the request's `{}` reply is not the result. The
//! compaction runs as a turn of its own: `turn/started`, `item/started` and
//! `item/completed` for a `contextCompaction` item, then `turn/completed`.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::app_server::{self, AppServer, Message};
use super::parser::{CODEX_AUTH_MESSAGE, unwrap_error_message};
use crate::events::{AdapterEvent, FailureKind, TurnId, TurnOutcome};

/// Production bound on a compaction's startup: all three requests answered.
///
/// **It covers only work that grows little with the conversation.** Measured
/// 2026-10-12 on copies of real rollouts in a scratch `CODEX_HOME`:
/// `initialize` plus `thread/resume` with `excludeTurns: true` answered in 0.08
/// to 0.15s for both a 237 KB and a 13.5 MB rollout, and in 0.09s with an MCP
/// server configured that never answers, so resuming doesn't wait for MCP
/// servers. Through this adapter, on a copy of the largest rollout on the
/// developer's machine (214.8 MB), the compaction turn started 1.7s after
/// launch. Codex queues the compaction and answers `thread/compact/start` at
/// once (0.24s in the probes). Thirty seconds is generous over all of that, so
/// a slow machine doesn't fail a healthy compaction.
///
/// **The compaction itself has no bound**, like any Switchboard turn: it is a
/// model call over the whole conversation, and the user can always press Stop.
pub(crate) const COMPACTION_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

const INITIALIZE_ID: i64 = 0;
const RESUME_ID: i64 = 1;
const COMPACT_ID: i64 = 2;

/// What a compaction needs to know about the agent.
pub(crate) struct CompactionTarget<'a> {
    pub thread_id: &'a str,
    pub cwd: &'a Path,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
}

/// The three requests, in the order they are written.
///
/// `thread/resume` carries the settings `codex exec` would get for the agent's
/// next turn, because Codex records each turn's settings in its `turn_context`
/// and the compaction is a turn:
///
/// - The agent's model, and its effort through `config`, mirroring the `-m`
///   and `-c model_reasoning_effort=…` that `build_args` passes. Absent when
///   the agent has none, as there. Verified 2026-10-12: a compaction resumed
///   with effort `medium` on a `low` thread recorded `medium`.
/// - `sandbox: "danger-full-access"` and `approvalPolicy: "never"`, the
///   settings `--dangerously-bypass-approvals-and-sandbox` gives `codex exec`.
///   Without them the compaction turn records sandbox `read-only` (both
///   2026-10-11 probe rollouts did), and the agent card, which shows the
///   latest turn's settings, would call the agent read-only until its next
///   reply. Approval `never` also means Codex asks the client nothing.
/// - `excludeTurns: true`. Without it the reply carries the whole history, and
///   Codex sends a notice that full-history hydration is deprecated.
pub(crate) fn requests(target: &CompactionTarget<'_>) -> [String; 3] {
    let mut resume = serde_json::json!({
        "threadId": target.thread_id,
        "cwd": target.cwd.to_string_lossy(),
        "excludeTurns": true,
        "sandbox": "danger-full-access",
        "approvalPolicy": "never",
    });
    if let Some(model) = target.model {
        resume["model"] = model.into();
    }
    if let Some(effort) = target.effort {
        resume["config"] = serde_json::json!({ "model_reasoning_effort": effort });
    }
    [
        app_server::initialize_request(INITIALIZE_ID),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": RESUME_ID,
            "method": "thread/resume",
            "params": resume,
        })
        .to_string(),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": COMPACT_ID,
            "method": "thread/compact/start",
            "params": { "threadId": target.thread_id },
        })
        .to_string(),
    ]
}

/// Drive one compaction to its end and report it on `tx`.
///
/// The stream gets `Liveness` for the compaction turn's activity, then exactly
/// one `TurnEnd`, except when `cancel` fires: then the server is torn down and
/// the stream ends with no terminal, and the dispatcher synthesizes
/// `Cancelled`, as for every turn. A receiver that goes away is treated the
/// same way, so nobody is left waiting on a server nobody listens to.
///
/// `startup_deadline` bounds only the three replies; see
/// [`COMPACTION_STARTUP_TIMEOUT`]. `startup_bound` is carried only so the
/// failure can name the budget it used up.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    mut server: AppServer,
    requests: [String; 3],
    thread_id: String,
    turn_id: TurnId,
    tx: tokio::sync::mpsc::UnboundedSender<AdapterEvent>,
    cancel: CancellationToken,
    startup_deadline: tokio::time::Instant,
    startup_bound: Duration,
) {
    // stdin stays open until `shut_down`: closing it aborts the compaction
    // (`AppServer::send`).
    server.send(&requests).await;

    let mut progress = Progress::new(thread_id);
    let startup = tokio::time::sleep_until(startup_deadline);
    tokio::pin!(startup);
    let ending = loop {
        if let Some(outcome) = progress.verdict() {
            break Ending::Decided(outcome);
        }
        let message = tokio::select! {
            biased;
            () = cancel.cancelled() => break Ending::Stopped,
            () = tx.closed() => break Ending::Stopped,
            () = &mut startup, if !progress.all_answered() => break Ending::StartupTimedOut,
            message = server.next_message() => message,
        };
        match message {
            Ok(Some(message)) => {
                if progress.observe(message) {
                    let _ = tx.send(AdapterEvent::Liveness { turn_id });
                }
            }
            Ok(None) => break Ending::Exited(String::new()),
            Err(e) => break Ending::Exited(e.to_string()),
        }
    };

    server.shut_down().await;

    let outcome = match ending {
        Ending::Decided(outcome) => outcome,
        Ending::Stopped => return,
        Ending::StartupTimedOut => {
            server.settle_stderr().await;
            adapter_failure(format!(
                "codex app-server did not answer within {startup_bound:?}{}",
                stderr_suffix(&server.stderr_tail())
            ))
        }
        Ending::Exited(detail) => {
            server.settle_stderr().await;
            let detail = if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            };
            adapter_failure(format!(
                "codex app-server exited before the compaction finished{detail}{}",
                stderr_suffix(&server.stderr_tail())
            ))
        }
    };
    let _ = tx.send(AdapterEvent::TurnEnd {
        turn_id,
        outcome,
        ended_at: Utc::now(),
        // Never a usage figure. The one Codex reports for a compaction leaves
        // out its fixed instructions and tool definitions (about 17,000
        // tokens), so the context bar would drop and then jump back on the next
        // reply. The bar keeps the last real turn's reading instead.
        usage: None,
        context_window_source: None,
        stable_message_id: None,
        first_message_id: None,
        spend: None,
        model: None,
        effort: None,
    });
}

/// Why the read loop stopped.
enum Ending {
    Decided(TurnOutcome),
    /// Cancelled, or nobody is listening any more.
    Stopped,
    StartupTimedOut,
    /// stdout ended, with the read error if that is how.
    Exited(String),
}

/// Everything seen so far that decides the outcome.
struct Progress {
    thread_id: String,
    /// The reply to each request, by id. `Err` holds the failure to report.
    replies: [Option<Result<(), TurnOutcome>>; 3],
    /// The compaction turn: the first `turn/started` on this thread. A bare
    /// server resumed with `excludeTurns: true` runs no other turn, and Codex
    /// queues the compaction before it answers `thread/compact/start`
    /// (`thread_compact_start_inner`), so this turn can start, or even finish,
    /// before that reply arrives.
    turn: Option<String>,
    /// Whether that turn completed a `contextCompaction` item.
    compacted: bool,
    /// That turn's `turn/completed` payload.
    finished: Option<Value>,
}

impl Progress {
    fn new(thread_id: String) -> Self {
        Self {
            thread_id,
            replies: [None, None, None],
            turn: None,
            compacted: false,
            finished: None,
        }
    }

    fn all_answered(&self) -> bool {
        self.replies.iter().all(Option::is_some)
    }

    /// Record `message`. Returns whether it was activity on the compaction
    /// turn, which the caller reports as liveness so the row shows the
    /// compaction is still running.
    fn observe(&mut self, message: Message) -> bool {
        match message {
            Message::Reply { id, result, error } => {
                let Some(slot) = usize::try_from(id)
                    .ok()
                    .and_then(|index| self.replies.get_mut(index))
                else {
                    return false;
                };
                if slot.is_none() {
                    *slot = Some(reply_outcome(id, result.as_ref(), error.as_ref()));
                }
                false
            }
            Message::Notification { method, params } => self.observe_notification(&method, &params),
        }
    }

    /// Only `turn/*` and `item/*` notifications on the compaction turn count.
    /// Everything else is ignored, including `thread/tokenUsage/updated`,
    /// whose figure is the one the context bar must not show (see `run`), and
    /// the server's own chatter (`mcpServer/startupStatus/updated`,
    /// `account/updated`, `remoteControl/status/changed`, `deprecationNotice`).
    fn observe_notification(&mut self, method: &str, params: &Value) -> bool {
        if params.get("threadId").and_then(Value::as_str) != Some(&self.thread_id) {
            return false;
        }
        let turn = params
            .get("turnId")
            .or_else(|| params.pointer("/turn/id"))
            .and_then(Value::as_str);
        if method == "turn/started" && self.turn.is_none() {
            self.turn = turn.map(str::to_owned);
            return self.turn.is_some();
        }
        if turn.is_none() || turn != self.turn.as_deref() {
            return false;
        }
        match method {
            "item/completed"
                if params.pointer("/item/type").and_then(Value::as_str)
                    == Some("contextCompaction") =>
            {
                self.compacted = true;
            }
            "turn/completed" => {
                self.finished = params.get("turn").cloned();
                return false;
            }
            _ => {}
        }
        method.starts_with("item/") || method.starts_with("turn/")
    }

    /// The outcome, once it is known.
    ///
    /// **A failed request is reported only once every earlier request has
    /// answered, and the earliest failure wins.** The three requests go out in
    /// one write, so when `thread/resume` fails, `thread/compact/start` fails
    /// too, with a bare `thread not found` that names the consequence and not
    /// the cause. Codex runs requests about one thread one at a time, so the
    /// resume's reply comes first in practice, and this rule makes the order
    /// irrelevant. The quota read does the same for its handshake.
    ///
    /// Otherwise the outcome needs all three replies and the turn's
    /// `turn/completed`, in any order.
    fn verdict(&self) -> Option<TurnOutcome> {
        for reply in &self.replies {
            match reply.as_ref()? {
                Ok(()) => {}
                Err(failure) => return Some(failure.clone()),
            }
        }
        let finished = self.finished.as_ref()?;
        Some(turn_outcome(finished, self.compacted))
    }
}

/// What one request's reply means.
fn reply_outcome(
    id: i64,
    result: Option<&Value>,
    error: Option<&Value>,
) -> Result<(), TurnOutcome> {
    match (error, result) {
        (Some(error), _) => {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| error.to_string(), unwrap_error_message);
            if id == INITIALIZE_ID {
                // Our request is malformed against a protocol that moved, not
                // Codex declining work it understood.
                Err(adapter_failure(format!(
                    "codex app-server rejected the handshake: {message}"
                )))
            } else {
                // Codex's own text, e.g. `thread not found: <id>`, or for a
                // fork whose original's rollout is gone, `invalid paginated
                // history lineage for <id>: missing source rollout` (verified
                // 2026-10-12 on a copied fork).
                Err(TurnOutcome::Failed {
                    kind: FailureKind::HarnessError,
                    message,
                })
            }
        }
        (None, Some(_)) => Ok(()),
        (None, None) => Err(adapter_failure(format!(
            "codex app-server answered request {id} with neither a result nor an error"
        ))),
    }
}

/// What the compaction turn's `turn/completed` means.
///
/// `failed` is classified from the turn error's `codexErrorInfo`, both values
/// of which are in the 0.162.1 schema: `usageLimitExceeded` is a spent quota,
/// with Codex's text (it carries the reset time), and `unauthorized` gets the
/// same sign-in message a send shows. `completed` without a completed
/// `contextCompaction` item fails closed, as Claude Code's compaction does
/// when it reports no verdict. `interrupted` reaches here only when
/// Switchboard didn't cancel, since a cancel stops reading first; the schema
/// allows it, and nothing has shown when Codex sends it unprompted.
fn turn_outcome(turn: &Value, compacted: bool) -> TurnOutcome {
    let error_message = turn
        .pointer("/error/message")
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .map(unwrap_error_message);
    match turn.get("status").and_then(Value::as_str) {
        Some("completed") if compacted => TurnOutcome::Completed,
        Some("completed") => harness_error("compaction did not run".to_owned()),
        Some("failed") => match turn
            .pointer("/error/codexErrorInfo")
            .and_then(Value::as_str)
        {
            Some("unauthorized") => TurnOutcome::Failed {
                kind: FailureKind::AuthFailure,
                message: CODEX_AUTH_MESSAGE.to_owned(),
            },
            Some("usageLimitExceeded") => TurnOutcome::Failed {
                kind: FailureKind::UsageLimit,
                message: error_message.unwrap_or_else(|| "usage limit reached".to_owned()),
            },
            _ => harness_error(error_message.unwrap_or_else(|| "compaction failed".to_owned())),
        },
        Some("interrupted") => {
            harness_error(error_message.unwrap_or_else(|| "compaction was interrupted".to_owned()))
        }
        other => adapter_failure(format!(
            "the compaction turn ended with an unknown status: {}",
            other.unwrap_or("none")
        )),
    }
}

fn harness_error(message: String) -> TurnOutcome {
    TurnOutcome::Failed {
        kind: FailureKind::HarnessError,
        message,
    }
}

fn adapter_failure(message: String) -> TurnOutcome {
    TurnOutcome::Failed {
        kind: FailureKind::AdapterFailure,
        message,
    }
}

fn stderr_suffix(tail: &str) -> String {
    if tail.is_empty() {
        String::new()
    } else {
        format!(" (stderr: {tail})")
    }
}
