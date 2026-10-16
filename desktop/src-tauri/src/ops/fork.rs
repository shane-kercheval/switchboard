//! The turn tier for forks: `fork_agent`.
//!
//! A fork is a turn. An orchestrator branches an agent's conversation to ask
//! a side question without disturbing the original, and the branch exists
//! only with its first message: a Claude branch is made by that turn's
//! `--fork-session`, and while Codex can copy a session without a message,
//! one rule for both harnesses is simpler and the orchestrator's uses start
//! with a message. So the operation runs the send's order with the fork's
//! creation inside it: the project is opened; the message is rendered, so a
//! prompt that cannot render refuses before anything is created; the fork is
//! created through the window's own command, which refuses a busy source, a
//! source with no session, and a harness that cannot fork, and for Codex runs
//! `codex exec fork` under the parent's session lock; then the window
//! readiness round trip, carrying the fork so the window places it in its own
//! pane and reads its earlier conversation first; then the message through
//! the send's own dispatch, whose preflight re-checks a Claude fork's parent
//! at the instant the copy is taken. One task, as a send: a client that hangs
//! up mid-call cannot stop it between creation and send, and the server's
//! stop signal leaves a created fork with nothing sent rather than starting a
//! turn.
//!
//! **One retry lifecycle.** Until the fork exists, a failure releases the key
//! as a send's does. Once it exists the key is kept whatever happens to the
//! message, so a retry with the same key finds the fork instead of making a
//! second. A message refused before it dispatched (a refusal at dispatch, the
//! window's deadline, the server stopping) is recorded on the receipt as a
//! rejected recipient with its code, the fork is returned beside it, and a
//! repeat under the key sends again to the same fork under a fresh `send_id`;
//! a message that dispatched is final and a repeat returns its receipt; a
//! message cancelled while preparing settles as cancelled and is never
//! resent. Concurrent repeats join one attempt. A failed admitted turn is
//! observed through `wait_send`, never rerun here. The rule is the ledger's
//! ([`SendLedger::claim_fork`]).
//!
//! **Stopping, and the call's own deadline.** Everything before the fork's
//! creation runs under the call's budget and ends at once on a server stop:
//! the login-shell PATH wait a Codex fork needs at launch, the questions to
//! the source's agent, the wait for its session. The creation itself cannot
//! be cut short: a Codex fork holds the parent's session lock while `codex
//! exec fork` runs, and dropping it mid-way would release that lock while the
//! process is still being killed. So the call waits for its task: on a stop,
//! up to [`STOP_ANSWER_ALLOWANCE`], within the server's grace for in-flight
//! calls, which is enough for every phase but a creation under way to settle
//! and answer with the receipt that names the fork; and otherwise to the
//! call's finishing deadline, which a creation begun late can outrun. When
//! the task is still at work at either point the call answers
//! `request_in_progress` with the send id: the task keeps its lock, finishes,
//! and settles the entry, and the caller reads the outcome with `wait_send`,
//! or resumes it with the same key.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use switchboard_core::{AgentId, AgentRecord, ProjectId, SendId, render_prompt_with_attachments};
use switchboard_harness::check_prompt_size;

use crate::commands::{fork_agent_impl, list_project_agents_readonly_impl, open_project_impl};
use crate::error::AppError;
use crate::state::{AppState, lock};

use super::attach;
use super::desktop_sync::SyncFork;
use super::error_code::error_code;
use super::handle::AppStateHandle;
use super::ledger::{ForkClaim, SendLedger, SendReceipt};
use super::send::{
    Body, OnBusyPolicy, SettleGuard, await_joined, dispatch_recipients, refuse_if_busy,
    render_body, request_hash, settle_cancelled,
};
use super::wait::{Outcome, WaitBudget};

/// How long the call keeps waiting for its task after a server stop before
/// answering `request_in_progress`. Kept under the server's grace for calls
/// in flight (`CALLS_GRACE`, 2 seconds; a server test holds the two apart),
/// so the answer still reaches the client.
pub const STOP_ANSWER_ALLOWANCE: std::time::Duration = std::time::Duration::from_secs(1);

