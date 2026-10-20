//! The read tier: lists, the conversation, replies, and saved prompts. Opens
//! nothing and locks nothing, so every function here keeps working for a
//! project the window has not loaded, when the window is slow, and when the
//! working directory is gone.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use switchboard_core::{AgentId, AgentRecord, Project, ProjectId, SendId};
use switchboard_harness::{SystemMarker, Turn, TurnOutcome, TurnStatus, turn_answer_text};
use switchboard_prompts::{PromptError, ProviderStatus};
use tokio_util::sync::CancellationToken;

use crate::commands::{
    AgentJournalView, ConversationItem, OutcomeStatus, ProjectConversation, ProjectListing,
    SavedPromptResolution, SendCorrelation, TurnRender, list_project_agents_readonly_impl,
    list_projects_impl, load_agent_transcript, load_project_conversation_impl,
    open_project_from_store, resolve_saved_prompt_impl,
};
use crate::error::AppError;
use crate::state::{AppState, lock};
use crate::workflow_commands::{WorkflowListing, list_workflows_impl};

use super::budget::{BudgetSpec, CutSite, json_escaped_len};
use super::context::{CONTEXT_READ_WAIT, ContextFigures, ContextState, ContextStore};
use super::ledger::AgentActivity;
use super::ledger::SendStatus;
use super::wait::{Outcome, WaitBudget};
use super::with_cap;

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

/// Every project in the store with whether it is busy, leaving out the ones
/// the user archived unless `include_archived`: the window's project list shows
/// only active projects until the user opens its archived view, and an
/// orchestrator choosing where to work should see the same set. A project
/// nobody has opened has no registered agents and no run, so it is idle. Each
/// agent's busy check asks its actor, which answers nothing while it prepares
/// a turn, so the checks end on `cancel` (the server's stop) with
/// `Interrupted`.
pub async fn list_projects(
    state: &AppState,
    include_archived: bool,
    cancel: &CancellationToken,
) -> Result<Vec<ProjectStatusListing>, AppError> {
    let listings = list_projects_impl(state)?;
    let mut out = Vec::with_capacity(listings.len());
    for listing in listings
        .into_iter()
        .filter(|listing| include_archived || !listing.archived)
    {
        let status = if project_is_busy(state, listing.id, cancel).await? {
            ProjectStatus::Busy
        } else {
            ProjectStatus::Idle
        };
        out.push(ProjectStatusListing { listing, status });
    }
    Ok(out)
}

