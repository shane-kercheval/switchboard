//! The read tier: lists, the conversation, and outputs. Opens nothing and
//! locks nothing, so every function here keeps working for a project the
//! window has not loaded, when the window is slow, and when the working
//! directory is gone.

use std::path::Path;

use serde::Serialize;
use switchboard_core::{AgentId, AgentRecord, Project, ProjectId, SendId};
use switchboard_harness::{TurnStatus, turn_answer_text};

use switchboard_harness::{Turn, TurnOutcome};

use crate::commands::{
    AgentJournalView, ProjectConversation, ProjectListing, SendCorrelation, TurnRender,
    list_project_agents_readonly_impl, list_projects_impl, load_agent_transcript,
    load_project_conversation_impl, open_project_from_store,
};
use crate::error::AppError;
use crate::state::{AppState, lock};
use crate::workflow_commands::{WorkflowListing, list_workflows_impl};

use super::budget::{BudgetSpec, CutSite};
use super::ledger::SendStatus;
use super::{OUTPUT_PAGE_CHARS, RESULT_BUDGET_CHARS};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    /// An agent of the project has running or queued work, or a workflow run
    /// is registered for it.
    Busy,
    Idle,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectStatusListing {
    #[serde(flatten)]
    pub listing: ProjectListing,
    pub status: ProjectStatus,
}

/// Every project in the store with whether it is busy. A project nobody has
/// opened has no registered agents and no run, so it is idle.
pub async fn list_projects(state: &AppState) -> Result<Vec<ProjectStatusListing>, AppError> {
    let listings = list_projects_impl(state)?;
    let mut out = Vec::with_capacity(listings.len());
    for listing in listings {
        let status = if project_is_busy(state, listing.id).await {
            ProjectStatus::Busy
        } else {
            ProjectStatus::Idle
        };
        out.push(ProjectStatusListing { listing, status });
    }
    Ok(out)
}

