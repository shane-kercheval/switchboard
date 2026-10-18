//! The turn tier for messages: `send`, `forward`, the waits, and the cancels.
//!
//! **Why a send runs as its own task.** Everything after the claim runs in a
//! spawned task that the caller awaits. Cancelling the tool call, or shutting
//! the server down, never interrupts a send part-way: every recipient reaches
//! a real outcome, and a retry with the same key gets the receipt.
//!
//! **Why `refuse` stops at the first refusal.** Under `refuse` every
//! recipient is asked up front whether it would accept a send, and a busy one
//! refuses the whole send with nothing dispatched. Dispatch then runs in
//! order, fail-fast. A recipient refused anyway — it became busy between the
//! check and its own admission — is `rejected`, every later recipient is
//! `not_dispatched`, and earlier accepted turns keep running: a check-then-
//! dispatch loop cannot promise all-or-nothing, and this is the rule
//! workflows already follow.
//!
//! **Why the key is released whenever nothing was dispatched.** A request
//! that fails before any recipient's turn exists — a bad recipient, a missing
//! directory, a source still mid-turn, a prompt that needs sign-in, every
//! recipient refused — leaves nothing to protect a retry from. Holding the key
//! would hand the natural retry (wait for the source, then send again) the
//! stored failure forever. The release happens inside the task, through a
//! guard that also runs if the task panics: the caller may have disconnected,
//! so nobody else would see the panic, and a panic after a recipient was
//! accepted must **keep** the key, or a retry would send to that recipient
//! again.
//!
//! **Cancelling a send that is still preparing.** A retry that runs out of
//! budget hands its caller the send id before the send has dispatched, so
//! `cancel_send` can arrive while a forward waits on its source, or in the
//! gap between the loop's cancel check and the dispatcher's answer. Each send
//! carries a cancel flag and signal in the ledger: the signal stops
//! preparation at once, the loop checks the flag before each dispatch, and
//! again after each admission (cancelling that message through the
//! dispatcher), so no window lets a cancelled send go out. A cancelled send
//! settles with its undispatched recipients `cancelled` and **keeps** its
//! key: a retry gets the cancelled receipt rather than sending what the
//! caller stopped.
//!
//! **What the budget bounds.** Everything before the first dispatch runs
//! under the call's wait budget: prompt rendering and a forward's compose
//! under its deadline, then the window sync and the busy check, which run
//! last so the window is known ready when the message goes out, under its
//! finishing allowance. At the end of either, nothing has gone out, so the
//! call fails and the key is released. The dispatch loop itself is never cut
//! short. A retry that joins an in-flight send waits under its own budget
//! and, if that ends first, reports `request_in_progress` with the send id,
//! leaving the key claimed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use switchboard_core::{
    AgentId, AgentRecord, Attachment, Project, ProjectId, SendId, render_prompt_with_attachments,
};
use switchboard_dispatcher::{OnBusy, SendOptions};
use switchboard_harness::{TurnStatus, check_prompt_size};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::commands::{
    AgentJournalView, ForwardArgInputs, ForwardInput, ForwardOutcome, ForwardSourceRef,
    OutcomeStatus, SendCorrelation, cancel_agent_impl, cancel_send_impl, combine_prompt_message,
    dispatch_message_impl, forward_message_inputs, forward_prompt_inputs,
    list_project_agents_readonly_impl, open_project_impl,
};
use crate::error::AppError;
use crate::state::{AppState, lock};

use super::attach;
use super::budget::{BudgetSpec, CutSite};
use super::error_code::error_code;
use super::handle::AppStateHandle;
use super::ledger::{Claim, ClaimResult, RecipientSnapshot, SendReceipt, SendSnapshot, SendStatus};
use super::read::{
    LatestReply, agent_in_project, correlate_turns, project_readonly, read_journal,
    read_latest_reply,
};
use super::usage::stored_usage;
use super::wait::{Observed, Outcome, WaitBudget, wait_until};

/// What a send says.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    Text {
        text: String,
    },
    /// A saved prompt rendered with `args`, followed by `appended_text` when
    /// given, combined exactly as the compose bar combines them.
    Prompt {
        provider: String,
        name: String,
        #[serde(default)]
        args: BTreeMap<String, String>,
        #[serde(default)]
        appended_text: Option<String>,
    },
}

/// What to do with a recipient that is busy.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnBusyPolicy {
    /// Refuse the whole send when any recipient is busy up front; otherwise
    /// stop at the first refusal.
    #[default]
    Refuse,
    /// Queue behind the running turn.
    Queue,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendRequest {
    pub project_id: ProjectId,
    pub recipients: Vec<AgentId>,
    pub body: Body,
    #[serde(default)]
    pub on_busy: OnBusyPolicy,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Files on this Mac to attach, as absolute paths. Each is copied into the
    /// project and every recipient gets the copies' paths after the message,
    /// as with a file attached in the window.
    #[serde(default)]
    pub attachments: Vec<PathBuf>,
}

/// A saved prompt in a forward: rendered with `args`, with each argument in
/// `forward_args` composed from its sources (typed text first, then the
/// forwarded blocks), as the compose bar's prompt composer does.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForwardPrompt {
    pub provider: String,
    pub name: String,
    #[serde(default)]
    pub args: BTreeMap<String, String>,
    #[serde(default)]
    pub forward_args: BTreeMap<String, Vec<ForwardFrom>>,
}

/// One source of a forward: an agent, possibly in another project, and
/// optionally one of its earlier replies.
///
/// **Why a source can name a reply.** Without `send_id` the source's latest
/// answer is forwarded, waiting for a turn in flight. An orchestrator often
/// wants an answer that is no longer the latest: a reviewer's first round
/// after a workflow's later step, or a report the agent has since followed
/// with other work. With `send_id`, that reply is forwarded as it is. A
/// finished reply cannot change, so nothing waits on the agent, which may be
/// busy with something else. A reply that is not finished, or did not
/// complete, is refused rather than waited for or replaced by a note: the
/// caller named it, so anything else would not be what it asked for.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForwardFrom {
    pub project_id: ProjectId,
    pub agent_id: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_id: Option<SendId>,
}

impl From<ForwardSourceRef> for ForwardFrom {
    fn from(source: ForwardSourceRef) -> Self {
        Self {
            project_id: source.project_id,
            agent_id: source.agent_id,
            send_id: None,
        }
    }
}

