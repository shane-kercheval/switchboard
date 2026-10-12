//! The read tier: lists, the conversation, and outputs. Opens nothing and
//! locks nothing, so every function here keeps working for a project the
//! window has not loaded, when the window is slow, and when the working
//! directory is gone.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use switchboard_core::{AgentId, AgentRecord, Project, ProjectId, SendId};
use switchboard_harness::{SystemMarker, Turn, TurnOutcome, TurnStatus, turn_answer_text};

use crate::commands::{
    AgentJournalView, ConversationItem, OutcomeStatus, ProjectConversation, ProjectListing,
    SendCorrelation, TurnRender, list_project_agents_readonly_impl, list_projects_impl,
    load_agent_transcript, load_project_conversation_impl, open_project_from_store,
};
use crate::error::AppError;
use crate::state::{AppState, lock};
use crate::workflow_commands::{WorkflowListing, list_workflows_impl};

use super::budget::{BudgetSpec, CutSite, json_escaped_len};
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

/// A project's conversation as an orchestrating agent reads it: what was sent,
/// what each agent answered, and which turns failed or were cancelled.
///
/// **Not the shape the window renders.** The window's items carry every tool
/// call with its input, output, and file diffs; one ordinary agentic turn
/// serializes past the result budget on those alone. Here an agent turn is
/// its answer text — every text block, the same text a forward sends — and
/// tool calls, their output, and thinking are always left out, so an empty
/// `text` means the turn wrote no text, not that it did nothing. Harness
/// bookkeeping is left out too, except a one-line marker where an agent's
/// context was compacted: no reply says so, and an agent may have lost
/// earlier instructions there.
///
/// **One status per turn.** A journaled failure or cancellation is folded
/// into the turn it ended, because the session file can record a cancelled
/// partial reply as complete, and the journal's marker sorts before the reply
/// (it carries the dispatch time), so trimming the oldest items could keep
/// the reply and lose the marker. A standalone outcome remains only for a
/// send that left no reply.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConversationDigest {
    pub items: Vec<DigestItem>,
    pub agents: Vec<DigestAgent>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DigestItem {
    /// One send, once however many recipients it had. No `send_id` for a
    /// prompt imported from a session that predates Switchboard.
    UserMessage {
        #[serde(skip_serializing_if = "Option::is_none")]
        send_id: Option<SendId>,
        agent_ids: Vec<AgentId>,
        text: String,
        /// Labels of the files attached, as the text refers to them.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<String>,
        at: DateTime<Utc>,
    },
    /// One agent's turn. With a `send_id`, `read_send_output` with
    /// [`ReadFrom::Transcript`] pages exactly this text; `send_correlation`
    /// says whether that match is a durable link or positional (a guess), and
    /// so how far to trust a journaled outcome applied through it.
    AgentTurn {
        agent_id: AgentId,
        #[serde(skip_serializing_if = "Option::is_none")]
        send_id: Option<SendId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        send_correlation: Option<SendCorrelation>,
        status: DigestTurnStatus,
        /// Why the turn failed or was cancelled, when the journal says.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The failure's kind (`usage_limit`, `auth_failure`, …), so a caller
        /// does not retry into a limit.
        #[serde(skip_serializing_if = "Option::is_none")]
        failure_kind: Option<String>,
        text: String,
        at: DateTime<Utc>,
    },
    /// A send that failed or was cancelled before the agent replied.
    Outcome {
        send_id: SendId,
        agent_id: AgentId,
        status: OutcomeStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        failure_kind: Option<String>,
        at: DateTime<Utc>,
    },
    /// The agent's harness compacted its context here: the agent may no
    /// longer hold instructions or decisions from before this point. A reply
    /// compacted mid-way continues after the marker as a turn with no send.
    Compacted {
        agent_id: AgentId,
        at: DateTime<Utc>,
    },
}

/// An agent turn's final status, with the journal's outcome applied.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DigestTurnStatus {
    Complete,
    Running,
    Failed,
    Cancelled,
}

/// An agent of the project, and whether its history loaded.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DigestAgent {
    pub agent_id: AgentId,
    pub name: String,
    /// Its transcript could not be read, so its turns are missing above.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_error: Option<String>,
    /// Lines of its transcript that could not be parsed and were skipped.
    #[serde(skip_serializing_if = "is_zero")]
    pub parse_warnings: usize,
}