async fn project_is_busy(
    state: &AppState,
    project_id: ProjectId,
    cancel: &CancellationToken,
) -> Result<bool, AppError> {
    if lock(&state.workflow_runs)
        .values()
        .any(|run| run.project_id == project_id)
    {
        return Ok(true);
    }
    let agents: Vec<AgentId> = lock(&state.agents_by_id)
        .values()
        .filter(|a| a.project_id == project_id)
        .map(|a| a.id)
        .collect();
    for agent_id in agents {
        let pending = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(AppError::Interrupted),
            pending = state.dispatcher.has_pending_work(agent_id) => pending,
        };
        if pending {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentListing {
    #[serde(flatten)]
    pub record: AgentRecord,
    /// Whether a send to the agent would be refused right now (an
    /// observation; another caller can occupy it before the next send).
    pub busy: bool,
    /// The agent's context fullness from its newest finished turn, or a newer
    /// compaction, as the sidebar's bar shows it. `None` while unknown, when
    /// that turn's figures are unusable, or when the harness reports no window.
    pub context: Option<ContextFigures>,
    /// The agent's session file is still being read for `context`; call again.
    pub context_pending: bool,
    /// Whether the agent is producing anything: seconds since its last output,
    /// its open tool calls, and its open background tasks. `None` before any
    /// output since the app started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<AgentActivity>,
}

/// The project's roster with each agent's model, effort, quick choices,
/// whether it would accept a send now, and its context fullness. An agent
/// nothing has parsed since launch has its file read once, here, waiting
/// [`CONTEXT_READ_WAIT`] for it; a read still running is reported as pending
/// and finishes on its own. `cancel` (the server's stop) ends the wait and the
/// busy checks after it with `Interrupted`, since an actor preparing a turn
/// answers no check until it is ready.
pub async fn list_agents(
    state: &AppState,
    project_id: ProjectId,
    home_dir: &Path,
    cancel: &CancellationToken,
) -> Result<Vec<AgentListing>, AppError> {
    let records = list_project_agents_readonly_impl(state, project_id)?;
    let mut reads = Vec::new();
    for record in &records {
        if state.context.lookup(record.id) != ContextState::Unknown {
            continue;
        }
        let project = project_readonly(state, project_id)?;
        let agent = record.clone();
        let home_dir = home_dir.to_path_buf();
        reads.push(state.context.ensure_reading(record.id, async move {
            tokio::task::spawn_blocking(move || load_agent_transcript(&project, &agent, &home_dir))
                .await
                .map_err(|e| AppError::Internal(format!("transcript load task failed: {e}")))?
        }));
    }
    if !reads.is_empty() {
        let finished = futures::future::join_all(reads.into_iter().map(|mut done| async move {
            // The guard the wait returns is dropped here, so nothing
            // non-`Send` is held across the join.
            let _ = done.wait_for(|done| *done).await;
        }));
        tokio::select! {
            () = cancel.cancelled() => {}
            _ = tokio::time::timeout(CONTEXT_READ_WAIT, finished) => {}
        }
    }
    let mut out = Vec::with_capacity(records.len());
    for record in records {
        let accepts = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(AppError::Interrupted),
            accepts = state.dispatcher.accepts_now(record.id) => accepts,
        };
        let busy = !accepts;
        let (context, context_pending) = match state.context.lookup(record.id) {
            ContextState::Known(figures) => (figures, false),
            ContextState::Unknown => (None, true),
        };
        out.push(AgentListing {
            activity: state.ledger.activity(record.id),
            record,
            busy,
            context,
            context_pending,
        });
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
    /// One agent's turn. With a `send_id`, `read_reply` with
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
    let open_before = state.ledger.open_turns();
    let conversation = load_project_conversation_impl(state, project_id, home_dir).await?;
    let running = running_sends(open_before.into_iter().chain(state.ledger.open_turns()));
    let roster = list_project_agents_readonly_impl(state, project_id)?;
    let mut digest = digest(conversation, &roster, &running);
    if let Some(n) = last_n_items
        && digest.items.len() > n
    {
        let cut = digest.items.len() - n;
        digest.items.drain(..cut);
    }
    Ok(digest)
}

/// The sends whose turns are running, as `(send, agent)`, from the ledger's
/// open turns, read before the session files are read and again after.
///
/// **Why the ledger decides, not the file.** A session file read while its
/// turn runs holds part of that turn, and the harness's parser can only guess
/// whether the turn has ended: Codex and Antigravity close an unfinished last
/// turn as failed, and Claude writes a turn's first cycle, end-of-turn marker
/// included, while the turn waits on background work, so it reads as
/// complete. The ledger has seen the turn start and not end, for every turn
/// on the agent's channel, the window's sends included. It is asked rather
/// than the agent's actor, which cannot answer while it prepares a turn.
///
/// **The reading before does the work.** A turn that ends during the read was
/// running when its file was read, so its copy may be partial, and only the
/// reading taken first still holds it open. The reading after helps only in a
/// window of microseconds: the journal is read before the session files, so a
/// turn that starts during the read is paired with its send only when the
/// actor journaled the send just before that journal read.
fn running_sends(open_turns: impl IntoIterator<Item = (AgentId, SendId)>) -> HashSet<SendTo> {
    open_turns
        .into_iter()
        .map(|(agent_id, send_id)| (send_id, agent_id))
        .collect()
}

/// Whether `send_id` was the send of one agent's open turn at either of two
/// readings, taken around a session-file read (see [`running_sends`]).
fn answering(readings: [Option<SendId>; 2], send_id: SendId) -> bool {
    readings.contains(&Some(send_id))
}

/// A running turn is reported `running`, with no reason, whatever its partial
/// copy on disk says ([`running_sends`]), and so is every later turn of the
/// same agent: a compaction part-way through a turn splits it on disk, and
/// only the first piece carries the send. A journaled failure or cancellation
/// wins over the ledger's reading, because it proves the turn ended: one that
/// ends after the reading is reported as it ended, as `read_reply` reports it.
fn digest(
    conversation: ProjectConversation,
    roster: &[AgentRecord],
    running: &HashSet<SendTo>,
) -> ConversationDigest {
    let (outcomes, answered) = journal_outcomes(&conversation.items);
    let mut still_answering: HashSet<AgentId> = HashSet::new();
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
                if outcome.is_none() && send_id.is_some_and(|s| running.contains(&(s, agent_id))) {
                    still_answering.insert(agent_id);
                }
                let (status, reason, failure_kind) =
                    if outcome.is_none() && still_answering.contains(&agent_id) {
                        (DigestTurnStatus::Running, None, None)
                    } else {
                        turn_status(status, outcome)
                    };
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

/// Which copy of a reply [`read_reply`] pages.
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
    /// `read_latest_reply` return, and so where their cuts read on.
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
/// one. The page holds up to `page_chars` of text; the full budget's page is
/// [`super::OUTPUT_PAGE_CHARS`].
#[allow(clippy::too_many_arguments)] // One reader: the ids, the position, the copy, and the size.
pub async fn read_reply(
    state: &AppState,
    project_id: ProjectId,
    send_id: SendId,
    agent_id: AgentId,
    offset: usize,
    read_from: ReadFrom,
    page_chars: usize,
    home_dir: &Path,
) -> Result<OutputPage, AppError> {
    let open_before = state.ledger.open_turn(agent_id);
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
                    page_chars,
                    OutputSource::Live,
                ));
            }
            Some(recipient.status)
        }
        _ => None,
    };
    let agent = agent_in_project(state, project_id, agent_id)?;
    let (view, correlated) = correlate_agent(state, project_id, &agent, home_dir).await?;
    let running = answering(
        [
            open_before.flatten(),
            state.ledger.open_turn(agent_id).flatten(),
        ],
        send_id,
    );
    let turn = correlated
        .iter()
        .rev()
        .find(|turn| turn.send_id == Some(send_id));
    let journaled = view.sends.iter().any(|(s, _)| *s == send_id);
    // The ledger's status is the most current when it holds the send, and its
    // open turn says the send is running when it does not ([`running_sends`]).
    // Otherwise the journal's recorded outcome wins over the transcript, as it
    // does for `wait_send` after a restart: a failed or cancelled send with
    // partial content on disk is not a completed answer, and one with no reply
    // at all still pages (empty) rather than reading as unknown.
    let status = match (live_status, view.outcomes.get(&send_id), turn) {
        (Some(status), _, _) => status,
        (None, None, _) if running && journaled => SendStatus::Running,
        (None, Some((OutcomeStatus::Cancelled, _)), _) => SendStatus::Cancelled,
        (None, Some((OutcomeStatus::Failed, _)), _) => SendStatus::Failed,
        (None, None, Some(turn)) => match turn.status {
            TurnStatus::Complete => SendStatus::Completed,
            TurnStatus::Streaming => SendStatus::Running,
            TurnStatus::Failed => SendStatus::Failed,
        },
        (None, None, None) if journaled => SendStatus::Started,
        (None, None, None) => return Err(AppError::SendUnknown(send_id)),
    };
    let source = match (turn.and_then(|t| t.correlation), live_status) {
        (Some(SendCorrelation::DurableLink), _) => OutputSource::DurableLink,
        (Some(SendCorrelation::Positional), _) => OutputSource::Positional,
        (_, Some(_)) => OutputSource::Live,
        _ => OutputSource::Journal,
    };
    let text = turn.map_or("", |t| t.text.as_str());
    Ok(page(
        send_id, agent_id, status, text, offset, page_chars, source,
    ))
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
/// here pages the same reply through `read_reply`.
pub(crate) async fn correlate_turns(
    project: &Project,
    agent: &AgentRecord,
    view: &AgentJournalView,
    home_dir: &Path,
    context: &ContextStore,
) -> Result<Vec<CorrelatedTurn>, AppError> {
    let transcript = {
        let project = project.clone();
        let agent = agent.clone();
        let home_dir = home_dir.to_path_buf();
        context
            .parse_recording(agent.id, async move {
                tokio::task::spawn_blocking(move || {
                    load_agent_transcript(&project, &agent, &home_dir)
                })
                .await
                .map_err(|e| AppError::Internal(format!("transcript load task failed: {e}")))?
            })
            .await?
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
    let turns = correlate_turns(&project, agent, &view, home_dir, &state.context).await?;
    Ok((view, turns))
}

fn page(
    send_id: SendId,
    agent_id: AgentId,
    status: SendStatus,
    text: &str,
    offset: usize,
    page_chars: usize,
    source: OutputSource,
) -> OutputPage {
    let total_chars = text.chars().count();
    let mut budget = page_chars;
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
pub enum LatestReplyStatus {
    /// `text` is the latest completed answer.
    Completed,
    /// The latest turn failed or was cancelled; `text` is the note the forward
    /// resolver would send in its place.
    Failed,
    Cancelled,
    /// The agent has no completed turn other than one still running.
    None,
}

/// An agent's latest reply, including turns the caller did not send.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LatestReply {
    pub agent_id: AgentId,
    pub status: LatestReplyStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The send the latest turn answered, when known, so a caller pages with
    /// `read_reply`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_id: Option<SendId>,
    pub source: OutputSource,
    /// The agent's context fullness after its newest finished turn, as
    /// `list_agents` reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextFigures>,
    /// The agent has a turn running, or a send made through the agent tools
    /// or by a workflow is still to reach it, so this answer is not its
    /// newest. `false` does not
    /// prove the answer current: a message the user typed may be starting. A
    /// caller following its own send waits on that send instead.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub newer_turn_running: bool,
}

/// The agent's context fullness after its newest finished turn, when known.
pub(crate) fn known_context(state: &AppState, agent_id: AgentId) -> Option<ContextFigures> {
    match state.context.lookup(agent_id) {
        ContextState::Known(figures) => figures,
        ContextState::Unknown => None,
    }
}

/// The agent's latest completed answer: the ledger's text when the agent's
/// newest turn is one the ledger holds and it completed; otherwise the
/// journal's typed outcome for a latest turn that failed or was cancelled
/// (with the note the forward resolver would send in its place); otherwise
/// the newest completed turn in the agent's own transcript, correlated to its
/// send with the merge's per-agent classifier. A turn still running is left
/// out, whoever sent it, and the answer is the one the agent would have had
/// it not started: Claude writes a turn's first cycle, end-of-turn marker
/// included, while the turn waits on background work, so the transcript can
/// hold a running turn that looks complete. A `send_id` is reported only
/// when that classifier assigns one: guessing from the last journaled send
/// would name a newer turn that is still running. Carries the agent's context
/// after its newest finished turn.
pub async fn read_latest_reply(
    state: &AppState,
    project_id: ProjectId,
    agent_id: AgentId,
    home_dir: &Path,
) -> Result<LatestReply, AppError> {
    // The same readings of the running turn serve both the skip and the flag:
    // the skip leaves out a send open at the reading before the transcript is
    // read or the one after it ([`running_sends`]), and the flag is set when
    // either found a turn open, so a turn ending or starting mid-read never
    // leaves an older answer marked current. It can leave the flag set beside
    // the answer before it, or beside its own when the ledger already holds
    // it: stale, which costs the caller one more read. Taken from what the
    // app already records, never by asking the agent's actor, which cannot
    // answer while it prepares a turn.
    let open_before = state.ledger.open_turn(agent_id);
    let mut output =
        latest_reply_alone(state, project_id, agent_id, open_before.flatten(), home_dir).await?;
    output.context = known_context(state, agent_id);
    output.newer_turn_running |=
        open_before.is_some() || state.ledger.has_unfinished_send_to(agent_id);
    Ok(output)
}

/// [`read_latest_reply`]'s answer, leaving out the turn answering `running`
/// (the open turn's send before the transcript is read) or the send open at a
/// second reading taken once it is read, and every later turn of the agent.
/// `newer_turn_running` is whether that second reading found a turn open.
async fn latest_reply_alone(
    state: &AppState,
    project_id: ProjectId,
    agent_id: AgentId,
    running: Option<SendId>,
    home_dir: &Path,
) -> Result<LatestReply, AppError> {
    let agent = agent_in_project(state, project_id, agent_id)?;
    if let Some((send_id, latest)) = state.ledger.latest_turn(agent_id)
        && latest.status == SendStatus::Completed
    {
        return Ok(LatestReply {
            agent_id,
            status: LatestReplyStatus::Completed,
            text: latest.text,
            send_id: Some(send_id),
            source: OutputSource::Live,
            context: None,
            newer_turn_running: false,
        });
    }
    let project = project_readonly(state, project_id)?;
    let records = read_journal(&project).await?;
    // The view pairs the transcript's turns with every send, the running one
    // included. The outcome check reads the sends without it, so a turn
    // cancelled or failed just before it is reported as such, not read from
    // disk.
    let view = AgentJournalView::from_records(&records, agent_id);
    let mut without_running = records;
    without_running.retain(|record| {
        !matches!(
            record,
            switchboard_core::JournalRecord::Send { send_id, agent_id: a, .. }
                if *a == agent_id && Some(*send_id) == running
        )
    });
    if let Some((outcome, note)) =
        crate::commands::latest_turn_outcome(&without_running, agent_id, &agent.name)
    {
        let latest_send = without_running
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
            Some(TurnOutcome::Cancelled { .. }) => LatestReplyStatus::Cancelled,
            _ => LatestReplyStatus::Failed,
        };
        return Ok(LatestReply {
            agent_id,
            status,
            text: Some(note),
            send_id: latest_send,
            source: OutputSource::Journal,
            context: None,
            newer_turn_running: false,
        });
    }
    let mut correlated = correlate_turns(&project, &agent, &view, home_dir, &state.context).await?;
    let open_after = state.ledger.open_turn(agent_id);
    // The running turn and every later turn of the agent are left out: a
    // compaction part-way through a turn splits it on disk, and only the first
    // piece carries the send. A compaction journals nothing, so no transcript
    // turn pairs with its send: while one runs, nothing is left out.
    if let Some(first) = correlated.iter().position(|turn| {
        turn.send_id
            .is_some_and(|send_id| answering([running, open_after.flatten()], send_id))
    }) {
        correlated.truncate(first);
    }
    let latest = correlated
        .into_iter()
        .rev()
        .find(|turn| turn.status == TurnStatus::Complete);
    let newer_turn_running = open_after.is_some();
    Ok(match latest {
        Some(turn) => LatestReply {
            agent_id,
            status: LatestReplyStatus::Completed,
            text: Some(turn.text),
            send_id: turn.send_id,
            source: match turn.correlation {
                Some(SendCorrelation::DurableLink) => OutputSource::DurableLink,
                Some(SendCorrelation::Positional) => OutputSource::Positional,
                _ => OutputSource::Uncorrelated,
            },
            context: None,
            newer_turn_running,
        },
        None => LatestReply {
            agent_id,
            status: LatestReplyStatus::None,
            text: None,
            send_id: None,
            source: OutputSource::Uncorrelated,
            context: None,
            newer_turn_running,
        },
    })
}