impl ForwardFrom {
    fn source_ref(self) -> ForwardSourceRef {
        ForwardSourceRef {
            agent_id: self.agent_id,
            project_id: self.project_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForwardRequest {
    pub project_id: ProjectId,
    /// Sources appended after the text (or after the rendered prompt). A
    /// source may live in another project, and may name one of its replies.
    pub sources: Vec<ForwardFrom>,
    pub recipients: Vec<AgentId>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub prompt: Option<ForwardPrompt>,
    #[serde(default)]
    pub on_busy: OnBusyPolicy,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Files on this Mac to attach, as absolute paths. Each is copied into the
    /// project and every recipient gets the copies' paths after the message,
    /// as with a file attached in the window.
    #[serde(default)]
    pub attachments: Vec<PathBuf>,
}

/// Send one message to the recipients and return each one's dispatch-time
/// status. A repeat with the same `idempotency_key` returns the original
/// receipt; the same key with a different request is `invalid_argument`.
/// `budget` has no caller-facing timeout: the tool layer builds it from the
/// default and whether the caller can be kept alive.
pub async fn send(
    handle: Arc<dyn AppStateHandle>,
    home_dir: PathBuf,
    request: SendRequest,
    budget: WaitBudget,
) -> Result<SendReceipt, AppError> {
    let plan = Plan {
        project_id: request.project_id,
        recipients: request.recipients,
        on_busy: request.on_busy,
        body: request.body.into(),
        attachments: request.attachments,
    };
    begin(handle, home_dir, plan, request.idempotency_key, budget).await
}

/// Forward the sources' latest output to the recipients, with or without a
/// saved prompt, composed exactly as the compose bar composes it. The compose
/// waits for a mid-turn source within `budget`; past it the forward fails
/// with `source_busy` and nothing is dispatched.
pub async fn forward(
    handle: Arc<dyn AppStateHandle>,
    home_dir: PathBuf,
    request: ForwardRequest,
    budget: WaitBudget,
) -> Result<SendReceipt, AppError> {
    if request.sources.is_empty()
        && request
            .prompt
            .as_ref()
            .is_none_or(|p| p.forward_args.values().all(Vec::is_empty))
    {
        return Err(AppError::NoForwardSources);
    }
    let plan = Plan {
        project_id: request.project_id,
        recipients: request.recipients,
        on_busy: request.on_busy,
        body: BodyPlan::Forward {
            sources: request.sources,
            text: request.text.unwrap_or_default(),
            prompt: request.prompt,
        },
        attachments: request.attachments,
    };
    begin(handle, home_dir, plan, request.idempotency_key, budget).await
}

/// The request as the task runs it. Serialized to hash it for the retry key:
/// two requests are "the same" when every field here is.
#[derive(Serialize)]
struct Plan {
    project_id: ProjectId,
    recipients: Vec<AgentId>,
    on_busy: OnBusyPolicy,
    body: BodyPlan,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<PathBuf>,
}

#[derive(Serialize)]
pub(super) enum BodyPlan {
    Text(String),
    Prompt {
        provider: String,
        name: String,
        args: BTreeMap<String, String>,
        appended_text: String,
    },
    Forward {
        sources: Vec<ForwardFrom>,
        text: String,
        prompt: Option<ForwardPrompt>,
    },
}

impl From<Body> for BodyPlan {
    fn from(body: Body) -> Self {
        match body {
            Body::Text { text } => Self::Text(text),
            Body::Prompt {
                provider,
                name,
                args,
                appended_text,
            } => Self::Prompt {
                provider,
                name,
                args,
                appended_text: appended_text.unwrap_or_default(),
            },
        }
    }
}

/// The retry key's view of a request: two requests are "the same" when their
/// serialized forms are.
pub(super) fn request_hash<T: Serialize>(plan: &T) -> String {
    use std::fmt::Write as _;
    let bytes = serde_json::to_vec(plan).unwrap_or_default();
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

async fn begin(
    handle: Arc<dyn AppStateHandle>,
    home_dir: PathBuf,
    plan: Plan,
    key: Option<String>,
    budget: WaitBudget,
) -> Result<SendReceipt, AppError> {
    let project_id = plan.project_id;
    if plan.recipients.is_empty() {
        return Err(AppError::InvalidArgument(
            "a send needs at least one recipient".to_owned(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    if let Some(repeated) = plan.recipients.iter().find(|id| !seen.insert(**id)) {
        return Err(AppError::InvalidArgument(format!(
            "recipient {repeated} is listed more than once"
        )));
    }
    let hash = request_hash(&plan);
    let send_id = match handle
        .app_state()
        .ledger
        .claim(project_id, key.as_deref(), &hash)
    {
        Claim::Fresh { send_id } => send_id,
        Claim::Joined {
            send_id: original,
            result,
        } => return await_joined(&budget, result, original).await,
        Claim::Mismatch => {
            return Err(AppError::InvalidArgument(
                "idempotency_key was already used for a different request in this project"
                    .to_owned(),
            ));
        }
    };
    // A child of the call's cancel token: server shutdown fires it too, and
    // the ledger's cancel flag tells the two apart.
    let send_cancel = budget.cancel_token().child_token();
    handle.app_state().ledger.register_send(
        send_id,
        project_id,
        &plan.recipients,
        send_cancel.clone(),
    );
    // Own task: see the module doc. The caller's future may be dropped at
    // any point from here on without affecting the send.
    let task = tokio::spawn(run(handle, home_dir, send_id, plan, budget, send_cancel));
    task.await.unwrap_or_else(|join_error| {
        Err(AppError::Internal(format!(
            "the send task did not finish: {join_error}"
        )))
    })
}

/// What a request that joined another's claim receives: the original's
/// receipt or error once it settles, or `request_in_progress` if the budget
/// runs out first (the original keeps running and keeps its key: the caller
/// calls again with the same key).
pub(super) async fn await_joined(
    budget: &WaitBudget,
    mut result: watch::Receiver<Option<ClaimResult>>,
    original: SendId,
) -> Result<SendReceipt, AppError> {
    match budget.run(result.wait_for(Option::is_some)).await {
        Outcome::Done(Ok(settled)) => match settled.clone() {
            Some(Ok(receipt)) => Ok(receipt),
            Some(Err(original)) => Err(AppError::Shared(original)),
            None => Err(AppError::Internal(
                "the original request settled without a result".to_owned(),
            )),
        },
        Outcome::Done(Err(_)) => Err(AppError::Internal(
            "the original request's record was dropped before it settled".to_owned(),
        )),
        Outcome::Timeout | Outcome::Cancelled => {
            Err(AppError::RequestInProgress { send_id: original })
        }
    }
}

/// Settles the send's ledger entry however the task ends. Lives in the task,
/// not the caller, because the caller may have disconnected; disarmed by a
/// normal finish. On an abnormal end (a panic): nothing accepted → release
/// the key; something accepted → settle what there is, keeping the key so a
/// retry cannot send the accepted recipient again.
///
/// **Only debug builds and tests reach the panic branch.** Release builds set
/// `panic = "abort"` (the root `Cargo.toml`), so a panic ends the whole
/// process: after the restart the ledger is empty and `wait_send` answers
/// from the journal. That is also why a recipient marked accepted just before
/// a panic, but never admitted, is not reconciled here — the case cannot
/// happen in the shipped app.
pub(super) struct SettleGuard<'a> {
    pub(super) state: &'a AppState,
    pub(super) send_id: SendId,
    pub(super) armed: bool,
}

impl Drop for SettleGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            settle_abnormally(self.state, self.send_id);
        }
    }
}

/// What the guard does for a send whose task ended abnormally. Separate so
/// the two outcomes are testable without provoking a panic inside a task. A
/// send that created a fork is settled, never released: its key must keep
/// naming that fork.
pub(crate) fn settle_abnormally(state: &AppState, send_id: SendId) {
    if state.ledger.dispatched_any(send_id) || state.ledger.fork_attached(send_id) {
        state.ledger.settle(send_id);
    } else {
        state.ledger.release(
            send_id,
            &Arc::new(AppError::Internal(
                "the send task ended before dispatching".to_owned(),
            )),
        );
    }
}

async fn run(
    handle: Arc<dyn AppStateHandle>,
    home_dir: PathBuf,
    send_id: SendId,
    plan: Plan,
    budget: WaitBudget,
    send_cancel: CancellationToken,
) -> Result<SendReceipt, AppError> {
    let state = handle.app_state();
    let project_id = plan.project_id;
    let mut guard = SettleGuard {
        state,
        send_id,
        armed: true,
    };
    // Preparation stops on either the call's cancellation (server shutdown)
    // or the send's own (`cancel_send`).
    let prep = budget.with_cancel(send_cancel);
    let result = Box::pin(run_inner(state, &home_dir, send_id, plan, &prep)).await;
    guard.armed = false;
    match result {
        Ok(receipt) => {
            tracing::info!(
                %project_id,
                %send_id,
                recipients = receipt.recipients.len(),
                accepted = receipt
                    .recipients
                    .iter()
                    .filter(|r| r.status == SendStatus::Accepted)
                    .count(),
                "send dispatched"
            );
            Ok(receipt)
        }
        // A send the caller cancelled while it was preparing settles with its
        // recipients cancelled and keeps its key; whatever the preparation
        // was doing when it stopped is not the outcome.
        Err(_) if state.ledger.cancel_requested(send_id) => settle_cancelled(state, send_id),
        // Every other error path ends with nothing dispatched (a refusal after
        // a dispatch is recorded per recipient, never returned), so the key is
        // safe to release here.
        Err(error) => {
            tracing::info!(%project_id, %send_id, code = error_code(&error), "send refused");
            let error = Arc::new(error);
            state.ledger.release(send_id, &error);
            Err(AppError::Shared(error))
        }
    }
}

/// Settle a send `cancel_send` stopped: every recipient not yet offered is
/// `cancelled`, admitted ones finish through the dispatcher's cancel, and the
/// key is kept so a retry returns this receipt.
pub(super) fn settle_cancelled(state: &AppState, send_id: SendId) -> Result<SendReceipt, AppError> {
    tracing::info!(%send_id, "send cancelled before it finished dispatching");
    state.ledger.cancel_undispatched(send_id);
    state.ledger.settle(send_id).ok_or_else(|| {
        AppError::Internal("the send's ledger entry vanished while cancelling".to_owned())
    })
}

async fn run_inner(
    state: &AppState,
    home_dir: &Path,
    send_id: SendId,
    plan: Plan,
    budget: &WaitBudget,
) -> Result<SendReceipt, AppError> {
    let project_id = plan.project_id;
    attach::check_within(state, budget, &plan.attachments).await?;
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
    let roster = list_project_agents_readonly_impl(state, project_id)?;
    for &agent_id in &plan.recipients {
        if !roster.iter().any(|a| a.id == agent_id) {
            return Err(AppError::UnknownRecipient {
                project_id,
                agent_id,
            });
        }
    }

    let body = render_body(state, home_dir, project_id, plan.body, budget).await?;

    // Last, right before dispatch: what has to hold is that the window was
    // ready when the message went out, and a forward can wait a long time for
    // its source above. Both checks are quick, so they share the call's
    // finishing allowance; a source that finishes at the deadline must not
    // fail an otherwise ready send.
    let report = match budget
        .run_with_grace(state.desktop_sync.ensure_ready(project_id, None))
        .await
    {
        Outcome::Done(report) => report?,
        Outcome::Timeout => return Err(AppError::ProjectLoadTimeout(project_id)),
        Outcome::Cancelled => return Err(AppError::Interrupted),
    };
    let unheard = report.hearing.unheard(plan.recipients.iter().copied());
    if plan.on_busy == OnBusyPolicy::Refuse {
        refuse_if_busy(state, budget, &plan.recipients, &unheard, &roster).await?;
    }
    // After the window has the project open, never before: see `attach`.
    let attachments =
        attach::stage(state, project_id, &plan.attachments, budget.cancel_token()).await?;
    if !attachments.is_empty() {
        check_prompt_size(&render_prompt_with_attachments(&body, &attachments))
            .map_err(AppError::PromptTooLarge)?;
    }
    dispatch_recipients(
        state,
        home_dir,
        send_id,
        &plan.recipients,
        plan.on_busy,
        &unheard,
        &roster,
        &body,
        &attachments,
    )
    .await
}

/// The message as it goes out: text as given, a saved prompt rendered under
/// the budget with its appended text, or a forward composed through the
/// existing resolvers. Checked against the prompt size on the final text, so
/// the whole send is refused before any recipient is dispatched. A send with
/// attachments is checked again with their paths once they are copied in; the
/// dispatcher's own check gives the same answer and stays as the backstop.
pub(super) async fn render_body(
    state: &AppState,
    home_dir: &Path,
    project_id: ProjectId,
    body: BodyPlan,
    budget: &WaitBudget,
) -> Result<String, AppError> {
    let body = match body {
        BodyPlan::Text(text) => text,
        BodyPlan::Prompt {
            provider,
            name,
            args,
            appended_text,
        } => match budget
            .run(state.prompts.render(&provider, &name, &args))
            .await
        {
            Outcome::Done(rendered) => combine_prompt_message(&rendered?.text, &appended_text),
            Outcome::Timeout => {
                return Err(AppError::TimedOut {
                    what: "rendering the saved prompt",
                });
            }
            Outcome::Cancelled => return Err(AppError::Interrupted),
        },
        BodyPlan::Forward {
            sources,
            text,
            prompt,
        } => {
            Box::pin(compose_forward(
                state, home_dir, project_id, sources, text, prompt, budget,
            ))
            .await?
        }
    };
    check_prompt_size(&body).map_err(AppError::PromptTooLarge)?;
    Ok(body)
}

fn agent_name(roster: &[AgentRecord], agent_id: AgentId) -> String {
    roster
        .iter()
        .find(|a| a.id == agent_id)
        .map_or_else(|| agent_id.to_string(), |a| a.name.clone())
}

/// The up-front check under `refuse`: every recipient the window can hear is
/// asked whether it would accept a send now, under the call's finishing
/// allowance (it runs after the window sync, which shares it). Only an agent
/// that answers "no" is busy. A deadline that passes first proves nothing
/// about the rest — it can pass before an idle agent is even asked — so the
/// send then fails with a timeout naming no one, unless an agent already
/// answered "no".
pub(super) async fn refuse_if_busy(
    state: &AppState,
    budget: &WaitBudget,
    recipients: &[AgentId],
    unsubscribed: &[AgentId],
    roster: &[AgentRecord],
) -> Result<(), AppError> {
    let mut busy = Vec::new();
    let candidates = recipients
        .iter()
        .copied()
        .filter(|a| !unsubscribed.contains(a));
    for agent_id in candidates {
        match budget
            .run_with_grace(state.dispatcher.accepts_now(agent_id))
            .await
        {
            Outcome::Done(true) => {}
            Outcome::Done(false) => busy.push(agent_name(roster, agent_id)),
            Outcome::Timeout if busy.is_empty() => {
                return Err(AppError::TimedOut {
                    what: "checking whether the recipients would accept a send",
                });
            }
            Outcome::Timeout => break,
            Outcome::Cancelled => return Err(AppError::Interrupted),
        }
    }
    if busy.is_empty() {
        Ok(())
    } else {
        Err(AppError::RecipientsBusy { names: busy })
    }
}

/// Dispatch in order. Under `refuse`, the first refusal **by the dispatcher**
/// stops the loop and leaves the rest `not_dispatched`; under `queue`, every
/// recipient is offered. A recipient the window cannot hear is rejected on its
/// own and never stops the loop, so whether the others are sent does not
/// depend on where it is listed. A send that dispatches nothing returns the
/// first refused recipient's own reason — a recipient the window cannot hear
/// must not read as "busy", or a caller would wait for an idle agent and retry
/// forever.
#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_recipients(
    state: &AppState,
    home_dir: &Path,
    send_id: SendId,
    recipients: &[AgentId],
    policy: OnBusyPolicy,
    unsubscribed: &[AgentId],
    roster: &[AgentRecord],
    body: &str,
    attachments: &[Attachment],
) -> Result<SendReceipt, AppError> {
    let mut dispatched_any = false;
    let mut stop = false;
    let mut refusals: Vec<(AgentId, AppError)> = Vec::new();
    for &agent_id in recipients {
        if stop {
            // Left `not_dispatched` by registration.
            break;
        }
        if state.ledger.cancel_requested(send_id) {
            break;
        }
        if state.ledger.recipient_cancel_requested(send_id, agent_id) {
            state
                .ledger
                .cancel_recipient_undispatched(send_id, agent_id);
            continue;
        }
        if unsubscribed.contains(&agent_id) {
            let error = AppError::DesktopUnavailable { agent_id };
            state
                .ledger
                .mark_rejected(send_id, agent_id, error_code(&error), &error.to_string());
            refusals.push((agent_id, error));
            continue;
        }
        let Some(message_id) = state.ledger.mark_accepted(send_id, agent_id) else {
            return Err(AppError::Internal(
                "the send's ledger entry vanished during dispatch".to_owned(),
            ));
        };
        let on_busy = match policy {
            OnBusyPolicy::Refuse => OnBusy::FailFast,
            OnBusyPolicy::Queue => OnBusy::Enqueue,
        };
        crate::commands::wait_at_ops_dispatch_seam(state).await;
        let outcome = dispatch_message_impl(
            state,
            agent_id,
            body,
            attachments.to_vec(),
            send_id,
            home_dir,
            on_busy,
            // Announced at admission so the window shows the message queued,
            // with the cancel button it has for its own, before the turn
            // starts; the user message follows once the send is journaled.
            SendOptions {
                message_id: Some(message_id),
                emit_user_message: true,
                announce_accepted: true,
            },
        )
        .await;
        match outcome {
            Ok(_) => {
                dispatched_any = true;
                // A cancel that arrived between the checks above and this
                // admission, for the whole send or this recipient alone, found
                // nothing in the dispatcher to cancel: cancel the message now
                // that it is there.
                if state.ledger.cancel_requested(send_id)
                    || state.ledger.recipient_cancel_requested(send_id, agent_id)
                {
                    cancel_send_impl(state, send_id, &[agent_id]);
                }
            }
            Err(error) => {
                state.ledger.mark_rejected(
                    send_id,
                    agent_id,
                    error_code(&error),
                    &error.to_string(),
                );
                refusals.push((agent_id, error));
                stop = policy == OnBusyPolicy::Refuse;
            }
        }
    }
    if state.ledger.cancel_requested(send_id) {
        return settle_cancelled(state, send_id);
    }
    if !dispatched_any && refusals.is_empty() {
        // Every recipient was cancelled one by one before it was offered the
        // message: a cancelled send, not a failed one.
        return state.ledger.settle(send_id).ok_or_else(|| {
            AppError::Internal("the send's ledger entry vanished while cancelling".to_owned())
        });
    }
    if !dispatched_any {
        // `run` releases the key.
        return Err(first_refusal(refusals, roster));
    }
    state.ledger.settle(send_id).ok_or_else(|| {
        AppError::Internal("the send's ledger entry vanished after dispatch".to_owned())
    })
}

/// The error a send that dispatched nothing returns: the first refusal's own
/// reason, with a busy refusal named the way the up-front check names it.
fn first_refusal(refusals: Vec<(AgentId, AppError)>, roster: &[AgentRecord]) -> AppError {
    let busy: Vec<String> = refusals
        .iter()
        .filter(|(_, e)| matches!(e, AppError::AgentBusy))
        .map(|(id, _)| agent_name(roster, *id))
        .collect();
    match refusals.into_iter().next() {
        Some((_, AppError::AgentBusy)) => AppError::RecipientsBusy { names: busy },
        Some((_, error)) => error,
        None => AppError::Internal("a send dispatched nothing and recorded no refusal".to_owned()),
    }
}

/// Compose a forward's body through the existing resolvers (they open the
/// sources' projects and wait for a mid-turn source), under the call's budget:
/// the kept-alive caller gets progress while a source finishes, and at the
/// deadline the resolver's future is dropped, which releases its hold (its
/// guard removes the cancel token), so nothing is left registered. A source
/// that names a reply is read first ([`named_reply`]) and handed to the
/// resolver as settled text, so it never waits.
async fn compose_forward(
    state: &AppState,
    home_dir: &Path,
    project_id: ProjectId,
    sources: Vec<ForwardFrom>,
    text: String,
    prompt: Option<ForwardPrompt>,
    budget: &WaitBudget,
) -> Result<String, AppError> {
    let forward_id = Uuid::now_v7();
    // Only the sources forwarded by latest answer can be waited on.
    let waited_on: Vec<ForwardSourceRef> = sources
        .iter()
        .chain(
            prompt
                .iter()
                .flat_map(|p| p.forward_args.values().flatten()),
        )
        .filter(|source| source.send_id.is_none())
        .map(|source| source.source_ref())
        .collect();
    let compose = async {
        let appended = forward_inputs(state, &sources, home_dir).await?;
        match prompt {
            None => {
                forward_message_inputs(state, text, appended, forward_id, home_dir, project_id)
                    .await
            }
            Some(prompt) => {
                let mut forward_args = Vec::new();
                for (name, sources, required) in forward_args_for(state, &prompt)? {
                    forward_args.push(ForwardArgInputs {
                        name,
                        sources: forward_inputs(state, &sources, home_dir).await?,
                        required,
                    });
                }
                forward_prompt_inputs(
                    state,
                    prompt.provider,
                    prompt.name,
                    prompt.args,
                    forward_args,
                    text,
                    appended,
                    forward_id,
                    home_dir,
                    project_id,
                )
                .await
            }
        }
    };
    let outcome = match budget.run(Box::pin(compose)).await {
        Outcome::Done(outcome) => outcome?,
        Outcome::Timeout => return Err(compose_timeout(state, budget, &waited_on).await),
        Outcome::Cancelled => return Err(AppError::Interrupted),
    };
    match outcome {
        ForwardOutcome::Resolved { body } => Ok(body),
        ForwardOutcome::Invalidated { reason } => Err(AppError::InvalidArgument(reason)),
        // Only this function holds the forward id, so nothing else cancels
        // the hold; reaching this means it was torn down from outside.
        ForwardOutcome::Cancelled => Err(AppError::Interrupted),
    }
}

/// The resolver's inputs for `sources`, in order: a source that names a reply
/// carries that reply's text, read now; the others are forwarded by latest
/// answer.
async fn forward_inputs(
    state: &AppState,
    sources: &[ForwardFrom],
    home_dir: &Path,
) -> Result<Vec<ForwardInput>, AppError> {
    let mut inputs = Vec::with_capacity(sources.len());
    for source in sources {
        let settled_text = match source.send_id {
            Some(send_id) => Some(named_reply(state, *source, send_id, home_dir).await?),
            None => None,
        };
        inputs.push(ForwardInput {
            source: source.source_ref(),
            settled_text,
        });
    }
    Ok(inputs)
}

/// The text of `source`'s reply to `send_id`, read as `wait_send` reads the
/// send ([`current_send`]): the ledger's captured reply while it holds the
/// send, otherwise the journal's answer, where a reply counts as completed
/// only through a durable link to its send, so a guess never stands in for
/// the reply the caller named. Only a completed reply is returned; one still
/// to be sent, waiting, or running, one that did not complete, and one that
/// cannot be tied to the send refuse the forward.
async fn named_reply(
    state: &AppState,
    source: ForwardFrom,
    send_id: SendId,
    home_dir: &Path,
) -> Result<String, AppError> {
    let agent = agent_in_project(state, source.project_id, source.agent_id)?;
    let current = current_send(state, source.project_id, send_id, home_dir).await?;
    let Some(recipient) = current
        .reply
        .recipients
        .into_iter()
        .find(|r| r.agent_id == source.agent_id)
    else {
        return Err(AppError::ForwardReplyUnknown {
            name: agent.name,
            send_id,
        });
    };
    let not_completed = |state: &'static str| AppError::ForwardReplyNotCompleted {
        name: agent.name.clone(),
        send_id,
        state,
    };
    match recipient.status {
        SendStatus::Completed => Ok(recipient.text.unwrap_or_default()),
        SendStatus::Accepted => Err(AppError::ForwardReplyUnfinished {
            name: agent.name,
            send_id,
            state: "waiting in line",
        }),
        SendStatus::Running => Err(AppError::ForwardReplyUnfinished {
            name: agent.name,
            send_id,
            state: "running",
        }),
        SendStatus::NotDispatched if !current.handed_out => Err(AppError::ForwardReplyUnfinished {
            name: agent.name,
            send_id,
            state: "waiting to be sent",
        }),
        SendStatus::Failed => Err(not_completed("failed")),
        SendStatus::Cancelled => Err(not_completed("was cancelled")),
        SendStatus::Rejected | SendStatus::NotDispatched => {
            Err(not_completed("never reached the agent"))
        }
        // The journal could not tie a finished reply to this send: it may
        // still be running (a message the window sent, which Switchboard
        // follows only on disk), the app may have restarted while it ran, or
        // its harness keeps no link from a reply to its send and Switchboard
        // no longer holds the send. A reply matched by position is a guess,
        // and a forward cannot mark it as one, so it is refused.
        SendStatus::Started | SendStatus::Unknown => Err(AppError::ForwardReplyUnconfirmed {
            name: agent.name,
            send_id,
        }),
    }
}

/// The error for a forward whose compose ran out of budget: `source_busy`
/// naming the sources still running, so the caller knows which agent to
/// `wait_agent` on; or, when none is running, a timeout — the time went to
/// opening source projects or rendering the prompt, and calling the sources
/// busy would send the caller to wait on idle agents. Asked within the call's
/// finishing allowance; a source that cannot answer in it is starting a turn,
/// so it counts as running.
async fn compose_timeout(
    state: &AppState,
    budget: &WaitBudget,
    sources: &[ForwardSourceRef],
) -> AppError {
    let mut names = Vec::new();
    for source in sources {
        let running = match budget
            .run_with_grace(state.dispatcher.running_turn_kind(source.agent_id))
            .await
        {
            Outcome::Done(kind) => kind.is_some(),
            Outcome::Timeout | Outcome::Cancelled => true,
        };
        if running {
            let name = lock(&state.agents_by_id)
                .get(&source.agent_id)
                .map_or_else(|| source.agent_id.to_string(), |a| a.name.clone());
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    if names.is_empty() {
        AppError::TimedOut {
            what: "preparing the forward",
        }
    } else {
        AppError::SourceBusy {
            name: names.join(", "),
        }
    }
}

/// The forwarded arguments, each with its sources and `required` read from
/// the prompt's own schema, never from the caller.
fn forward_args_for(
    state: &AppState,
    prompt: &ForwardPrompt,
) -> Result<Vec<(String, Vec<ForwardFrom>, bool)>, AppError> {
    let schema = state
        .prompts
        .list()
        .into_iter()
        .find(|p| p.provider == prompt.provider && p.name == prompt.name)
        .ok_or_else(|| {
            AppError::Prompt(switchboard_prompts::PromptError::PromptNotFound {
                provider: prompt.provider.clone(),
                name: prompt.name.clone(),
            })
        })?;
    prompt
        .forward_args
        .iter()
        .map(|(name, sources)| {
            let argument = schema
                .arguments
                .iter()
                .find(|a| &a.name == name)
                .ok_or_else(|| {
                    AppError::InvalidArgument(format!(
                        "prompt {}:{} has no argument {name:?}",
                        prompt.provider, prompt.name
                    ))
                })?;
            Ok((name.clone(), sources.clone(), argument.required))
        })
        .collect()
}

// --- cancels -----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CancelledSend {
    pub send_id: SendId,
    pub outcome: CancelOutcome,
    /// The recipients the cancel reached: those still accepted or running, and,
    /// for a send still dispatching, those not yet sent.
    pub recipients: Vec<AgentId>,
}

/// What a cancel found.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CancelOutcome {
    /// At least one recipient was still queued, running, or unsent.
    Cancelled,
    /// Every recipient had already ended: nothing to cancel.
    AlreadyFinished,
    /// One recipient's cancel was sent to its agent without a record of the
    /// send to check against: a workflow's send the ledger has pruned since the
    /// app started (who started its run is kept until the app quits, so the
    /// cancel is still allowed; after a restart it is refused). The agent stops
    /// the reply if it still has it.
    Requested,
}

/// Cancel a send: every recipient still accepted or running is cancelled
/// through the dispatcher, and a send still preparing or dispatching is told to
/// stop, so its remaining recipients are never sent. Needs the ledger's record
/// of the send: a queued send does not survive a restart. A workflow's send is
/// held like the agent tools' own, so this reaches it too, and the cancelled
/// replies end its run as any cancelled participant does.
///
/// With `agent_id`, only that recipient's reply is cancelled, queued or
/// running: its actor drops the send's item, and the other recipients carry
/// on. That needs no ledger record, so it also reaches a workflow's send the
/// ledger no longer holds; the outcome is then `requested`, because nothing
/// here can see whether the agent still had the reply.
///
/// Either way, a send made by a workflow the user started from the window is
/// refused: the user's work is the user's to stop.
pub fn cancel_send(
    state: &AppState,
    project_id: ProjectId,
    send_id: SendId,
    agent_id: Option<AgentId>,
) -> Result<CancelledSend, AppError> {
    if workflow_send_owner(state, project_id, send_id) == Some(false) {
        return Err(AppError::ReplyInUserWorkflow { send_id });
    }
    if let Some(agent_id) = agent_id {
        return cancel_one_recipient(state, project_id, send_id, agent_id);
    }
    let snapshot = state
        .ledger
        .send(send_id)
        .filter(|s| s.project_id == project_id)
        .ok_or(AppError::SendUnknown(send_id))?;
    let in_dispatcher = state.ledger.request_cancel(send_id).unwrap_or_default();
    cancel_send_impl(state, send_id, &in_dispatcher);
    let recipients: Vec<AgentId> = snapshot
        .recipients
        .iter()
        .filter(|r| {
            !r.status.is_terminal() || (!snapshot.settled && r.status == SendStatus::NotDispatched)
        })
        .map(|r| r.agent_id)
        .collect();
    let outcome = if recipients.is_empty() && snapshot.is_done() {
        CancelOutcome::AlreadyFinished
    } else {
        CancelOutcome::Cancelled
    };
    tracing::info!(%project_id, %send_id, recipients = recipients.len(), ?outcome, "send cancelled");
    Ok(CancelledSend {
        send_id,
        outcome,
        recipients,
    })
}

/// [`cancel_send`] for one recipient. Only a send made through the agent
/// tools or by a workflow started through them (the ledger holds both, and
/// [`cancel_send`] has already refused a workflow the user started): a reply
/// to a message the user typed is the user's to stop.
fn cancel_one_recipient(
    state: &AppState,
    project_id: ProjectId,
    send_id: SendId,
    agent_id: AgentId,
) -> Result<CancelledSend, AppError> {
    agent_in_project(state, project_id, agent_id)?;
    let held = state
        .ledger
        .send(send_id)
        .filter(|s| s.project_id == project_id);
    let outcome = if let Some(snapshot) = held {
        let recipient = snapshot
            .recipients
            .iter()
            .find(|r| r.agent_id == agent_id)
            .ok_or(AppError::UnknownRecipient {
                project_id,
                agent_id,
            })?;
        let still_to_offer = recipient.status == SendStatus::NotDispatched && !snapshot.settled;
        if recipient.status.is_terminal() && !still_to_offer {
            CancelOutcome::AlreadyFinished
        } else {
            // Flagged before the dispatcher is told, so a recipient the send
            // has not offered yet is skipped, and one it is offering now is
            // cancelled once admitted.
            state.ledger.request_cancel_recipient(send_id, agent_id);
            if state.ledger.no_recipient_left(send_id) {
                // Nobody left to receive it: stop the whole send, as the
                // window's cancel on a waiting forward does, so a forward still
                // waiting on its source stops now and a retry under the same
                // key returns this cancellation rather than sending again.
                let in_dispatcher = state.ledger.request_cancel(send_id).unwrap_or_default();
                cancel_send_impl(state, send_id, &in_dispatcher);
            } else {
                cancel_send_impl(state, send_id, &[agent_id]);
            }
            CancelOutcome::Cancelled
        }
    } else {
        match workflow_send_owner(state, project_id, send_id) {
            Some(true) => {
                cancel_send_impl(state, send_id, &[agent_id]);
                CancelOutcome::Requested
            }
            Some(false) => return Err(AppError::ReplyInUserWorkflow { send_id }),
            None => return Err(AppError::SendNotCancellable { send_id }),
        }
    };
    let recipients = if outcome == CancelOutcome::AlreadyFinished {
        Vec::new()
    } else {
        vec![agent_id]
    };
    tracing::info!(%project_id, %send_id, %agent_id, ?outcome, "one recipient of a send cancelled");
    Ok(CancelledSend {
        send_id,
        outcome,
        recipients,
    })
}

/// Who started the workflow that made `send_id`, if one of the project's
/// workflows made it: `Some(true)` through the agent tools, `Some(false)` from
/// the window. Read from the origins recorded at each step, which outlive the
/// run: a failed run ends while replies it sent keep running.
fn workflow_send_owner(state: &AppState, project_id: ProjectId, send_id: SendId) -> Option<bool> {
    lock(&state.workflow_send_origins)
        .get(&send_id)
        .filter(|origin| origin.project_id == project_id)
        .map(|origin| origin.through_agent_access)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StoppedAgent {
    pub agent_id: AgentId,
    /// Whether the agent had an actor to deliver the cancel to.
    pub requested: bool,
}

/// Stop an agent: cancel its running turn and drop its queued backlog, as the
/// window's "Stop agent" does.
pub fn stop_agent(
    state: &AppState,
    project_id: ProjectId,
    agent_id: AgentId,
) -> Result<StoppedAgent, AppError> {
    agent_in_project(state, project_id, agent_id)?;
    let outcome = cancel_agent_impl(state, agent_id);
    tracing::info!(%project_id, %agent_id, ?outcome, "agent stopped");
    Ok(StoppedAgent {
        agent_id,
        requested: matches!(outcome, switchboard_dispatcher::CancelOutcome::Requested),
    })
}

// --- waits ---------------------------------------------------------------------

/// Where a wait's answer came from.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaitSource {
    /// The ledger, from the live event stream.
    Live,
    /// The journal and merged conversation: the ledger does not hold the send
    /// (an app restart, or it was pruned).
    Journal,
    /// Neither knows the send.
    Unknown,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WaitSendReply {
    pub send_id: SendId,
    /// The wait was shortened to this many seconds: the client sent no
    /// progress token to keep a longer one alive (or asked for over an hour).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capped_to_seconds: Option<u64>,
    /// Each harness account's limits as last read, as `get_usage` reports them
    /// without each window's credits and duration, on a finished result only:
    /// information for choosing the next step's model and effort. No Codex
    /// account is read for it; each window's `observed_at` says how old it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Vec<super::usage::HarnessUsageReport>>,
    /// Every recipient reached a terminal status. While `false`, recipients
    /// carry statuses, activity, and the end of their text so far, never the
    /// whole reply, so a caller that must call again every 50 seconds pays a
    /// few lines per call.
    pub done: bool,
    pub source: WaitSource,
    pub recipients: Vec<RecipientSnapshot>,
}

/// [`wait_send_unreported`], with the reply saying when the wait was
/// shortened.
pub async fn wait_send(
    state: &AppState,
    project_id: ProjectId,
    send_id: SendId,
    budget: &WaitBudget,
    home_dir: &Path,
) -> Result<WaitSendReply, AppError> {
    let mut reply = wait_send_unreported(state, project_id, send_id, budget, home_dir).await?;
    reply.capped_to_seconds = budget.capped_to_seconds();
    add_context(state, &mut reply);
    if reply.done {
        reply.usage = Some(stored_usage(state));
    }
    Ok(reply)
}

/// Block until every recipient of the send is terminal or the budget ends,
/// and return the current state either way.
async fn wait_send_unreported(
    state: &AppState,
    project_id: ProjectId,
    send_id: SendId,
    budget: &WaitBudget,
    home_dir: &Path,
) -> Result<WaitSendReply, AppError> {
    let signal = state.ledger.subscribe();
    let observed = wait_until(
        budget,
        signal,
        || async move {
            match held_send(state, project_id, send_id) {
                Some(snapshot) if !snapshot.is_done() => Observed::NotYet(Some(snapshot)),
                held => Observed::Done(held),
            }
        },
        || None,
    )
    .await;
    match observed {
        Some(snapshot) => Ok(live_reply(send_id, snapshot)),
        None => unheld_reply(state, project_id, send_id, budget, home_dir).await,
    }
}

/// Several sends waited on together, as [`wait_any_send`] reports them.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WaitAnySendReply {
    /// The wait was shortened to this many seconds: the client sent no
    /// progress token to keep a longer one alive (or asked for over an hour).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capped_to_seconds: Option<u64>,
    /// Each harness account's limits as last read, as `get_usage` reports them
    /// without each window's credits and duration, on a finished result only:
    /// information for choosing the next step's model and effort. No Codex
    /// account is read for it; each window's `observed_at` says how old it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Vec<super::usage::HarnessUsageReport>>,
    /// At least one of the sends is done.
    pub any_done: bool,
    /// Each send asked about, once, in the order asked, as `wait_send`
    /// reports it: a done one with its replies, the others with their
    /// recipients' statuses and activity.
    pub sends: Vec<WaitSendReply>,
}

/// Block until any one of `send_ids` is done or the budget ends, and return
/// every one's current state either way. For work running on several agents
/// at once: a wait on one send would not notice another finishing first.
pub async fn wait_any_send(
    state: &AppState,
    project_id: ProjectId,
    send_ids: Vec<SendId>,
    budget: &WaitBudget,
    home_dir: &Path,
) -> Result<WaitAnySendReply, AppError> {
    let mut ids: Vec<SendId> = Vec::with_capacity(send_ids.len());
    for send_id in send_ids {
        if !ids.contains(&send_id) {
            ids.push(send_id);
        }
    }
    if ids.is_empty() {
        return Err(AppError::InvalidArgument(
            "send_ids must name at least one send".to_owned(),
        ));
    }
    let signal = state.ledger.subscribe();
    let observed = wait_until(
        budget,
        signal,
        || {
            let ids = ids.clone();
            async move {
                let held: Vec<Option<SendSnapshot>> = ids
                    .iter()
                    .map(|send_id| held_send(state, project_id, *send_id))
                    .collect();
                if held
                    .iter()
                    .any(|snapshot| snapshot.as_ref().is_none_or(SendSnapshot::is_done))
                {
                    Observed::Done(held)
                } else {
                    Observed::NotYet(held)
                }
            }
        },
        || ids.iter().map(|_| None).collect(),
    )
    .await;
    let sends =
        futures::future::join_all(ids.iter().zip(observed).map(|(&send_id, held)| async move {
            let mut reply = match held {
                Some(snapshot) => live_reply(send_id, snapshot),
                None => unheld_reply(state, project_id, send_id, budget, home_dir).await?,
            };
            add_context(state, &mut reply);
            Ok::<_, AppError>(reply)
        }))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    let any_done = sends.iter().any(|send| send.done);
    Ok(WaitAnySendReply {
        capped_to_seconds: budget.capped_to_seconds(),
        usage: any_done.then(|| stored_usage(state)),
        any_done,
        sends,
    })
}

/// The send as the ledger holds it, when it is this project's.
fn held_send(state: &AppState, project_id: ProjectId, send_id: SendId) -> Option<SendSnapshot> {
    state
        .ledger
        .send(send_id)
        .filter(|snapshot| snapshot.project_id == project_id)
}

/// A send's recipients as they stand, without waiting: the ledger's snapshot
/// while it holds the send, each completed recipient with its reply even while
/// others still run; otherwise the journal's answer, as after a restart, where
/// `started` means the outcome could not be confirmed. The same reading
/// `wait_send` gives once it stops waiting; a forward naming a reply and
/// `read_workflow_run` read sends through this, so all three agree on every
/// status.
pub(super) async fn current_send(
    state: &AppState,
    project_id: ProjectId,
    send_id: SendId,
    home_dir: &Path,
) -> Result<CurrentSend, AppError> {
    match held_send(state, project_id, send_id) {
        Some(snapshot) => Ok(CurrentSend {
            handed_out: snapshot.settled,
            reply: WaitSendReply {
                send_id,
                capped_to_seconds: None,
                usage: None,
                done: snapshot.is_done(),
                source: WaitSource::Live,
                recipients: snapshot.recipients,
            },
        }),
        None => Ok(CurrentSend {
            handed_out: true,
            reply: journal_fallback(state, project_id, send_id, home_dir).await?,
        }),
    }
}

/// A send as [`current_send`] reads it.
pub(super) struct CurrentSend {
    pub reply: WaitSendReply,
    /// The send has been offered to every recipient it will reach. Until then
    /// a recipient not yet offered reads `not_dispatched` though its turn may
    /// be moments away: a send is handed to its recipients one at a time, and
    /// its id can be known before the last is reached. A send read from the
    /// journal is reported as handed out because the journal cannot say
    /// otherwise: it records a recipient's copy only when that recipient's turn
    /// starts, so a recipient of a message the window sent that is still
    /// waiting in line is absent from it, as if it never received the send.
    pub handed_out: bool,
}

/// A send the ledger holds, as a wait reports it: reply text only once every
/// recipient is done.
fn live_reply(send_id: SendId, snapshot: SendSnapshot) -> WaitSendReply {
    let done = snapshot.is_done();
    let recipients = snapshot
        .recipients
        .into_iter()
        .map(|mut r| {
            if !done {
                r.text = None;
            }
            r
        })
        .collect();
    WaitSendReply {
        send_id,
        capped_to_seconds: None,
        usage: None,
        done,
        source: WaitSource::Live,
        recipients,
    }
}

/// A send the ledger does not hold, answered from the journal within the
/// call's finishing allowance: the fallback reads the journal and
/// transcripts, and a caller that cannot be kept alive must still get an
/// answer before its client gives up. Unfinished, the reply is "not done"
/// with no recipients, and the caller calls again.
async fn unheld_reply(
    state: &AppState,
    project_id: ProjectId,
    send_id: SendId,
    budget: &WaitBudget,
    home_dir: &Path,
) -> Result<WaitSendReply, AppError> {
    match budget
        .run_with_grace(journal_fallback(state, project_id, send_id, home_dir))
        .await
    {
        Outcome::Done(reply) => reply,
        Outcome::Timeout | Outcome::Cancelled => Ok(WaitSendReply {
            send_id,
            capped_to_seconds: None,
            usage: None,
            done: false,
            source: WaitSource::Journal,
            recipients: Vec::new(),
        }),
    }
}

/// A done send's completed recipients carry their agent's context after the
/// turn, for deciding whether to compact before the next step.
fn add_context(state: &AppState, reply: &mut WaitSendReply) {
    if !reply.done {
        return;
    }
    for recipient in &mut reply.recipients {
        if recipient.status == SendStatus::Completed {
            recipient.context = super::read::known_context(state, recipient.agent_id);
        }
    }
}

/// After a restart the ledger is empty, so answer from what is durable: a
/// failed or cancelled `Outcome` record wins over a link; an agent turn joined
/// to the send by a **durable** link with a completed status is `completed`
/// with its text; a `Send` record alone is `started`; nothing is `unknown`.
/// Only a durable link counts, and only with a completed status, because the
/// dispatcher writes links for failed turns too and positional correlation is
/// a guess.
///
/// Transcripts are read in parallel, one per recipient with no recorded
/// outcome, and a read that fails leaves only that recipient `started`, with
/// the read error as its message: "outcome unknown" stays distinct from "the
/// agent failed", and one unreadable file does not hide the others' replies.
async fn journal_fallback(
    state: &AppState,
    project_id: ProjectId,
    send_id: SendId,
    home_dir: &Path,
) -> Result<WaitSendReply, AppError> {
    let project = project_readonly(state, project_id)?;
    let records = read_journal(&project).await?;
    let views = AgentJournalView::by_agent(&records);
    // Recipients in the order the journal first recorded them.
    let mut order: Vec<AgentId> = Vec::new();
    for record in &records {
        if let switchboard_core::JournalRecord::Send {
            send_id: s,
            agent_id,
            ..
        } = record
            && *s == send_id
            && !order.contains(agent_id)
        {
            order.push(*agent_id);
        }
    }
    for record in &records {
        if let switchboard_core::JournalRecord::Outcome {
            send_id: s,
            agent_id,
            ..
        } = record
            && *s == send_id
            && !order.contains(agent_id)
        {
            order.push(*agent_id);
        }
    }
    if order.is_empty() {
        return Ok(WaitSendReply {
            send_id,
            capped_to_seconds: None,
            usage: None,
            done: true,
            source: WaitSource::Unknown,
            recipients: Vec::new(),
        });
    }
    let roster = list_project_agents_readonly_impl(state, project_id)?;
    let recipients = futures::future::join_all(order.into_iter().map(|agent_id| {
        let view = views.get(&agent_id);
        let agent = roster.iter().find(|a| a.id == agent_id);
        fallback_recipient(
            &project,
            send_id,
            agent_id,
            view,
            agent,
            home_dir,
            &state.context,
        )
    }))
    .await;
    Ok(WaitSendReply {
        send_id,
        capped_to_seconds: None,
        usage: None,
        // Nothing more can arrive from a restart's journal; the caller decides
        // what a lingering `started` means.
        done: true,
        source: WaitSource::Journal,
        recipients,
    })
}

/// One recipient of [`journal_fallback`]: its journaled outcome, or else its
/// durably linked completed turn, or else `started`.
async fn fallback_recipient(
    project: &Project,
    send_id: SendId,
    agent_id: AgentId,
    view: Option<&AgentJournalView>,
    agent: Option<&AgentRecord>,
    home_dir: &Path,
    context: &super::context::ContextStore,
) -> RecipientSnapshot {
    let mut recipient = RecipientSnapshot::bare(agent_id, SendStatus::Started);
    match view.and_then(|v| v.outcomes.get(&send_id)) {
        Some((OutcomeStatus::Cancelled, _)) => recipient.status = SendStatus::Cancelled,
        Some((OutcomeStatus::Failed, reason)) => {
            recipient.status = SendStatus::Failed;
            recipient.error.clone_from(reason);
        }
        None => {
            let (Some(view), Some(agent)) = (view, agent) else {
                return recipient;
            };
            match correlate_turns(project, agent, view, home_dir, context).await {
                Ok(turns) => {
                    if let Some(turn) = turns.into_iter().rev().find(|t| {
                        t.send_id == Some(send_id)
                            && t.correlation == Some(SendCorrelation::DurableLink)
                            && t.status == TurnStatus::Complete
                    }) {
                        recipient.status = SendStatus::Completed;
                        recipient.text = Some(turn.text);
                    }
                }
                Err(error) => {
                    recipient.error =
                        Some(format!("could not read this agent's transcript: {error}"));
                }
            }
        }
    }
    recipient
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentWaitStatus {
    /// The dispatcher would accept a send now. An observation: another caller
    /// can occupy the agent before the next send.
    Idle,
    Running,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WaitAgentReply {
    pub agent_id: AgentId,
    /// Whether the agent is producing anything: seconds since its last output
    /// and its open tool calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<super::ledger::AgentActivity>,
    /// The wait was shortened to this many seconds: the client sent no
    /// progress token to keep a longer one alive (or asked for over an hour).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capped_to_seconds: Option<u64>,
    /// Each harness account's limits as last read, as `get_usage` reports them
    /// without each window's credits and duration, on a finished result only:
    /// information for choosing the next step's model and effort. No Codex
    /// account is read for it; each window's `observed_at` says how old it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Vec<super::usage::HarnessUsageReport>>,
    pub status: AgentWaitStatus,
    /// The agent's latest output, once idle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<LatestReply>,
    /// The agent is idle but its output could not be read within the call's
    /// allowance: call `read_latest_reply`. Distinguishes "nothing to show"
    /// from "not read".
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub output_unavailable: bool,
}

/// [`wait_agent_unreported`], with the reply saying when the wait was
/// shortened.
pub async fn wait_agent(
    state: &AppState,
    project_id: ProjectId,
    agent_id: AgentId,
    budget: &WaitBudget,
    home_dir: &Path,
) -> Result<WaitAgentReply, AppError> {
    let mut reply = wait_agent_unreported(state, project_id, agent_id, budget, home_dir).await?;
    reply.capped_to_seconds = budget.capped_to_seconds();
    if reply.status == AgentWaitStatus::Idle {
        reply.usage = Some(stored_usage(state));
    }
    Ok(reply)
}

/// Block until the dispatcher would accept a send to the agent, or the budget
/// ends. Between checks it parks on the ledger's per-agent signal (an event on
/// the agent's channel that can change admission: a turn's start or end, a
/// failed or cancelled message, the agent going idle), never re-polling: while
/// an agent shuts down both dispatcher queries answer instantly, so a polling
/// loop would never yield. The output is read within the call's finishing
/// allowance.
async fn wait_agent_unreported(
    state: &AppState,
    project_id: ProjectId,
    agent_id: AgentId,
    budget: &WaitBudget,
    home_dir: &Path,
) -> Result<WaitAgentReply, AppError> {
    agent_in_project(state, project_id, agent_id)?;
    let signal = state.ledger.subscribe_agent(agent_id);
    let idle = wait_until(
        budget,
        signal,
        || async move {
            if state.dispatcher.accepts_now(agent_id).await {
                Observed::Done(true)
            } else {
                Observed::NotYet(false)
            }
        },
        || false,
    )
    .await;
    if !idle {
        return Ok(WaitAgentReply {
            agent_id,
            activity: state.ledger.activity(agent_id),
            capped_to_seconds: None,
            usage: None,
            status: AgentWaitStatus::Running,
            output: None,
            output_unavailable: false,
        });
    }
    let output = match budget
        .run_with_grace(read_latest_reply(state, project_id, agent_id, home_dir))
        .await
    {
        Outcome::Done(output) => Some(output?),
        Outcome::Timeout | Outcome::Cancelled => None,
    };
    Ok(WaitAgentReply {
        agent_id,
        activity: state.ledger.activity(agent_id),
        capped_to_seconds: None,
        usage: None,
        status: AgentWaitStatus::Idle,
        output_unavailable: output.is_none(),
        output,
    })
}

/// The budget rules for a [`WaitAnySendReply`]: those of [`wait_send_budget`],
/// across every send's recipients, each cut reading on through its own send.
#[must_use]
pub fn wait_any_send_budget(project_id: ProjectId, budget_chars: usize) -> BudgetSpec {
    BudgetSpec {
        budget_chars,
        droppable_list: None,
        read_with: Some("read_reply"),
        pointer: Box::new(move |site: &CutSite<'_>| {
            let agent_id = site.ancestors.last()?.get("agent_id")?.clone();
            let send_id = site
                .ancestors
                .iter()
                .rev()
                .find_map(|object| object.get("send_id"))?
                .clone();
            Some(super::with_cap(
                serde_json::json!({
                    "tool": "read_reply",
                    "project_id": project_id,
                    "send_id": send_id,
                    "agent_id": agent_id,
                    "offset": site.kept_chars,
                }),
                budget_chars,
            ))
        }),
    }
}

/// The budget rules for a [`WaitSendReply`]: reply text may be cut, shared
/// evenly across recipients; recipients are never dropped; a cut reply reads
/// on through `read_reply` from where the cut ended.
#[must_use]
pub fn wait_send_budget(project_id: ProjectId, send_id: SendId, budget_chars: usize) -> BudgetSpec {
    BudgetSpec {
        budget_chars,
        droppable_list: None,
        read_with: Some("read_reply"),
        pointer: Box::new(move |site: &CutSite<'_>| {
            let agent_id = site.ancestors.last()?.get("agent_id")?.clone();
            Some(super::with_cap(
                serde_json::json!({
                    "tool": "read_reply",
                    "project_id": project_id,
                    "send_id": send_id,
                    "agent_id": agent_id,
                    "offset": site.kept_chars,
                }),
                budget_chars,
            ))
        }),
    }
}