/// A journaled non-completed outcome: status, reason, failure kind.
type JournalOutcome = (OutcomeStatus, Option<String>, Option<String>);

/// One recipient of one send.
type SendTo = (SendId, AgentId);

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The project's conversation, cut to its last `last_n_items` items, as a
/// [`ConversationDigest`].
///
/// Known limitation: no cursor paging. A caller that needs older items than
/// one result holds has no way to ask for them; the remote-control plan's
/// windowed transcript is where that arrives.
pub async fn read_conversation(
    state: &AppState,
    project_id: ProjectId,
    last_n_items: Option<usize>,
    home_dir: &Path,
) -> Result<ConversationDigest, AppError> {
    let conversation = load_project_conversation_impl(state, project_id, home_dir).await?;
    let roster = list_project_agents_readonly_impl(state, project_id)?;
    let mut digest = digest(conversation, &roster);
    if let Some(n) = last_n_items
        && digest.items.len() > n
    {
        let cut = digest.items.len() - n;
        digest.items.drain(..cut);
    }
    Ok(digest)
}

fn digest(conversation: ProjectConversation, roster: &[AgentRecord]) -> ConversationDigest {
    let (outcomes, answered) = journal_outcomes(&conversation.items);
    let items = conversation
        .items
        .into_iter()
        .filter_map(|item| match item {
            ConversationItem::UserMessage {
                send_id,
                agent_ids,
                text,
                attachments,
                at,
                ..
            } => Some(DigestItem::UserMessage {
                send_id,
                agent_ids,
                text,
                attachments: attachments.into_iter().map(|a| a.label).collect(),
                at,
            }),
            ConversationItem::AgentTurn {
                agent_id,
                send_id,
                send_correlation,
                status,
                items,
                started_at,
                ..
            } => {
                let outcome = send_id.and_then(|s| outcomes.get(&(s, agent_id)));
                let (status, reason, failure_kind) = turn_status(status, outcome);
                Some(DigestItem::AgentTurn {
                    agent_id,
                    send_id,
                    send_correlation,
                    status,
                    reason,
                    failure_kind,
                    text: turn_answer_text(&items),
                    at: started_at,
                })
            }
            ConversationItem::Outcome {
                send_id,
                agent_id,
                status,
                reason,
                failure_kind,
                at,
                ..
            } => (!answered.contains(&(send_id, agent_id))).then_some(DigestItem::Outcome {
                send_id,
                agent_id,
                status,
                reason,
                failure_kind,
                at,
            }),
            ConversationItem::SystemMarker {
                agent_id,
                marker: SystemMarker::Compaction { .. },
                at,
                ..
            } => Some(DigestItem::Compacted { agent_id, at }),
            ConversationItem::SystemMarker { .. } => None,
        })
        .collect();
    let agents = conversation
        .agents
        .into_iter()
        .map(|meta| DigestAgent {
            agent_id: meta.agent_id,
            name: roster
                .iter()
                .find(|a| a.id == meta.agent_id)
                .map(|a| a.name.clone())
                .unwrap_or_default(),
            load_error: meta.load_error,
            parse_warnings: meta.warnings.len(),
        })
        .collect();
    ConversationDigest { items, agents }
}

/// The journal's outcome for each `(send, agent)`, and which of those sends
/// have a reply in the conversation to fold it into.
fn journal_outcomes(
    items: &[ConversationItem],
) -> (HashMap<SendTo, JournalOutcome>, HashSet<SendTo>) {
    let mut outcomes: HashMap<SendTo, JournalOutcome> = HashMap::new();
    let mut answered: HashSet<SendTo> = HashSet::new();
    for item in items {
        match item {
            ConversationItem::Outcome {
                send_id,
                agent_id,
                status,
                reason,
                failure_kind,
                ..
            } => {
                outcomes.insert(
                    (*send_id, *agent_id),
                    (*status, reason.clone(), failure_kind.clone()),
                );
            }
            ConversationItem::AgentTurn {
                send_id: Some(send_id),
                agent_id,
                ..
            } => {
                answered.insert((*send_id, *agent_id));
            }
            _ => {}
        }
    }
    (outcomes, answered)
}