pub struct ForkRequest {
    pub project_id: ProjectId,
    /// The source: the agent whose conversation is branched.
    pub agent_id: AgentId,
    pub body: Body,
    pub idempotency_key: Option<String>,
    /// Files on this Mac to attach to the fork's first message, as absolute
    /// paths, handled as a send's are.
    pub attachments: Vec<PathBuf>,
}

/// What a fork request returns: the fork, and the receipt of the message
/// sent to it. `fork` is absent only for a request cancelled before the fork
/// was created, which is final under its key as a cancelled send is.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ForkReceipt {
    pub fork: Option<AgentRecord>,
    #[serde(flatten)]
    pub send: SendReceipt,
}

/// The request as the retry key sees it.
#[derive(Serialize)]
struct Fingerprint<'a> {
    project_id: ProjectId,
    agent_id: AgentId,
    body: &'a Body,
    #[serde(skip_serializing_if = "<[PathBuf]>::is_empty")]
    attachments: &'a [PathBuf],
}

/// Branch `agent_id`'s conversation into a new agent and send it the
/// message; see the module doc for the order and the retry rule.
pub async fn fork(
    handle: Arc<dyn AppStateHandle>,
    home_dir: PathBuf,
    request: ForkRequest,
    budget: WaitBudget,
) -> Result<ForkReceipt, AppError> {
    let ForkRequest {
        project_id,
        agent_id,
        body,
        idempotency_key: key,
        attachments,
    } = request;
    let hash = request_hash(&Fingerprint {
        project_id,
        agent_id,
        body: &body,
        attachments: &attachments,
    });
    let ledger: &SendLedger = &handle.app_state().ledger;
    // A child of the call's cancel token: server shutdown fires it too, and
    // the ledger's cancel flag tells the two apart.
    let cancel = budget.cancel_token().child_token();
    let (send_id, existing) =
        match ledger.claim_fork(project_id, key.as_deref(), &hash, cancel.clone()) {
            ForkClaim::Fresh { send_id } => (send_id, None),
            ForkClaim::Resume { send_id, fork } => (send_id, Some(fork)),
            ForkClaim::Joined { send_id, result } => {
                let send = await_joined(&budget, result, send_id).await?;
                let fork = key
                    .as_deref()
                    .and_then(|key| ledger.fork_for_key(project_id, key));
                return Ok(ForkReceipt { fork, send });
            }
            ForkClaim::Mismatch => {
                return Err(AppError::InvalidArgument(
                    "idempotency_key was already used for a different request in this project"
                        .to_owned(),
                ));
            }
        };
    let plan = Plan {
        send_id,
        project_id,
        source: agent_id,
        body,
        attachments,
        existing,
    };
    // Own task, as a send: the caller's future may be dropped at any point
    // from here on without affecting the fork or its message. Its budget
    // stops on either the call's cancellation (server shutdown) or the
    // send's own (`cancel_send`).
    let prep = budget.clone().with_cancel(cancel);
    let mut task = tokio::spawn(run(handle, home_dir, plan, prep));
    let joined = match budget.run_with_grace(&mut task).await {
        Outcome::Done(joined) => joined,
        Outcome::Timeout => return Err(AppError::RequestInProgress { send_id }),
        // See the module doc: every phase but a creation under way settles
        // within the allowance and answers for itself.
        Outcome::Cancelled => match tokio::time::timeout(STOP_ANSWER_ALLOWANCE, &mut task).await {
            Ok(joined) => joined,
            Err(_) => return Err(AppError::RequestInProgress { send_id }),
        },
    };
    joined.unwrap_or_else(|join_error| {
        Err(AppError::Internal(format!(
            "the fork task did not finish: {join_error}"
        )))
    })
}

struct Plan {
    send_id: SendId,
    project_id: ProjectId,
    source: AgentId,
    body: Body,
    attachments: Vec<PathBuf>,
    /// The fork a resumed request sends to; `None` creates one.
    existing: Option<AgentRecord>,
}

