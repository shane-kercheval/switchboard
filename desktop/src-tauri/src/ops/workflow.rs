//! Workflow operations: describe, run, wait, list, cancel, and the forward
//! merge the Tauri wrappers share with `run_workflow`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use switchboard_core::{ProjectId, SendId};
use switchboard_workflow::InputValue;
use uuid::Uuid;

use crate::commands::{ForwardSourceRef, open_project_impl, resolve_workflow_forwards};
use crate::error::AppError;
use crate::state::{AppState, lock};
use crate::workflow::ResolvedRecipient;
use crate::workflow_commands::step_label;
use crate::workflow_commands::{
    InvokeOptions, WorkflowFormDescriptor, WorkflowRunInfo, cancel_workflow_run_impl,
    classify_run_file, describe_workflow_form_fresh_impl, invoke_workflow_with, read_run_files,
};

use super::budget::BudgetSpec;
use super::ledger::{RecipientSnapshot, RunSend, RunState, SendStatus};
use super::read::project_readonly;
use super::send::{WaitSource, current_send, wait_any_send_budget};
use super::wait::{Observed, Outcome, WaitBudget, wait_until};

/// Resolve any forward fields (completed-only) and merge the composed text
/// into the invocation inputs, so validation and launch see a forwarded field
/// as a filled value. A still-streaming source rejects the whole invocation.
pub async fn merge_workflow_forwards(
    state: &AppState,
    inputs: &BTreeMap<String, InputValue>,
    forward_sources: &BTreeMap<String, Vec<ForwardSourceRef>>,
    recipient_project: ProjectId,
    home_dir: &Path,
) -> Result<BTreeMap<String, InputValue>, AppError> {
    if forward_sources.values().all(Vec::is_empty) {
        return Ok(inputs.clone());
    }
    let resolved =
        resolve_workflow_forwards(state, forward_sources, inputs, home_dir, recipient_project)
            .await?;
    let mut effective = inputs.clone();
    for (field, text) in resolved {
        effective.insert(field, InputValue::Text(text));
    }
    Ok(effective)
}