/// The budget rules for a [`ConversationDigest`]: message and answer text may
/// be cut; the oldest items are dropped when the structure alone is over; an
/// agent turn's text reads on through `read_reply` from the session
/// file's copy, from where it was cut, when the turn has a send; a user
/// message's cannot be read on.
#[must_use]
pub fn conversation_budget(project_id: ProjectId, budget_chars: usize) -> BudgetSpec {
    BudgetSpec {
        budget_chars,
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
            Some(with_cap(
                serde_json::json!({
                    "tool": "read_reply",
                    "project_id": project_id,
                    "send_id": send_id,
                    "agent_id": item.get("agent_id")?.clone(),
                    "offset": site.kept_chars,
                    "read_from": ReadFrom::Transcript,
                }),
                budget_chars,
            ))
        }),
    }
}

/// The budget rules for a [`LatestReply`]: a reply may be cut and reads on
/// through `read_reply`, from the copy its text came from, when the
/// output has a send. A journal note is not a reply and has no read-on.
#[must_use]
pub fn latest_reply_budget(project_id: ProjectId, budget_chars: usize) -> BudgetSpec {
    BudgetSpec {
        budget_chars,
        droppable_list: None,
        read_with: Some("read_reply"),
        pointer: Box::new(move |site: &CutSite<'_>| {
            let output = site.ancestors.last()?;
            let send_id = output.get("send_id")?.as_str()?;
            // Read on from the copy the text came from (see `ReadFrom`).
            let read_from = match output.get("source").and_then(serde_json::Value::as_str) {
                Some("live") => ReadFrom::Auto,
                Some("journal") => return None,
                _ => ReadFrom::Transcript,
            };
            Some(with_cap(
                serde_json::json!({
                    "tool": "read_reply",
                    "project_id": project_id,
                    "send_id": send_id,
                    "agent_id": output.get("agent_id")?.clone(),
                    "offset": site.kept_chars,
                    "read_from": read_from,
                }),
                budget_chars,
            ))
        }),
    }
}