async fn project_is_busy(state: &AppState, project_id: ProjectId) -> bool {
    if lock(&state.workflow_runs)
        .values()
        .any(|run| run.project_id == project_id)
    {
        return true;
    }
    let agents: Vec<AgentId> = lock(&state.agents_by_id)
        .values()
        .filter(|a| a.project_id == project_id)
        .map(|a| a.id)
        .collect();
    for agent_id in agents {
        if state.dispatcher.has_pending_work(agent_id).await {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentListing {
    #[serde(flatten)]
    pub record: AgentRecord,
    /// Whether a send to the agent would be refused right now (an
    /// observation; another caller can occupy it before the next send).
    pub busy: bool,
}

/// The project's roster with each agent's model, effort, quick choices, and
/// whether it would accept a send now.
pub async fn list_agents(
    state: &AppState,
    project_id: ProjectId,
) -> Result<Vec<AgentListing>, AppError> {
    let records = list_project_agents_readonly_impl(state, project_id)?;
    let mut out = Vec::with_capacity(records.len());
    for record in records {
        let busy = !state.dispatcher.accepts_now(record.id).await;
        out.push(AgentListing { record, busy });
    }
    Ok(out)
}

/// The project's merged conversation, cut to its last `last_n_items` items,
/// in the shape the desktop renders.
///
/// Known limitation: no cursor paging. A caller that needs older items than
/// one result holds has no way to ask for them; the remote-control plan's
/// windowed transcript is where that arrives.
pub async fn read_conversation(
    state: &AppState,
    project_id: ProjectId,
    last_n_items: Option<usize>,
    home_dir: &Path,
) -> Result<ProjectConversation, AppError> {
    let mut conversation = load_project_conversation_impl(state, project_id, home_dir).await?;
    if let Some(n) = last_n_items
        && conversation.items.len() > n
    {
        let cut = conversation.items.len() - n;
        conversation.items.drain(..cut);
    }
    Ok(conversation)
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputSource {
    /// The ledger's captured text.
    Live,
    /// The merged conversation, joined to the send by a durable journal link.
    DurableLink,
    /// The merged conversation, joined to the send by order. A guess: trust it
    /// less than a durable link.
    Positional,
    /// The journal's outcome record for a turn that did not complete.
    Journal,
    /// No send could be assigned to the turn.
    Uncorrelated,
}

/// One page of one recipient's reply to one send.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OutputPage {
    pub send_id: SendId,
    pub agent_id: AgentId,
    pub status: SendStatus,
    pub text: String,
    pub offset: usize,
    pub total_chars: usize,
    /// Where the next page starts, or `None` on the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
    pub source: OutputSource,
}

/// One recipient's reply to one send, paged from `offset` (in characters).
/// The ledger's captured text when it holds the send, otherwise the agent
/// turn in the merged conversation whose `send_id` matches. The stable way to
/// read a long reply: a new turn on the same agent does not move this one.
pub async fn read_send_output(
    state: &AppState,
    project_id: ProjectId,
    send_id: SendId,
    agent_id: AgentId,
    offset: usize,
    home_dir: &Path,
) -> Result<OutputPage, AppError> {
    if let Some(snapshot) = state.ledger.send(send_id)
        && snapshot.project_id == project_id
    {
        let recipient = snapshot
            .recipients
            .into_iter()
            .find(|r| r.agent_id == agent_id)
            .ok_or(AppError::UnknownRecipient {
                project_id,
                agent_id,
            })?;
        let text = recipient.text.unwrap_or_default();
        return Ok(page(
            send_id,
            agent_id,
            recipient.status,
            &text,
            offset,
            OutputSource::Live,
        ));
    }
    let agent = agent_in_project(state, project_id, agent_id)?;
    let correlated = correlate_agent(state, project_id, &agent, home_dir).await?;
    let turn = correlated.iter().rev().find_map(|turn| match turn {
        CorrelatedTurn {
            send_id: Some(s),
            correlation,
            status,
            text,
        } if *s == send_id => Some((*correlation, *status, text.as_str())),
        _ => None,
    });
    let Some((correlation, status, text)) = turn else {
        return Err(AppError::SendUnknown(send_id));
    };
    let source = match correlation {
        Some(SendCorrelation::DurableLink) => OutputSource::DurableLink,
        _ => OutputSource::Positional,
    };
    let status = match status {
        TurnStatus::Complete => SendStatus::Completed,
        TurnStatus::Streaming => SendStatus::Running,
        TurnStatus::Failed => SendStatus::Failed,
    };
    Ok(page(send_id, agent_id, status, text, offset, source))
}

/// One of an agent's transcript turns, correlated to its send exactly as the
/// merged conversation correlates it, with its answer text.
struct CorrelatedTurn {
    send_id: Option<SendId>,
    correlation: Option<SendCorrelation>,
    status: TurnStatus,
    text: String,
}

/// Read one agent's transcript and correlate its turns to its journaled
/// sends with the merge's own per-agent classifier — one file parsed, the
/// same matching the merged conversation applies, so a `send_id` reported
/// here pages the same reply through `read_send_output`.
async fn correlate_agent(
    state: &AppState,
    project_id: ProjectId,
    agent: &AgentRecord,
    home_dir: &Path,
) -> Result<Vec<CorrelatedTurn>, AppError> {
    let project = project_readonly(state, project_id)?;
    let records = switchboard_core::journal::read_records(&project.journal_path())?;
    let view = AgentJournalView::from_records(&records, agent.id);
    let transcript = {
        let project = project.clone();
        let agent = agent.clone();
        let home_dir = home_dir.to_path_buf();
        tokio::task::spawn_blocking(move || load_agent_transcript(&project, &agent, &home_dir))
            .await
            .map_err(|e| AppError::Internal(format!("transcript load task failed: {e}")))??
    };
    let renders = view.classify(&transcript.turns);
    Ok(transcript
        .turns
        .iter()
        .zip(renders)
        .filter_map(|(turn, render)| match (turn, render) {
            (
                Turn::Agent { status, items, .. },
                TurnRender::Agent {
                    send_id,
                    correlation,
                },
            ) => Some(CorrelatedTurn {
                send_id,
                correlation,
                status: *status,
                text: turn_answer_text(items),
            }),
            _ => None,
        })
        .collect())
}

/// Characters a character costs once JSON-escaped: quotes, backslashes, and
/// control characters grow, so a page is sized on what it will serialize to,
/// never on raw characters, or a reply full of code could exceed the budget.
fn escaped_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' => 2,
        c if (c as u32) < 0x20 => 6,
        _ => 1,
    }
}

fn page(
    send_id: SendId,
    agent_id: AgentId,
    status: SendStatus,
    text: &str,
    offset: usize,
    source: OutputSource,
) -> OutputPage {
    let total_chars = text.chars().count();
    let mut budget = OUTPUT_PAGE_CHARS;
    let page: String = text
        .chars()
        .skip(offset)
        .take_while(|c| {
            let cost = escaped_len(*c);
            if cost > budget {
                return false;
            }
            budget -= cost;
            true
        })
        .collect();
    let end = offset + page.chars().count();
    OutputPage {
        send_id,
        agent_id,
        status,
        text: page,
        offset,
        total_chars,
        next_offset: (end < total_chars).then_some(end),
        source,
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutputStatus {
    /// `text` is the latest completed answer.
    Completed,
    /// The latest turn failed or was cancelled; `text` is the note the forward
    /// resolver would send in its place.
    Failed,
    Cancelled,
    /// The agent has no completed turn.
    None,
}

/// An agent's latest output, for turns the caller did not send.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentOutput {
    pub agent_id: AgentId,
    pub status: AgentOutputStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The send the latest turn answered, when known, so a caller pages with
    /// `read_send_output`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_id: Option<SendId>,
    pub source: OutputSource,
}

/// The agent's latest completed answer: the ledger's text when the agent's
/// newest turn is one the ledger holds and it completed; otherwise the
/// journal's typed outcome for a latest turn that failed or was cancelled
/// (with the note the forward resolver would send in its place); otherwise
/// the newest completed turn in the agent's own transcript, correlated to its
/// send with the merge's per-agent classifier. A `send_id` is reported only
/// when that classifier assigns one: guessing from the last journaled send
/// would name a newer turn that is still running.
pub async fn read_agent_output(
    state: &AppState,
    project_id: ProjectId,
    agent_id: AgentId,
    home_dir: &Path,
) -> Result<AgentOutput, AppError> {
    let agent = agent_in_project(state, project_id, agent_id)?;
    if let Some((send_id, latest)) = state.ledger.latest_turn(agent_id)
        && latest.status == SendStatus::Completed
    {
        return Ok(AgentOutput {
            agent_id,
            status: AgentOutputStatus::Completed,
            text: latest.text,
            send_id: Some(send_id),
            source: OutputSource::Live,
        });
    }
    let project = project_readonly(state, project_id)?;
    let records = switchboard_core::journal::read_records(&project.journal_path())?;
    if let Some((outcome, note)) =
        crate::commands::latest_turn_outcome(&records, agent_id, &agent.name)
    {
        let latest_send = records
            .iter()
            .filter_map(|r| match r {
                switchboard_core::JournalRecord::Send {
                    send_id,
                    agent_id: a,
                    ..
                } if *a == agent_id => Some(*send_id),
                _ => None,
            })
            .next_back();
        let status = match outcome {
            Some(TurnOutcome::Cancelled { .. }) => AgentOutputStatus::Cancelled,
            _ => AgentOutputStatus::Failed,
        };
        return Ok(AgentOutput {
            agent_id,
            status,
            text: Some(note),
            send_id: latest_send,
            source: OutputSource::Journal,
        });
    }
    let correlated = correlate_agent(state, project_id, &agent, home_dir).await?;
    let latest = correlated
        .into_iter()
        .rev()
        .find(|turn| turn.status == TurnStatus::Complete);
    Ok(match latest {
        Some(turn) => AgentOutput {
            agent_id,
            status: AgentOutputStatus::Completed,
            text: Some(turn.text),
            send_id: turn.send_id,
            source: match turn.correlation {
                Some(SendCorrelation::DurableLink) => OutputSource::DurableLink,
                Some(SendCorrelation::Positional) => OutputSource::Positional,
                _ => OutputSource::Uncorrelated,
            },
        },
        None => AgentOutput {
            agent_id,
            status: AgentOutputStatus::None,
            text: None,
            send_id: None,
            source: OutputSource::Uncorrelated,
        },
    })
}

/// The budget rules for a [`ProjectConversation`]: text, tool input and
/// output may be cut; the oldest items are dropped when the structure alone is
/// over; an agent turn's text reads on through `read_send_output` when the
/// turn has a send, and nothing else can be read on.
#[must_use]
pub fn conversation_budget(project_id: ProjectId) -> BudgetSpec {
    BudgetSpec {
        budget_chars: RESULT_BUDGET_CHARS,
        droppable_list: Some("items"),
        read_with: "read_conversation with a smaller last_n_items",
        pointer: Box::new(move |site: &CutSite<'_>| {
            // The item (`ConversationItem::AgentTurn`) is the ancestor that
            // carries the send; the cut field's own object is a `TurnItem`.
            let turn = site.ancestors.iter().rev().find(|o| {
                o.get("kind").and_then(serde_json::Value::as_str) == Some("agent_turn")
            })?;
            let send_id = turn.get("send_id")?.as_str()?;
            let agent_id = turn.get("agent_id")?.clone();
            (site.key == "text").then(|| {
                serde_json::json!({
                    "tool": "read_send_output",
                    "project_id": project_id,
                    "send_id": send_id,
                    "agent_id": agent_id,
                    "offset": 0,
                })
            })
        }),
    }
}

/// The budget rules for an [`AgentOutput`]: its text may be cut and reads on
/// through `read_send_output` when the output has a send.
#[must_use]
pub fn agent_output_budget(project_id: ProjectId) -> BudgetSpec {
    BudgetSpec {
        budget_chars: RESULT_BUDGET_CHARS,
        droppable_list: None,
        read_with: "read_send_output",
        pointer: Box::new(move |site: &CutSite<'_>| {
            let output = site.ancestors.last()?;
            let send_id = output.get("send_id")?.as_str()?;
            Some(serde_json::json!({
                "tool": "read_send_output",
                "project_id": project_id,
                "send_id": send_id,
                "agent_id": output.get("agent_id")?.clone(),
                "offset": site.kept_chars,
            }))
        }),
    }
}

#[must_use]
pub fn list_prompts(state: &AppState) -> Vec<switchboard_prompts::Prompt> {
    crate::commands::list_prompts_impl(state)
}

#[must_use]
pub fn list_workflows(state: &AppState) -> Vec<WorkflowListing> {
    list_workflows_impl(state)
}

/// The project as loaded, or read from the store without loading or locking
/// it.
pub(crate) fn project_readonly(
    state: &AppState,
    project_id: ProjectId,
) -> Result<Project, AppError> {
    if let Some(loaded) = lock(&state.projects).get(&project_id).cloned() {
        return Ok(loaded);
    }
    open_project_from_store(state, project_id)
}

/// The agent's record if it is in `project_id`'s roster, read without loading
/// the project.
pub(crate) fn agent_in_project(
    state: &AppState,
    project_id: ProjectId,
    agent_id: AgentId,
) -> Result<AgentRecord, AppError> {
    list_project_agents_readonly_impl(state, project_id)?
        .into_iter()
        .find(|a| a.id == agent_id)
        .ok_or(AppError::AgentNotFound(agent_id))
}