async fn run(
    handle: Arc<dyn AppStateHandle>,
    home_dir: PathBuf,
    plan: Plan,
    prep: WaitBudget,
) -> Result<ForkReceipt, AppError> {
    let state = handle.app_state();
    let (send_id, project_id) = (plan.send_id, plan.project_id);
    let mut guard = SettleGuard {
        state,
        send_id,
        armed: true,
    };
    let mut fork = plan.existing.clone();
    let result = Box::pin(run_inner(state, &home_dir, plan, &prep, &mut fork)).await;
    guard.armed = false;
    match result {
        Ok(send) => {
            tracing::info!(%project_id, %send_id, fork = ?fork.as_ref().map(|f| f.id), "fork dispatched");
            Ok(ForkReceipt { fork, send })
        }
        // Cancelled while preparing: final under its key, as a send is,
        // whether or not the fork was created first.
        Err(_) if state.ledger.cancel_requested(send_id) => {
            settle_cancelled(state, send_id).map(|send| ForkReceipt { fork, send })
        }
        // The fork exists, so the key stays: the refusal goes on the
        // recipient, the entry settles, and the receipt reports both, so a
        // repeat under the key sends again to this fork.
        Err(error) if fork.is_some() => {
            tracing::info!(%project_id, %send_id, code = error_code(&error), "fork created, its message refused");
            state
                .ledger
                .reject_undispatched(send_id, error_code(&error), &error.to_string());
            let send = state.ledger.settle(send_id).ok_or_else(|| {
                AppError::Internal("the fork's ledger entry vanished after its refusal".to_owned())
            })?;
            Ok(ForkReceipt { fork, send })
        }
        Err(error) => {
            tracing::info!(%project_id, %send_id, code = error_code(&error), "fork refused");
            let error = Arc::new(error);
            state.ledger.release(send_id, &error);
            Err(AppError::Shared(error))
        }
    }
}

/// Wait for the source's session to be released after its reply. `wait_agent`
/// reports idle at the reply's terminal, when the dispatcher would accept a
/// send, but the actor holds the source's session through its post-reply
/// work (Codex reads its rollout then), and a fork taken under that hold is
/// refused as busy though the caller did as told. So the fork waits for the
/// release, under the call's budget, woken by the agent's events. A source
/// with a turn running is refused at once: that wait is the caller's, with
/// `wait_agent`. The questions to the agent run under the call's finishing
/// allowance, as the send's busy check does: an agent preparing a turn
/// answers none for seconds, and a stop must not leave the call hanging.
async fn await_source_released(
    state: &AppState,
    source: AgentId,
    source_name: &str,
    budget: &WaitBudget,
) -> Result<(), AppError> {
    // Subscribed before the checks, so a release between a check and the
    // park is seen.
    let mut signal = state.ledger.subscribe_agent(source);
    loop {
        let running = match budget
            .run_with_grace(state.dispatcher.running_turn_kind(source))
            .await
        {
            Outcome::Done(kind) => kind.is_some(),
            Outcome::Timeout => {
                return Err(AppError::TimedOut {
                    what: "asking whether the source is running a turn",
                });
            }
            Outcome::Cancelled => return Err(AppError::Interrupted),
        };
        if running {
            return Err(AppError::ForkSourceBusy {
                name: source_name.to_owned(),
            });
        }
        let pending = match budget
            .run_with_grace(state.dispatcher.has_pending_work(source))
            .await
        {
            Outcome::Done(pending) => pending,
            Outcome::Timeout => {
                return Err(AppError::TimedOut {
                    what: "asking whether the source still holds its session",
                });
            }
            Outcome::Cancelled => return Err(AppError::Interrupted),
        };
        if !pending {
            return Ok(());
        }
        match budget.run(signal.changed()).await {
            Outcome::Done(Ok(())) => {}
            Outcome::Done(Err(_)) => {
                return Err(AppError::Internal(
                    "the agent's event signal closed while waiting for its session".to_owned(),
                ));
            }
            Outcome::Timeout => {
                return Err(AppError::ForkSourceBusy {
                    name: source_name.to_owned(),
                });
            }
            Outcome::Cancelled => return Err(AppError::Interrupted),
        }
    }
}

