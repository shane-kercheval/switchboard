//! Compaction for callers that are not the window: `compact_agent`.
//!
//! A compaction is a turn, so it runs the send's order: the project is opened,
//! then the three gates (the harness supports compaction, a Claude fork has
//! had its first turn, the agent has a session to summarize), which need only
//! the agent record and the project folder and so refuse at once rather than
//! after a wait for the window; then the window readiness round trip; then
//! the same dispatch the Compact button makes, with the ledger's `message_id`
//! and the admission announced so the window shows the compaction row. Not
//! gated on idle: it queues behind running and queued work, as the button's
//! does.
//!
//! The ledger holds it as a one-recipient entry, so `wait_send` reports
//! `completed`, `failed`, or `cancelled` with no text (a compaction emits no
//! reply), `cancel_send` cancels a queued one, and the per-agent wake-ups
//! cover it. After an app restart it reports `unknown`: nothing journals a
//! compaction, because the journal is the user's side of the conversation.
//! The rule against piling compactions up is the ledger's
//! ([`SendLedger::claim_compaction`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use switchboard_core::{AgentId, ProjectId};
use switchboard_dispatcher::CompactOptions;
use tokio_util::sync::CancellationToken;

use crate::commands::{
    cancel_send_impl, compact_agent_with_impl, compaction_gates, open_project_impl,
};
use crate::error::AppError;
use crate::state::{AppState, lock};

use super::error_code::error_code;
use super::handle::AppStateHandle;
use super::ledger::{CompactionClaim, SendLedger, SendReceipt};
use super::send::{SettleGuard, await_joined, settle_cancelled};
use super::wait::{Outcome, WaitBudget};

pub struct CompactRequest {
    pub project_id: ProjectId,
    pub agent_id: AgentId,
    pub idempotency_key: Option<String>,
}

/// Queue a compaction of `agent_id`'s conversation and return its receipt.
pub async fn compact(
    handle: Arc<dyn AppStateHandle>,
    home_dir: PathBuf,
    request: CompactRequest,
    budget: WaitBudget,
) -> Result<SendReceipt, AppError> {
    let CompactRequest {
        project_id,
        agent_id,
        idempotency_key: key,
    } = request;
    let hash = format!("compact:{project_id}:{agent_id}");
    let ledger: &SendLedger = &handle.app_state().ledger;
    let send_id = match ledger.claim_compaction(project_id, agent_id, key.as_deref(), &hash) {
        CompactionClaim::Fresh { send_id } => send_id,
        CompactionClaim::Joined { send_id, result } => {
            return await_joined(&budget, result, send_id).await;
        }
        CompactionClaim::Reused { receipt } => return Ok(receipt),
        CompactionClaim::Mismatch => {
            return Err(AppError::InvalidArgument(
                "idempotency_key was already used for a different request in this project"
                    .to_owned(),
            ));
        }
    };
    let cancel = budget.cancel_token().child_token();
    ledger.register_compaction(send_id, project_id, agent_id, cancel.clone());
    // Own task, as a send: the caller's future may be dropped at any point
    // from here on without affecting the compaction.
    let task = tokio::spawn(run(
        handle, home_dir, send_id, project_id, agent_id, budget, cancel,
    ));
    task.await.unwrap_or_else(|join_error| {
        Err(AppError::Internal(format!(
            "the compaction task did not finish: {join_error}"
        )))
    })
}

async fn run(
    handle: Arc<dyn AppStateHandle>,
    home_dir: PathBuf,
    send_id: switchboard_core::SendId,
    project_id: ProjectId,
    agent_id: AgentId,
    budget: WaitBudget,
    cancel: CancellationToken,
) -> Result<SendReceipt, AppError> {
    let state = handle.app_state();
    let mut guard = SettleGuard {
        state,
        send_id,
        armed: true,
    };
    let prep = budget.with_cancel(cancel);
    let result = run_inner(state, &home_dir, send_id, project_id, agent_id, &prep).await;
    guard.armed = false;
    match result {
        Ok(receipt) => {
            tracing::info!(%project_id, %send_id, %agent_id, "compaction queued");
            Ok(receipt)
        }
        Err(_) if state.ledger.cancel_requested(send_id) => settle_cancelled(state, send_id),
        Err(error) => {
            tracing::info!(%project_id, %send_id, code = error_code(&error), "compaction refused");
            let error = Arc::new(error);
            state.ledger.release(send_id, &error);
            Err(AppError::Shared(error))
        }
    }
}

async fn run_inner(
    state: &AppState,
    home_dir: &Path,
    send_id: switchboard_core::SendId,
    project_id: ProjectId,
    agent_id: AgentId,
    budget: &WaitBudget,
) -> Result<SendReceipt, AppError> {
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
    // The gates first, on the record and the folder alone.
    let (snapshot_project, _, _) = compaction_gates(state, agent_id, home_dir)?;
    if snapshot_project.id != project_id {
        return Err(AppError::UnknownRecipient {
            project_id,
            agent_id,
        });
    }
    let report = match budget
        .run_with_grace(state.desktop_sync.ensure_ready(project_id))
        .await
    {
        Outcome::Done(report) => report?,
        Outcome::Timeout => return Err(AppError::ProjectLoadTimeout(project_id)),
        Outcome::Cancelled => return Err(AppError::Interrupted),
    };
    if !report.hearing.unheard([agent_id]).is_empty() {
        return Err(AppError::DesktopUnavailable { agent_id });
    }
    if state.ledger.cancel_requested(send_id) {
        return settle_cancelled(state, send_id);
    }
    let Some(message_id) = state.ledger.mark_accepted(send_id, agent_id) else {
        return Err(AppError::Internal(
            "the compaction's ledger entry vanished during dispatch".to_owned(),
        ));
    };
    match compact_agent_with_impl(
        state,
        agent_id,
        send_id,
        home_dir,
        CompactOptions {
            message_id: Some(message_id),
            announce_accepted: true,
        },
    )
    .await
    {
        Ok(_) => {
            // A cancel that arrived between the check above and this admission
            // found nothing in the dispatcher to cancel: cancel it now.
            if state.ledger.cancel_requested(send_id) {
                cancel_send_impl(state, send_id, &[agent_id]);
            }
        }
        Err(error) => {
            state
                .ledger
                .mark_rejected(send_id, agent_id, error_code(&error), &error.to_string());
            return Err(error);
        }
    }
    state.ledger.settle(send_id).ok_or_else(|| {
        AppError::Internal("the compaction's ledger entry vanished after dispatch".to_owned())
    })
}