/// The workflow's full input form: declared inputs plus the arguments derived
/// from its saved prompts, and whether it is runnable as picked. `project_id`
/// is checked against the store because every operation names its project;
/// the form itself is user-global.
pub async fn describe_workflow(
    state: &AppState,
    project_id: ProjectId,
    name: &str,
    is_builtin: bool,
) -> Result<WorkflowFormDescriptor, AppError> {
    project_readonly(state, project_id)?;
    describe_workflow_form_fresh_impl(state, name, is_builtin).await
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunWorkflowRequest {
    pub project_id: ProjectId,
    pub name: String,
    #[serde(default)]
    pub is_builtin: bool,
    /// Declared inputs and derived prompt arguments. Agent-typed inputs take
    /// agent ids (a string for `agent`, a list for `[agent]`).
    #[serde(default)]
    pub inputs: BTreeMap<String, InputValue>,
    #[serde(default)]
    pub forward_sources: BTreeMap<String, Vec<ForwardSourceRef>>,
}

/// Start a workflow run and return its id. The run reports its status; the
/// caller reads output with `read_agent_output` or `read_send_output`.
///
/// Agent inputs arrive as ids and are converted to names **inside** the
/// invocation, against the roster and workflow it binds with, so a rename and
/// name reuse between two reads cannot run the wrong agent. The window's
/// readiness report is enforced on every recipient the steps resolve to.
///
/// **Preparation runs under the call's budget, as a send's does.** Reading
/// the forwarded sources and the window's readiness answer can each take
/// seconds, and the server's stop must be able to end a start still waiting
/// on them: otherwise a run could launch after agent access was turned off.
/// The cancel is checked once more right before the run starts.
pub async fn run_workflow(
    state: &AppState,
    home_dir: &Path,
    request: RunWorkflowRequest,
    budget: &WaitBudget,
) -> Result<Uuid, AppError> {
    let project_id = request.project_id;
    open_project_impl(state, project_id)?;
    let effective = match budget
        .run(merge_workflow_forwards(
            state,
            &request.inputs,
            &request.forward_sources,
            project_id,
            home_dir,
        ))
        .await
    {
        Outcome::Done(effective) => effective?,
        Outcome::Timeout => {
            return Err(AppError::TimedOut {
                what: "reading the forwarded sources",
            });
        }
        Outcome::Cancelled => return Err(AppError::Interrupted),
    };
    // After the forwards are read, as a send checks just before dispatch, and
    // reported the way a send reports it.
    let report = match budget
        .run_with_grace(state.desktop_sync.ensure_ready(project_id, None))
        .await
    {
        Outcome::Done(report) => report?,
        Outcome::Timeout => return Err(AppError::ProjectLoadTimeout(project_id)),
        Outcome::Cancelled => return Err(AppError::Interrupted),
    };
    if budget.cancel_token().is_cancelled() {
        return Err(AppError::Interrupted);
    }
    let run_id = invoke_workflow_with(
        state,
        project_id,
        &request.name,
        request.is_builtin,
        &effective,
        home_dir,
        &InvokeOptions {
            agent_inputs_are_ids: true,
            hearing: report.hearing,
            through_agent_access: true,
        },
    )?;
    tracing::info!(%project_id, %run_id, workflow = %request.name, "workflow run started");
    Ok(run_id)
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    /// A run file with no terminal record and no live run: the process died
    /// mid-run.
    Interrupted,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WaitWorkflowReply {
    pub run_id: Uuid,
    /// The wait was shortened to this many seconds: the client sent no
    /// progress token to keep a longer one alive (or asked for over an hour).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capped_to_seconds: Option<u64>,
    pub status: WorkflowRunStatus,
    pub done: bool,
    pub workflow: String,
    /// The step in progress, or the one that failed.
    pub step: usize,
    pub total: usize,
    /// That step's label, and its agents as the engine resolved them once it
    /// has (each with `agent_id` and `name`; for a send step, `dispatched`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recipients: Vec<ResolvedRecipient>,
    /// Every send the run's send steps made so far, in step order: follow
    /// one with `wait_send`, and read any recipient's reply with
    /// `read_send_output` and its `send_id`, even after a later step.
    pub sends: Vec<RunSend>,
    /// Switchboard no longer holds the run's live record (after a restart, or
    /// once it pruned the run), so its sends are not known, which is not the
    /// same as a run that made none.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sends_unavailable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The workflow wait's result fits the budget by dropping its oldest sends
/// first, so a run with more sends than one reply holds still reports its
/// status; `items_omitted` says how many were left out, and
/// [`read_workflow_run`] still lists them all. Reaching that takes hundreds of
/// send steps in one fixed workflow file; revisit with real paging when loops
/// (`for_each`) can run.
#[must_use]
pub fn wait_workflow_budget(budget_chars: usize) -> BudgetSpec {
    BudgetSpec::unpageable(
        budget_chars,
        Some("sends"),
        "list_workflow_runs, for the run's step and status",
    )
}

/// Block until the run is terminal or the budget ends. From the ledger's run
/// entries while the ledger holds the run; for one it does not, from the
/// live registry or a retained run file.
pub async fn wait_workflow(
    state: &AppState,
    project_id: ProjectId,
    run_id: Uuid,
    budget: &WaitBudget,
) -> Result<WaitWorkflowReply, AppError> {
    let mut reply = wait_workflow_unreported(state, project_id, run_id, budget).await?;
    reply.capped_to_seconds = budget.capped_to_seconds();
    Ok(reply)
}

async fn wait_workflow_unreported(
    state: &AppState,
    project_id: ProjectId,
    run_id: Uuid,
    budget: &WaitBudget,
) -> Result<WaitWorkflowReply, AppError> {
    let signal = state.ledger.subscribe();
    let observed = wait_until(
        budget,
        signal,
        || async move {
            match observe_run(state, project_id, run_id) {
                Some(reply) if reply.done => Observed::Done(Some(reply)),
                Some(reply) => Observed::NotYet(Some(reply)),
                None => Observed::Done(None),
            }
        },
        || None,
    )
    .await;
    match observed {
        Some(reply) => Ok(reply),
        None => run_from_files(state, project_id, run_id),
    }
}

/// The run as this process holds it now: the ledger's entry, or for a run
/// registered but not yet announced on its channel, the live registry (its
/// first progress event is moments away, so a wait keeps waiting on it).
/// `None` when the process holds neither.
fn observe_run(state: &AppState, project_id: ProjectId, run_id: Uuid) -> Option<WaitWorkflowReply> {
    match state.ledger.run(run_id) {
        Some(run) if run.project_id == project_id => Some(WaitWorkflowReply {
            run_id,
            capped_to_seconds: None,
            status: match run.status {
                RunState::Running => WorkflowRunStatus::Running,
                RunState::Completed => WorkflowRunStatus::Completed,
                RunState::Failed => WorkflowRunStatus::Failed,
                RunState::Cancelled => WorkflowRunStatus::Cancelled,
            },
            done: run.status != RunState::Running,
            workflow: run.workflow,
            step: run.step,
            total: run.total,
            label: run.label,
            recipients: run.recipients,
            sends: run.sends,
            sends_unavailable: false,
            reason: run.reason,
        }),
        _ => live_run(state, project_id, run_id),
    }
}

/// A run this process no longer holds, read from its retained file: a failed
/// or interrupted run after a restart. Its step is known; which agents the
/// engine resolved and which sends it made are not. A completed or cancelled
/// run's file is deleted when it ends, so such a run is not found.
fn run_from_files(
    state: &AppState,
    project_id: ProjectId,
    run_id: Uuid,
) -> Result<WaitWorkflowReply, AppError> {
    let info = list_workflow_runs(state, project_id)?
        .into_iter()
        .find(|r| r.run_id == run_id.to_string())
        .ok_or(AppError::WorkflowRunNotFound { run_id })?;
    let status = match info.status {
        "running" => WorkflowRunStatus::Running,
        "interrupted" => WorkflowRunStatus::Interrupted,
        _ => WorkflowRunStatus::Failed,
    };
    Ok(WaitWorkflowReply {
        run_id,
        capped_to_seconds: None,
        status,
        done: status != WorkflowRunStatus::Running,
        label: step_label(&info.steps, info.step),
        workflow: info.workflow,
        step: info.step,
        total: info.total,
        recipients: Vec::new(),
        sends: Vec::new(),
        sends_unavailable: true,
        reason: info.reason,
    })
}

fn live_run(state: &AppState, project_id: ProjectId, run_id: Uuid) -> Option<WaitWorkflowReply> {
    let runs = lock(&state.workflow_runs);
    let run = runs
        .get(&run_id)
        .filter(|run| run.project_id == project_id)?;
    let step = run.snapshot.current_step;
    let sends = run
        .resolved
        .iter()
        .filter_map(|(&index, resolution)| {
            Some(RunSend {
                step: index,
                label: step_label(&run.steps, index),
                send_id: resolution.send_id?,
                recipients: resolution.recipients.clone(),
            })
        })
        .collect();
    Some(WaitWorkflowReply {
        run_id,
        capped_to_seconds: None,
        status: WorkflowRunStatus::Running,
        done: false,
        workflow: run.workflow.clone(),
        step,
        total: run.snapshot.total_steps,
        label: step_label(&run.steps, step),
        recipients: run
            .resolved
            .get(&step)
            .map(|resolution| resolution.recipients.clone())
            .unwrap_or_default(),
        sends,
        sends_unavailable: false,
        reason: None,
    })
}

/// Every reply a workflow run produced, by send, in step order.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WorkflowRunReplies {
    pub run_id: Uuid,
    pub workflow: String,
    pub status: WorkflowRunStatus,
    pub done: bool,
    /// Every send the run's send steps made so far, in step order, each with
    /// its recipients' replies.
    pub sends: Vec<RunSendReplies>,
    /// Switchboard no longer holds the run's live record (after a restart, or
    /// once it pruned the run), so its sends are not known: the replies are
    /// still in the conversation.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sends_unavailable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One send of a run and its recipients' replies.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RunSendReplies {
    pub step: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub send_id: SendId,
    /// Every recipient reached a terminal status.
    pub done: bool,
    /// Where the statuses came from: `live` while Switchboard holds the send,
    /// else `journal`, where `started` means the outcome could not be
    /// confirmed.
    pub source: WaitSource,
    pub recipients: Vec<RunReply>,
}

/// One recipient's reply to a run's send: as `wait_send` reports it, with
/// the agent's name, and the reply text once that recipient completed, even
/// while the others still run.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RunReply {
    pub name: String,
    #[serde(flatten)]
    pub reply: RecipientSnapshot,
}

/// Every reply a workflow run produced, in step order, without waiting.
///
/// **Why a separate read.** `wait_workflow` lists the run's sends without
/// their text, so a caller that wants the replies reads each recipient with
/// `read_send_output`, one call per reply, and an agent's latest answer holds
/// only its last step. This returns them all at once, a reviewer's first round
/// under its own step. Each send is read as `wait_send` reads it
/// ([`current_send`]): from the ledger while it holds the send, so a running
/// recipient reads `running`; from the journal after that, where `started`
/// means a restart cut the outcome off. A recipient the run resolved but the
/// send's record lacks never received the message.
///
/// The run is found as `wait_workflow` finds it; after a restart only a failed
/// or interrupted run's file remains, which names no sends, and that is said
/// with `sends_unavailable` rather than an empty list that reads as "no
/// replies".
pub async fn read_workflow_run(
    state: &AppState,
    project_id: ProjectId,
    run_id: Uuid,
    home_dir: &Path,
) -> Result<WorkflowRunReplies, AppError> {
    let run = match observe_run(state, project_id, run_id) {
        Some(run) => run,
        None => run_from_files(state, project_id, run_id)?,
    };
    let mut sends = Vec::with_capacity(run.sends.len());
    for send in run.sends {
        let current = current_send(state, project_id, send.send_id, home_dir).await?;
        let recipients = send
            .recipients
            .iter()
            .map(|resolved| {
                let reply = current
                    .recipients
                    .iter()
                    .find(|r| r.agent_id == resolved.agent_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        RecipientSnapshot::bare(resolved.agent_id, SendStatus::NotDispatched)
                    });
                RunReply {
                    name: resolved.name.clone(),
                    reply,
                }
            })
            .collect();
        sends.push(RunSendReplies {
            step: send.step,
            label: send.label,
            send_id: send.send_id,
            done: current.done,
            source: current.source,
            recipients,
        });
    }
    Ok(WorkflowRunReplies {
        run_id,
        workflow: run.workflow,
        status: run.status,
        done: run.done,
        sends,
        sends_unavailable: run.sends_unavailable,
        reason: run.reason,
    })
}

