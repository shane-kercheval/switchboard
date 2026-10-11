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
//! **What the budget bounds.** Everything before the first dispatch — the
//! window sync, prompt rendering, a forward's compose — runs under the call's
//! wait budget: at its deadline nothing has gone out, so the call fails and
//! the key is released. The dispatch loop itself is never cut short. A retry
//! that joins an in-flight send waits under its own budget and, if that ends
//! first, reports `request_in_progress` with the send id, leaving the key
//! claimed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use switchboard_core::{AgentId, ProjectId, SendId};
use switchboard_dispatcher::{OnBusy, SendOptions};
use switchboard_harness::{TurnStatus, check_prompt_size, turn_answer_text};
use uuid::Uuid;

use crate::commands::{
    ConversationItem, ForwardArg, ForwardOutcome, ForwardSourceRef, SendCorrelation,
    cancel_agent_impl, cancel_send_impl, combine_prompt_message, dispatch_message_impl,
    forward_message_impl, forward_prompt_impl, list_project_agents_readonly_impl,
    load_project_conversation_impl, open_project_impl,
};
use crate::error::AppError;
use crate::state::{AppState, lock};

use super::RESULT_BUDGET_CHARS;
use super::budget::{BudgetSpec, CutSite};
use super::error_code::error_code;
use super::handle::AppStateHandle;
use super::ledger::{Claim, RecipientSnapshot, SendReceipt, SendStatus};
use super::read::{AgentOutput, agent_in_project, project_readonly, read_agent_output};
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
    pub forward_args: BTreeMap<String, Vec<ForwardSourceRef>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForwardRequest {
    pub project_id: ProjectId,
    /// Sources appended after the text (or after the rendered prompt). A
    /// source may live in another project.
    pub sources: Vec<ForwardSourceRef>,
    pub recipients: Vec<AgentId>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub prompt: Option<ForwardPrompt>,
    #[serde(default)]
    pub on_busy: OnBusyPolicy,
    #[serde(default)]
    pub idempotency_key: Option<String>,
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
        body: match request.body {
            Body::Text { text } => BodyPlan::Text(text),
            Body::Prompt {
                provider,
                name,
                args,
                appended_text,
            } => BodyPlan::Prompt {
                provider,
                name,
                args,
                appended_text: appended_text.unwrap_or_default(),
            },
        },
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
}

#[derive(Serialize)]
enum BodyPlan {
    Text(String),
    Prompt {
        provider: String,
        name: String,
        args: BTreeMap<String, String>,
        appended_text: String,
    },
    Forward {
        sources: Vec<ForwardSourceRef>,
        text: String,
        prompt: Option<ForwardPrompt>,
    },
}

fn request_hash(plan: &Plan) -> String {
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
        Claim::Joined(mut result) => {
            return match budget.run(result.wait_for(Option::is_some)).await {
                Outcome::Done(Ok(settled)) => match settled.clone() {
                    Some(Ok(receipt)) => Ok(receipt),
                    Some(Err(original)) => Err(AppError::Joined(original)),
                    None => Err(AppError::Internal(
                        "the original request settled without a result".to_owned(),
                    )),
                },
                Outcome::Done(Err(_)) => Err(AppError::Internal(
                    "the original request's record was dropped before it settled".to_owned(),
                )),
                // The original keeps running and keeps its key: the caller
                // calls again with the same key.
                Outcome::Timeout | Outcome::Cancelled => Err(AppError::RequestInProgress {
                    send_id: key
                        .as_deref()
                        .and_then(|key| handle.app_state().ledger.send_id_for_key(project_id, key))
                        .unwrap_or_default(),
                }),
            };
        }
        Claim::Mismatch => {
            return Err(AppError::InvalidArgument(
                "idempotency_key was already used for a different request in this project"
                    .to_owned(),
            ));
        }
    };
    handle
        .app_state()
        .ledger
        .register_send(send_id, project_id, &plan.recipients, key.as_deref());
    // Own task: see the module doc. The caller's future may be dropped at
    // any point from here on without affecting the send.
    let task = tokio::spawn(run(handle, home_dir, send_id, plan, budget));
    task.await.unwrap_or_else(|join_error| {
        Err(AppError::Internal(format!(
            "the send task did not finish: {join_error}"
        )))
    })
}