async fn run_inner(
    state: &AppState,
    home_dir: &Path,
    plan: Plan,
    budget: &WaitBudget,
    fork: &mut Option<AgentRecord>,
) -> Result<SendReceipt, AppError> {
    let Plan {
        send_id,
        project_id,
        source,
        body,
        attachments,
        ..
    } = plan;
    // Every attempt, before the fork is created: a file that cannot be read
    // refuses the request with no fork made.
    match budget.run(attach::check_sources(state, &attachments)).await {
        Outcome::Done(checked) => checked?,
        Outcome::Timeout => {
            return Err(AppError::TimedOut {
                what: "checking the attachments",
            });
        }
        Outcome::Cancelled => return Err(AppError::Interrupted),
    }
    open_project_impl(state, project_id)?;
    let project = lock(&state.projects)
        .get(&project_id)
        .cloned()
        .ok_or(AppError::ProjectNotLoaded(project_id))?;
    if !project.directory.is_dir() {
        return Err(AppError::DirectoryMissing {
            project_id,
            path: project.directory.clone(),
        });
    }
    // Membership first, as a send checks it; then the message, so a prompt
    // that cannot render refuses before anything is created.
    if fork.is_none()
        && !list_project_agents_readonly_impl(state, project_id)?
            .iter()
            .any(|a| a.id == source)
    {
        return Err(AppError::UnknownRecipient {
            project_id,
            agent_id: source,
        });
    }
    let text = render_body(state, home_dir, project_id, body.into(), budget).await?;
    if fork.is_none() {
        let source_record = list_project_agents_readonly_impl(state, project_id)?
            .into_iter()
            .find(|a| a.id == source);
        let source_name = source_record
            .as_ref()
            .map_or_else(|| source.to_string(), |a| a.name.clone());
        // A Codex fork runs `codex` on the login-shell PATH, whose capture at
        // launch can take seconds: waited for here, under the budget, so a
        // stop ends it; the creation then finds it settled.
        if source_record.is_some_and(|a| a.harness.supports_fork_without_turn()) {
            match budget.run(crate::commands::fork_path_ready()).await {
                Outcome::Done(_) => {}
                Outcome::Timeout => {
                    return Err(AppError::TimedOut {
                        what: "waiting for the login-shell PATH",
                    });
                }
                Outcome::Cancelled => return Err(AppError::Interrupted),
            }
        }
        await_source_released(state, source, &source_name, budget).await?;
        let created = fork_agent_impl(state, source, home_dir, &state.codex_binary).await?;
        state.ledger.attach_fork(send_id, &created);
        *fork = Some(created);
    }
    // The roster after creation: the fork's current record (a rename between
    // attempts keeps its name), and a fork removed between attempts is not
    // restored.
    let roster = list_project_agents_readonly_impl(state, project_id)?;
    let Some(fork_id) = fork.as_ref().map(|f| f.id) else {
        return Err(AppError::Internal(
            "the fork was not recorded before its message".to_owned(),
        ));
    };
    let Some(current) = roster.iter().find(|a| a.id == fork_id) else {
        return Err(AppError::UnknownRecipient {
            project_id,
            agent_id: fork_id,
        });
    };
    *fork = Some(current.clone());
    let carried = SyncFork {
        fork_id,
        source_agent_id: source,
    };
    let report = match budget
        .run_with_grace(state.desktop_sync.ensure_ready(project_id, Some(carried)))
        .await
    {
        Outcome::Done(report) => report?,
        Outcome::Timeout => return Err(AppError::ProjectLoadTimeout(project_id)),
        Outcome::Cancelled => return Err(AppError::Interrupted),
    };
    let unheard = report.hearing.unheard([fork_id]);
    refuse_if_busy(state, budget, &[fork_id], &unheard, &roster).await?;
    // After the window has the project open, as a send copies: see `attach`.
    // A copy that fails leaves the fork without its message, which the retry
    // rule above covers: the same key sends to this fork again.
    let attachments = attach::stage(state, project_id, &attachments, budget.cancel_token()).await?;
    if !attachments.is_empty() {
        check_prompt_size(&render_prompt_with_attachments(&text, &attachments))
            .map_err(AppError::PromptTooLarge)?;
    }
    dispatch_recipients(
        state,
        home_dir,
        send_id,
        &[fork_id],
        OnBusyPolicy::Refuse,
        &unheard,
        &roster,
        &text,
        &attachments,
    )
    .await
}