/// The budget rules for [`WorkflowRunReplies`]: those of every reply-carrying
/// result. Reply text is cut evenly across every recipient of every send;
/// sends and recipients are never dropped; a cut reply reads on through
/// `read_send_output` with its own send.
#[must_use]
pub fn read_workflow_run_budget(project_id: ProjectId, budget_chars: usize) -> BudgetSpec {
    wait_any_send_budget(project_id, budget_chars)
}

/// Live runs plus retained failed and interrupted run files, read without
/// loading the project.
pub fn list_workflow_runs(
    state: &AppState,
    project_id: ProjectId,
) -> Result<Vec<WorkflowRunInfo>, AppError> {
    let mut out = Vec::new();
    let mut live_ids: Vec<Uuid> = Vec::new();
    {
        let runs = lock(&state.workflow_runs);
        for (id, run) in runs.iter() {
            if run.project_id != project_id {
                continue;
            }
            live_ids.push(*id);
            out.push(WorkflowRunInfo {
                run_id: id.to_string(),
                workflow: run.workflow.clone(),
                step: run.snapshot.current_step,
                total: run.snapshot.total_steps,
                status: "running",
                reason: None,
                steps: run.steps.clone(),
            });
        }
    }
    let project = project_readonly(state, project_id)?;
    for (run_id, records) in read_run_files(&project.runs_dir()) {
        if live_ids.contains(&run_id) {
            continue;
        }
        if let Some(info) = classify_run_file(run_id, &records) {
            out.push(info);
        }
    }
    Ok(out)
}

/// Fire a running workflow's cancel token.
pub fn cancel_workflow_run(
    state: &AppState,
    project_id: ProjectId,
    run_id: Uuid,
) -> Result<(), AppError> {
    let through_agent_access = lock(&state.workflow_runs)
        .get(&run_id)
        .filter(|run| run.project_id == project_id)
        .map(|run| run.through_agent_access);
    match through_agent_access {
        None => return Err(AppError::WorkflowRunNotFound { run_id }),
        Some(false) => return Err(AppError::RunStartedByUser { run_id }),
        Some(true) => {}
    }
    cancel_workflow_run_impl(state, run_id);
    tracing::info!(%project_id, %run_id, "workflow run cancelled");
    Ok(())
}