/// Settles the send's ledger entry however the task ends. Lives in the task,
/// not the caller, because the caller may have disconnected; disarmed by a
/// normal finish. On an abnormal end (a panic): nothing accepted → release
/// the key; something accepted → settle what there is, keeping the key so a
/// retry cannot send the accepted recipient again.
struct SettleGuard<'a> {
    state: &'a AppState,
    send_id: SendId,
    armed: bool,
}

impl Drop for SettleGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            settle_abnormally(self.state, self.send_id);
        }
    }
}

/// What the guard does for a send whose task ended abnormally. Separate so
/// the two outcomes are testable without provoking a panic inside a task.
pub(crate) fn settle_abnormally(state: &AppState, send_id: SendId) {
    if state.ledger.dispatched_any(send_id) {
        state.ledger.settle(send_id);
    } else {
        state.ledger.release(
            send_id,
            Arc::new(AppError::Internal(
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
) -> Result<SendReceipt, AppError> {
    let state = handle.app_state();
    let project_id = plan.project_id;
    let mut guard = SettleGuard {
        state,
        send_id,
        armed: true,
    };
    let result = Box::pin(run_inner(state, &home_dir, send_id, plan, &budget)).await;
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
        // Every error path ends with nothing dispatched (a refusal after a
        // dispatch is recorded per recipient, never returned), so the key is
        // safe to release here. The error is shared with any joiner and
        // returned through the same wrapper, so both map to one code.
        Err(error) => {
            tracing::info!(%project_id, %send_id, code = error_code(&error), "send refused");
            let error = Arc::new(error);
            state.ledger.release(send_id, Arc::clone(&error));
            Err(AppError::Joined(error))
        }
    }
}

async fn run_inner(
    state: &AppState,
    home_dir: &Path,
    send_id: SendId,
    plan: Plan,
    budget: &WaitBudget,
) -> Result<SendReceipt, AppError> {
    let project_id = plan.project_id;
    open_project_impl(state, project_id)?;
    let report = match budget
        .run(state.desktop_sync.ensure_ready(project_id))
        .await
    {
        Outcome::Done(report) => report?,
        Outcome::Timeout => return Err(AppError::ProjectLoadTimeout(project_id)),
        Outcome::Cancelled => return Err(AppError::Interrupted),
    };
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

    let body = match plan.body {
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
                return Err(AppError::InvalidArgument(
                    "rendering the saved prompt did not finish within the wait budget".to_owned(),
                ));
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
    // On the final body, after rendering and forwarding: the dispatcher's own
    // check gives the same answer (MCP sends carry no attachments) and stays
    // as the backstop; checking here refuses the whole send before any
    // recipient is dispatched.
    check_prompt_size(&body).map_err(AppError::PromptTooLarge)?;

    if plan.on_busy == OnBusyPolicy::Refuse {
        let mut busy = Vec::new();
        for &agent_id in &plan.recipients {
            if !report.unsubscribed_agents.contains(&agent_id)
                && !state.dispatcher.accepts_now(agent_id).await
            {
                let name = roster
                    .iter()
                    .find(|a| a.id == agent_id)
                    .map_or_else(|| agent_id.to_string(), |a| a.name.clone());
                busy.push(name);
            }
        }
        if !busy.is_empty() {
            return Err(AppError::RecipientsBusy { names: busy });
        }
    }
    dispatch_recipients(
        state,
        home_dir,
        send_id,
        &plan.recipients,
        plan.on_busy,
        &report.unsubscribed_agents,
        &body,
    )
    .await
}

/// Dispatch in order. Under `refuse`, the first refusal stops the loop and
/// leaves the rest `not_dispatched`; under `queue`, every recipient is offered.
async fn dispatch_recipients(
    state: &AppState,
    home_dir: &Path,
    send_id: SendId,
    recipients: &[AgentId],
    policy: OnBusyPolicy,
    unsubscribed: &[AgentId],
    body: &str,
) -> Result<SendReceipt, AppError> {
    let mut dispatched_any = false;
    let mut stopped = false;
    for &agent_id in recipients {
        if stopped {
            // Left `not_dispatched` by registration.
            continue;
        }
        if unsubscribed.contains(&agent_id) {
            let error = AppError::DesktopUnavailable { agent_id };
            state
                .ledger
                .mark_rejected(send_id, agent_id, error_code(&error), &error.to_string());
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
            Vec::new(),
            send_id,
            home_dir,
            on_busy,
            SendOptions {
                message_id: Some(message_id),
                emit_user_message: true,
            },
        )
        .await;
        match outcome {
            Ok(_) => dispatched_any = true,
            Err(error) => {
                state.ledger.mark_rejected(
                    send_id,
                    agent_id,
                    error_code(&error),
                    &error.to_string(),
                );
                if policy == OnBusyPolicy::Refuse {
                    stopped = true;
                }
            }
        }
    }
    if !dispatched_any {
        // The same answer as the up-front refusal, so the caller and a
        // concurrent retry with the same key see one outcome. `run` releases
        // the key.
        let names: Vec<String> = state
            .ledger
            .send(send_id)
            .map(|s| {
                s.recipients
                    .iter()
                    .filter(|r| r.status == SendStatus::Rejected)
                    .map(|r| r.agent_id.to_string())
                    .collect()
            })
            .unwrap_or_default();
        return Err(AppError::RecipientsBusy { names });
    }
    state.ledger.settle(send_id).ok_or_else(|| {
        AppError::Internal("the send's ledger entry vanished after dispatch".to_owned())
    })
}

/// Compose a forward's body through the existing resolvers (they open the
/// sources' projects and wait for a mid-turn source), under the call's budget:
/// the kept-alive caller gets progress while a source finishes, and at the
/// deadline the resolver's future is dropped, which releases its hold (its
/// guard removes the cancel token), so nothing is left registered.
async fn compose_forward(
    state: &AppState,
    home_dir: &Path,
    project_id: ProjectId,
    sources: Vec<ForwardSourceRef>,
    text: String,
    prompt: Option<ForwardPrompt>,
    budget: &WaitBudget,
) -> Result<String, AppError> {
    let forward_id = Uuid::now_v7();
    let all_sources: Vec<ForwardSourceRef> = sources
        .iter()
        .copied()
        .chain(
            prompt
                .iter()
                .flat_map(|p| p.forward_args.values().flatten().copied()),
        )
        .collect();
    let compose = async {
        match prompt {
            None => {
                forward_message_impl(state, text, sources, forward_id, home_dir, project_id).await
            }
            Some(prompt) => {
                let forward_args = forward_args_for(state, &prompt)?;
                forward_prompt_impl(
                    state,
                    prompt.provider,
                    prompt.name,
                    prompt.args,
                    forward_args,
                    text,
                    sources,
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
        Outcome::Timeout => return Err(source_busy(state, &all_sources).await),
        Outcome::Cancelled => return Err(AppError::Interrupted),
    };
    match outcome {
        ForwardOutcome::Resolved { body } => Ok(body),
        ForwardOutcome::Invalidated { reason } => Err(AppError::InvalidArgument(reason)),
        ForwardOutcome::Cancelled => Err(source_busy(state, &all_sources).await),
    }
}

/// Name the sources still running at the deadline, so the caller knows which
/// agent to `wait_agent` on.
async fn source_busy(state: &AppState, sources: &[ForwardSourceRef]) -> AppError {
    let mut names = Vec::new();
    for source in sources {
        if state
            .dispatcher
            .running_turn_kind(source.agent_id)
            .await
            .is_some()
        {
            let name = lock(&state.agents_by_id)
                .get(&source.agent_id)
                .map_or_else(|| source.agent_id.to_string(), |a| a.name.clone());
            names.push(name);
        }
    }
    AppError::SourceBusy {
        name: if names.is_empty() {
            "a forward source".to_owned()
        } else {
            names.join(", ")
        },
    }
}

/// The forwarded arguments with `required` read from the prompt's own schema,
/// never from the caller.
fn forward_args_for(state: &AppState, prompt: &ForwardPrompt) -> Result<Vec<ForwardArg>, AppError> {
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
            Ok(ForwardArg {
                name: name.clone(),
                sources: sources.clone(),
                required: argument.required,
            })
        })
        .collect()
}

// --- cancels -----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CancelledSend {
    pub send_id: SendId,
    /// The recipients the cancel was delivered to: those still accepted or
    /// running when it was issued.
    pub recipients: Vec<AgentId>,
}

/// Cancel a send on every recipient still accepted or running. Needs the
/// ledger's record of the send: a queued send does not survive a restart.
pub fn cancel_send(
    state: &AppState,
    project_id: ProjectId,
    send_id: SendId,
) -> Result<CancelledSend, AppError> {
    let snapshot = state
        .ledger
        .send(send_id)
        .filter(|s| s.project_id == project_id)
        .ok_or(AppError::SendUnknown(send_id))?;
    let recipients: Vec<AgentId> = snapshot
        .recipients
        .iter()
        .filter(|r| !r.status.is_terminal())
        .map(|r| r.agent_id)
        .collect();
    cancel_send_impl(state, send_id, &recipients);
    tracing::info!(%project_id, %send_id, recipients = recipients.len(), "send cancelled");
    Ok(CancelledSend {
        send_id,
        recipients,
    })
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CancelledAgent {
    pub agent_id: AgentId,
    /// Whether the agent had an actor to deliver the cancel to.
    pub requested: bool,
}

/// Stop an agent: cancel its running turn and drop its queued backlog.
pub fn cancel_agent(
    state: &AppState,
    project_id: ProjectId,
    agent_id: AgentId,
) -> Result<CancelledAgent, AppError> {
    agent_in_project(state, project_id, agent_id)?;
    let outcome = cancel_agent_impl(state, agent_id);
    tracing::info!(%project_id, %agent_id, ?outcome, "agent cancelled");
    Ok(CancelledAgent {
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
    /// Every recipient reached a terminal status. While `false`, recipients
    /// carry statuses only, never reply text, so a caller that must call
    /// again every 50 seconds pays a few lines per call.
    pub done: bool,
    pub source: WaitSource,
    pub recipients: Vec<RecipientSnapshot>,
}

/// Block until every recipient of the send is terminal or the budget ends,
/// and return the current state either way.
pub async fn wait_send(
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
        async || match state.ledger.send(send_id) {
            Some(snapshot) if snapshot.project_id == project_id => {
                if snapshot.is_done() {
                    Observed::Done(Some(snapshot))
                } else {
                    Observed::NotYet(Some(snapshot))
                }
            }
            _ => Observed::Done(None),
        },
        || None,
    )
    .await;
    match observed {
        Some(snapshot) => {
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
            Ok(WaitSendReply {
                send_id,
                done,
                source: WaitSource::Live,
                recipients,
            })
        }
        None => journal_fallback(state, project_id, send_id, home_dir).await,
    }
}

/// After a restart the ledger is empty, so answer from what is durable: a
/// failed or cancelled `Outcome` record wins over a link; an agent turn joined
/// to the send by a **durable** link with a completed status is `completed`
/// with its text; a `Send` record alone is `started`; nothing is `unknown`.
/// Only a durable link counts, and only with a completed status, because the
/// dispatcher writes links for failed turns too and positional correlation is
/// a guess.
async fn journal_fallback(
    state: &AppState,
    project_id: ProjectId,
    send_id: SendId,
    home_dir: &Path,
) -> Result<WaitSendReply, AppError> {
    let project = project_readonly(state, project_id)?;
    let records = switchboard_core::journal::read_records(&project.journal_path())?;
    let mut recipients: Vec<RecipientSnapshot> = Vec::new();
    for record in &records {
        match record {
            switchboard_core::JournalRecord::Send {
                send_id: s,
                agent_id,
                ..
            } if *s == send_id && !recipients.iter().any(|r| r.agent_id == *agent_id) => {
                recipients.push(RecipientSnapshot {
                    agent_id: *agent_id,
                    message_id: None,
                    status: SendStatus::Started,
                    text: None,
                    error: None,
                    code: None,
                    failure_kind: None,
                });
            }
            switchboard_core::JournalRecord::Outcome {
                send_id: s,
                agent_id,
                outcome,
                ..
            } if *s == send_id => {
                let (status, reason) = match outcome.get("status").and_then(|v| v.as_str()) {
                    Some("cancelled") => (SendStatus::Cancelled, None),
                    _ => (
                        SendStatus::Failed,
                        outcome
                            .get("message")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                    ),
                };
                match recipients.iter_mut().find(|r| r.agent_id == *agent_id) {
                    Some(r) => {
                        r.status = status;
                        r.error = reason;
                    }
                    None => recipients.push(RecipientSnapshot {
                        agent_id: *agent_id,
                        message_id: None,
                        status,
                        text: None,
                        error: reason,
                        code: None,
                        failure_kind: None,
                    }),
                }
            }
            _ => {}
        }
    }
    if recipients.is_empty() {
        return Ok(WaitSendReply {
            send_id,
            done: true,
            source: WaitSource::Unknown,
            recipients,
        });
    }
    let conversation = load_project_conversation_impl(state, project_id, home_dir).await?;
    for item in &conversation.items {
        if let ConversationItem::AgentTurn {
            agent_id,
            send_id: Some(s),
            send_correlation: Some(SendCorrelation::DurableLink),
            status: TurnStatus::Complete,
            items,
            ..
        } = item
            && *s == send_id
            && let Some(recipient) = recipients
                .iter_mut()
                .find(|r| r.agent_id == *agent_id && r.status == SendStatus::Started)
        {
            recipient.status = SendStatus::Completed;
            recipient.text = Some(turn_answer_text(items));
        }
    }
    Ok(WaitSendReply {
        send_id,
        // Nothing more can arrive from a restart's journal; the caller decides
        // what a lingering `started` means.
        done: true,
        source: WaitSource::Journal,
        recipients,
    })
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentWaitStatus {
    /// The dispatcher would accept a send now. An observation: another caller
    /// can occupy the agent before the next send.
    Idle,
    Running,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WaitAgentReply {
    pub agent_id: AgentId,
    pub status: AgentWaitStatus,
    /// The agent's latest output, once idle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<AgentOutput>,
}

/// Block until the dispatcher would accept a send to the agent, or the budget
/// ends. Between checks it parks on the ledger's per-agent signal (any event
/// on the agent's channel), never re-polling: while an agent shuts down both
/// dispatcher queries answer instantly, so a polling loop would never yield.
pub async fn wait_agent(
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
        async || {
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
            status: AgentWaitStatus::Running,
            output: None,
        });
    }
    let output = read_agent_output(state, project_id, agent_id, home_dir).await?;
    Ok(WaitAgentReply {
        agent_id,
        status: AgentWaitStatus::Idle,
        output: Some(output),
    })
}

/// The budget rules for a [`WaitSendReply`]: reply text may be cut, shared
/// evenly across recipients; recipients are never dropped; a cut reply reads
/// on through `read_send_output` from where the cut ended.
#[must_use]
pub fn wait_send_budget(project_id: ProjectId, send_id: SendId) -> BudgetSpec {
    BudgetSpec {
        budget_chars: RESULT_BUDGET_CHARS,
        droppable_list: None,
        read_with: "read_send_output",
        pointer: Box::new(move |site: &CutSite<'_>| {
            let agent_id = site.ancestors.last()?.get("agent_id")?.clone();
            Some(serde_json::json!({
                "tool": "read_send_output",
                "project_id": project_id,
                "send_id": send_id,
                "agent_id": agent_id,
                "offset": site.kept_chars,
            }))
        }),
    }
}