#[must_use]
pub fn list_prompts(state: &AppState) -> Vec<switchboard_prompts::Prompt> {
    crate::commands::list_prompts_impl(state)
}

/// What a prompt server says in place of a template it does not give out.
pub const SERVER_TEMPLATE_NOTE: &str = "This prompt comes from a prompt server, which keeps its \
template on the server and gives out only finished text, so there is no template to show. Pass \
args (use {} for a prompt with no arguments) to get the text a send of this prompt would \
deliver.";

/// A saved prompt's text: a template, or a rendering, or both.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PromptText {
    /// The only field the result budget cuts (see [`super::budget`]).
    pub text: String,
}

/// Said in place of a description and arguments Switchboard has no listing
/// for: a prompt server's prompt whose last fetch did not list it.
pub const UNLISTED_NOTE: &str = "Switchboard has no listing of this prompt from its server \
right now (it may need sign-in, have been unreachable at the last fetch, or not have been \
fetched yet), so its description and arguments are unknown here; the rendering is current.";

/// A saved prompt as `read_prompt` returns it: what `list_prompts` lists for
/// it, its template, and on request its rendering.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PromptReading {
    pub provider: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The declared arguments; `None` when Switchboard has no listing of the
    /// prompt, so it cannot read as "takes no arguments".
    pub arguments: Option<Vec<switchboard_prompts::PromptArgument>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The template with its placeholders intact, for a built-in or local
    /// prompt; `None` for a prompt server's.
    pub template: Option<PromptText>,
    /// Why there is no template, when the prompt comes from a prompt server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_note: Option<&'static str>,
    /// Why the description and arguments are missing, when the prompt
    /// rendered but Switchboard has no listing of it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listing_note: Option<&'static str>,
    /// The text a send of this prompt with the given args would deliver.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rendered: Option<PromptText>,
}