/// A turn's final status: the journal's failure or cancellation when it
/// recorded one, otherwise what the session file says.
fn turn_status(
    status: TurnStatus,
    outcome: Option<&JournalOutcome>,
) -> (DigestTurnStatus, Option<String>, Option<String>) {
    match outcome {
        Some((OutcomeStatus::Cancelled, reason, _)) => {
            (DigestTurnStatus::Cancelled, reason.clone(), None)
        }
        Some((OutcomeStatus::Failed, reason, kind)) => {
            (DigestTurnStatus::Failed, reason.clone(), kind.clone())
        }
        None => (
            match status {
                TurnStatus::Complete => DigestTurnStatus::Complete,
                TurnStatus::Streaming => DigestTurnStatus::Running,
                TurnStatus::Failed => DigestTurnStatus::Failed,
            },
            None,
            None,
        ),
    }
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

/// Which copy of a reply [`read_send_output`] pages.
///
/// A completed reply exists twice: the text the app captured as the turn
/// streamed, and the session file's. They agree for a whole turn, but the
/// session file splits a reply that spans a context compaction into two
/// turns, and only the first carries the send. An offset is only meaningful
/// in the copy it was taken from, so a caller reading on from a cut keeps to
/// that copy.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadFrom {
    /// The captured text when the app holds it (a completed reply sent since
    /// the app started), otherwise the session file's. What `wait_send` and
    /// `read_agent_output` return, and so where their cuts read on.
    #[default]
    Auto,
    /// The session file's, as `read_conversation` shows the reply.
    Transcript,
}

/// One recipient's reply to one send, paged from `offset` (in characters),
/// from the copy `read_from` names. Without captured text — the send is still
/// running, failed, or was cancelled, or the app restarted since — the session
/// file's is read, so the answer does not change across a restart. The stable
/// way to read a long reply: a new turn on the same agent does not move this
/// one.
pub async fn read_send_output(
    state: &AppState,
    project_id: ProjectId,
    send_id: SendId,
    agent_id: AgentId,
    offset: usize,
    read_from: ReadFrom,
    home_dir: &Path,
) -> Result<OutputPage, AppError> {
    let live_status = match state.ledger.send(send_id) {
        Some(snapshot) if snapshot.project_id == project_id => {
            let recipient = snapshot
                .recipients
                .into_iter()
                .find(|r| r.agent_id == agent_id)
                .ok_or(AppError::UnknownRecipient {
                    project_id,
                    agent_id,
                })?;
            if read_from == ReadFrom::Auto
                && let Some(text) = &recipient.text
            {
                return Ok(page(
                    send_id,
                    agent_id,
                    recipient.status,
                    text,
                    offset,
                    OutputSource::Live,
                ));
            }
            Some(recipient.status)
        }
        _ => None,
    };
    let agent = agent_in_project(state, project_id, agent_id)?;
    let (view, correlated) = correlate_agent(state, project_id, &agent, home_dir).await?;
    let turn = correlated
        .iter()
        .rev()
        .find(|turn| turn.send_id == Some(send_id));
    // The ledger's status is the most current when it holds the send. Without
    // it, the journal's recorded outcome wins over the transcript, as it does
    // for `wait_send` after a restart: a failed or cancelled send with partial
    // content on disk is not a completed answer, and one with no reply at all
    // still pages (empty) rather than reading as unknown.
    let status = match (live_status, view.outcomes.get(&send_id), turn) {
        (Some(status), _, _) => status,
        (None, Some((OutcomeStatus::Cancelled, _)), _) => SendStatus::Cancelled,
        (None, Some((OutcomeStatus::Failed, _)), _) => SendStatus::Failed,
        (None, None, Some(turn)) => match turn.status {
            TurnStatus::Complete => SendStatus::Completed,
            TurnStatus::Streaming => SendStatus::Running,
            TurnStatus::Failed => SendStatus::Failed,
        },
        (None, None, None) if view.sends.iter().any(|(s, _)| *s == send_id) => SendStatus::Started,
        (None, None, None) => return Err(AppError::SendUnknown(send_id)),
    };
    let source = match (turn.and_then(|t| t.correlation), live_status) {
        (Some(SendCorrelation::DurableLink), _) => OutputSource::DurableLink,
        (Some(SendCorrelation::Positional), _) => OutputSource::Positional,
        (_, Some(_)) => OutputSource::Live,
        _ => OutputSource::Journal,
    };
    let text = turn.map_or("", |t| t.text.as_str());
    Ok(page(send_id, agent_id, status, text, offset, source))
}