/// One saved prompt: its metadata as `list_prompts` reports it, its template,
/// and with `args`, its rendering.
///
/// **Why an orchestrator may read a template.** The user's prompts are their
/// standing instructions, and an orchestrator choosing between them, or
/// deciding what to append, needs to know what one says before sending it,
/// as the user does from the window's prompt preview. The template is shown
/// to the caller, never sent to an agent: a send still delivers the rendering.
///
/// **A prompt server gives out no template.** MCP's `prompts/get` returns
/// finished text, so a prompt server's prompt has metadata and, with `args`,
/// a rendering, which is exactly what a send would deliver. The result says
/// so in [`SERVER_TEMPLATE_NOTE`] rather than leaving `template` null
/// unexplained.
///
/// **With `args`, it renders as a send does.** A send renders without
/// consulting Switchboard's cached copy of each server's prompt list, so this
/// does too: the same render, argument checks, and errors, bounded by
/// `budget` because a server's render is a network call. The cache supplies
/// the description and arguments when it has them; when it does not (the
/// server needed sign-in or was unreachable at its last fetch, or has not
/// been fetched yet) they are unknown and [`UNLISTED_NOTE`] says so.
///
/// **Without `args`, it reads the cache and nothing else.** The cache is the
/// only source of a server prompt's metadata, and refreshing it means
/// refetching every configured server and reloading the window's prompt list,
/// which one read should not cost. A miss is classified as the window
/// classifies a saved prompt ([`resolve_saved_prompt_impl`]): missing only
/// when the server's last fetch succeeded without it; otherwise the server's
/// state at that fetch, such as needing sign-in.
pub async fn read_prompt(
    state: &AppState,
    provider: &str,
    name: &str,
    args: Option<&BTreeMap<String, String>>,
    budget: &WaitBudget,
) -> Result<PromptReading, AppError> {
    let listed = match resolve_saved_prompt_impl(state, provider, name) {
        SavedPromptResolution::Available { prompt, .. } => Some(prompt),
        _ if args.is_some() => None,
        SavedPromptResolution::NotConfigured { .. } => {
            return Err(PromptError::ProviderNotFound {
                provider: provider.to_owned(),
            }
            .into());
        }
        SavedPromptResolution::TemporarilyUnavailable { .. } => {
            return Err(unlisted_provider_error(state, provider));
        }
        SavedPromptResolution::ConfirmedMissing { .. } => {
            return Err(PromptError::PromptNotFound {
                provider: provider.to_owned(),
                name: name.to_owned(),
            }
            .into());
        }
    };
    let rendered = match args {
        None => None,
        Some(args) => match budget.run(state.prompts.render(provider, name, args)).await {
            Outcome::Done(rendered) => Some(PromptText {
                text: rendered?.text,
            }),
            Outcome::Timeout => {
                return Err(AppError::TimedOut {
                    what: "rendering the prompt",
                });
            }
            Outcome::Cancelled => return Err(AppError::Interrupted),
        },
    };
    let template = state
        .prompts
        .source(provider, name)
        .map(|source| PromptText { text: source.text });
    let from_server = ![
        switchboard_prompts::BUILTIN_PROVIDER,
        switchboard_prompts::LOCAL_PROVIDER,
    ]
    .contains(&provider);
    let listing_note = listed.is_none().then_some(UNLISTED_NOTE);
    let (title, description, arguments, tags) = match listed {
        Some(prompt) => (
            prompt.title,
            prompt.description,
            Some(prompt.arguments),
            prompt.tags,
        ),
        None => (None, None, None, Vec::new()),
    };
    Ok(PromptReading {
        provider: provider.to_owned(),
        name: name.to_owned(),
        title,
        description,
        arguments,
        tags,
        template_note: (template.is_none() && from_server).then_some(SERVER_TEMPLATE_NOTE),
        template,
        listing_note,
        rendered,
    })
}

/// Why a prompt server's prompt is not in the cache, from the server's state
/// at its last fetch: the sign-in error when it needed sign-in, so the caller
/// reports that rather than a missing prompt.
fn unlisted_provider_error(state: &AppState, provider: &str) -> AppError {
    match state
        .prompts
        .resolution_snapshot()
        .provider_status(provider)
    {
        Some(ProviderStatus::NeedsAuth) => PromptError::McpNeedsAuth {
            provider: provider.to_owned(),
        }
        .into(),
        Some(ProviderStatus::Errored { message }) => AppError::Internal(format!(
            "prompt provider {provider} failed at Switchboard's last fetch of its prompts \
             ({message}), so the prompt's description and arguments are unknown: pass args to \
             render it anyway"
        )),
        Some(ProviderStatus::StoreUnavailable) => AppError::Internal(format!(
            "Switchboard could not read the stored sign-in for prompt provider {provider} at \
             its last fetch (the user may need to unlock the keychain at the Mac): pass args to \
             render the prompt anyway"
        )),
        _ => AppError::Internal(format!(
            "Switchboard has not listed prompt provider {provider}'s prompts yet: pass args to \
             render the prompt now, or call again in a moment"
        )),
    }
}

/// The budget rules for a [`PromptReading`]: the template and the rendering
/// are cut evenly to fit, each cut reported, and nothing reads on, because a
/// saved prompt is a few thousand characters and one over the result budget
/// is not worth a pager. Nothing else is cut.
#[must_use]
pub fn prompt_budget(budget_chars: usize) -> BudgetSpec {
    BudgetSpec::whole(budget_chars)
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
        .ok_or(AppError::AgentNotInProject {
            project_id,
            agent_id,
        })
}