/// One of an agent's transcript turns, correlated to its send exactly as the
/// merged conversation correlates it, with its answer text.
pub(crate) struct CorrelatedTurn {
    pub send_id: Option<SendId>,
    pub correlation: Option<SendCorrelation>,
    pub status: TurnStatus,
    pub text: String,
}

/// The project's journal, read off the async workers: a blocking read cannot
/// be interrupted by a wait's deadline, so it must not hold a worker thread.
pub(crate) async fn read_journal(
    project: &Project,
) -> Result<Vec<switchboard_core::JournalRecord>, AppError> {
    let path = project.journal_path();
    tokio::task::spawn_blocking(move || switchboard_core::journal::read_records(&path))
        .await
        .map_err(|e| AppError::Internal(format!("journal read task failed: {e}")))?
        .map_err(AppError::from)
}

/// Read one agent's transcript and correlate its turns to its journaled
/// sends with the merge's own per-agent derivation — one file parsed, the
/// same matching the merged conversation applies, so a `send_id` reported
/// here pages the same reply through `read_send_output`.
pub(crate) async fn correlate_turns(
    project: &Project,
    agent: &AgentRecord,
    view: &AgentJournalView,
    home_dir: &Path,
) -> Result<Vec<CorrelatedTurn>, AppError> {
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

/// [`correlate_turns`] for an agent of `project_id`, with the agent's journal
/// view it used.
async fn correlate_agent(
    state: &AppState,
    project_id: ProjectId,
    agent: &AgentRecord,
    home_dir: &Path,
) -> Result<(AgentJournalView, Vec<CorrelatedTurn>), AppError> {
    let project = project_readonly(state, project_id)?;
    let records = read_journal(&project).await?;
    let view = AgentJournalView::from_records(&records, agent.id);
    let turns = correlate_turns(&project, agent, &view, home_dir).await?;
    Ok((view, turns))
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
            let cost = json_escaped_len(*c);
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
    let records = read_journal(&project).await?;
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
    let view = AgentJournalView::from_records(&records, agent_id);
    let correlated = correlate_turns(&project, &agent, &view, home_dir).await?;
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

/// The budget rules for a [`ConversationDigest`]: message and answer text may
/// be cut; the oldest items are dropped when the structure alone is over; an
/// agent turn's text reads on through `read_send_output` from the session
/// file's copy, from where it was cut, when the turn has a send; a user
/// message's cannot be read on.
#[must_use]
pub fn conversation_budget(project_id: ProjectId) -> BudgetSpec {
    BudgetSpec {
        budget_chars: RESULT_BUDGET_CHARS,
        droppable_list: Some("items"),
        read_with: Some("read_conversation with a smaller last_n_items"),
        pointer: Box::new(move |site: &CutSite<'_>| {
            let item = site.ancestors.last()?;
            if site.key != "text"
                || item.get("kind").and_then(serde_json::Value::as_str) != Some("agent_turn")
            {
                return None;
            }
            let send_id = item.get("send_id")?.as_str()?;
            // The conversation's text is the session file's, so reading on
            // keeps to that copy (see `ReadFrom`).
            Some(serde_json::json!({
                "tool": "read_send_output",
                "project_id": project_id,
                "send_id": send_id,
                "agent_id": item.get("agent_id")?.clone(),
                "offset": site.kept_chars,
                "read_from": ReadFrom::Transcript,
            }))
        }),
    }
}

/// The budget rules for an [`AgentOutput`]: a reply may be cut and reads on
/// through `read_send_output`, from the copy its text came from, when the
/// output has a send. A journal note is not a reply and has no read-on.
#[must_use]
pub fn agent_output_budget(project_id: ProjectId) -> BudgetSpec {
    BudgetSpec {
        budget_chars: RESULT_BUDGET_CHARS,
        droppable_list: None,
        read_with: Some("read_send_output"),
        pointer: Box::new(move |site: &CutSite<'_>| {
            let output = site.ancestors.last()?;
            let send_id = output.get("send_id")?.as_str()?;
            // Read on from the copy the text came from (see `ReadFrom`).
            let read_from = match output.get("source").and_then(serde_json::Value::as_str) {
                Some("live") => ReadFrom::Auto,
                Some("journal") => return None,
                _ => ReadFrom::Transcript,
            };
            Some(serde_json::json!({
                "tool": "read_send_output",
                "project_id": project_id,
                "send_id": send_id,
                "agent_id": output.get("agent_id")?.clone(),
                "offset": site.kept_chars,
                "read_from": read_from,
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
