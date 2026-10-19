//! The send ledger: per-send, per-recipient status and reply text, plus
//! per-run status, fed by the event stream.
//!
//! **Why the event stream.** The dispatcher's completion handle exists only on
//! the fail-fast awaitable path, so a queued send has nothing to await, and
//! the handle is consumed by one caller. The per-agent event stream already
//! carries everything the window uses to show completion (`turn_start`,
//! content, `turn_end`, `message_failed`, `message_cancelled`) for every kind
//! of send, queued or not. Reading it once, in process, gives one source of
//! truth for every caller that is not the window. [`OpsLedgerEmitter`] is the
//! same shape as the wake-lock decorator: it wraps the base emitter where
//! `AppState.emitter` is built, observes, and forwards unchanged. It also
//! feeds every turn end's usage to [`super::context`], the read tier's record
//! of each agent's context fullness.
//!
//! **Correlation.** `ops`, and the workflow engine through its dispatch
//! provider, choose each send's `message_id` and register it here *before*
//! dispatching, so the first event the actor emits for it lands on an entry
//! that already exists: no orphan map, no fallback matching. `turn_start`,
//! `message_failed`, and `message_cancelled` carry `message_id`; `turn_end`
//! and content carry the `turn_id` that `turn_start` announced. Events for
//! message ids the ledger never registered (the window's compose sends) are
//! ignored, except that every agent's latest `turn_start` is noted so a
//! reader can tell whether the agent's newest turn is one the ledger holds. A
//! compaction's `turn_start` is the exception: a compaction is not an answer,
//! so the agent's newest answer stays readable through it.
//!
//! **Workflow sends** are held like the agent tools' own, whoever started the
//! run, so `wait_send`, `read_reply`, and `cancel_send` follow a step's
//! send while it runs rather than reading it back from the journal as if the
//! app had restarted. The app's workflow provider
//! (`ProjectDispatchFactoryProvider`) registers the step's send before
//! building its message, every recipient `not_dispatched`; marks each
//! recipient accepted, minting its `message_id`, just before offering it;
//! records a busy agent as rejected; and settles the send when the step's
//! dispatch ends, refusing the recipients it never reached with
//! `workflow_step_failed` when the step could not get its message ready. A
//! workflow send has no idempotency key. A cancel `cancel_send` records, of
//! the send or of any one of its replies (which ends the run), stops the
//! step offering the message to anyone else and ends the run cancelled, even
//! when the dispatcher's answer was that the agent was busy or a later
//! recipient's pre-dispatch check refused: the engine decides once, when the
//! step stops, and tells the provider, so the recipients it never reached read
//! `cancelled`.
//!
//! **Text.** What counts as an agent's answer is [`TextCapture`]'s rule,
//! observed over the wire events; it is the same rule the dispatcher applies
//! to the adapter events, so a forward and a ledger read agree byte for byte.
//! Captured text is kept whole in memory and budgeted only on the way out.
//!
//! **Workflow runs** are observed on `workflow:<project_id>`, where the
//! progress sink emits. Needed because a completed or cancelled run's file is
//! deleted at its end, so without this a caller that looked away could never
//! learn how it ended.
//!
//! **Bounds.** The 500 most recent **settled** sends and terminal runs: a send
//! whose dispatch is unfinished, or with a recipient still running, is never
//! pruned by count, because dropping it would let a retry with its key send
//! again and would hide it from `cancel_send`. Everything, settled or not, is
//! dropped after 24 hours, the backstop against a terminal event that never
//! arrives. Queued work is therefore unbounded in memory, as it is in the
//! dispatcher.
//!
//! **Parsing.** Events are read in their typed forms (`NormalizedEvent`, the
//! progress payload), so a renamed field breaks the build rather than failing
//! silently. An unknown run status is ignored and logged (the run may still be
//! going); an unknown turn outcome ends the turn as `failed`, because a turn
//! end is always terminal and leaving the recipient `running` would make a wait
//! never finish. Per-agent waiters are woken only by the events that can change
//! admission, never by content chunks.
//!
//! **Compactions.** A compaction another client asks for is a one-recipient
//! entry of kind [`SendKind::Compaction`], so `wait_send`, `cancel_send`, and
//! the per-agent wake-ups cover it. Because a compaction always queues, one
//! rule stops a looping client piling them up, decided under the ledger lock
//! in [`SendLedger::claim_compaction`], in this order. A key already claimed
//! joins its claim. A compaction for the agent whose dispatch has not settled
//! is joined, whatever the key, and receives what its first caller receives:
//! until `settle`, its dispatch can still fail and release it, and the release
//! must reach every waiter, so no caller is handed an id the ledger then
//! drops. A compaction whose dispatch has settled (it is queued or running in
//! the dispatcher, not yet finished) is reused, returning its receipt, while
//! it is the agent's newest unfinished entry by the time it was admitted to
//! the agent's queue and was not asked to cancel. Otherwise a fresh entry is
//! created under the same lock, so a concurrent request finds it. Admission
//! time, not registration time: an entry is registered at its claim and
//! admitted at dispatch, after preparation, so a message registered first can
//! be admitted after a compaction registered later. The ledger holds only
//! what orchestrators and workflows queued, so work the window's compose bar
//! queued is not seen; nothing is lost or duplicated either way.
//!
//! **Idempotency.** A caller's `idempotency_key`, scoped to the project, is
//! claimed here under the ledger lock together with a hash of the request. A
//! concurrent or later repeat with the same hash waits for and returns the
//! original receipt; a different hash is refused; a request that failed before
//! dispatching anything releases the key so the same request can be retried.
//!
//! **Forks.** A `fork_agent` request is a one-recipient message whose
//! recipient, the fork, does not exist when the request is claimed, so its
//! entry starts with no recipient and gains one when the fork is created
//! ([`SendLedger::attach_fork`]). From then on the key is kept whatever
//! happens to the message, because a released key would let a retry make a
//! second fork: a message refused before it dispatched is recorded on the
//! recipient as rejected and the entry settled, and a repeat under the key
//! resumes with a fresh entry for the same fork ([`SendLedger::claim_fork`]);
//! a message that dispatched or was cancelled is final, as a send's is. A key
//! naming a fork outlives its entries, up to the age bound, since a rejected
//! attempt's entry is finished and would be dropped first.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use switchboard_core::{AgentId, AgentRecord, ProjectId, SendId};
use switchboard_dispatcher::EventEmitter;
use switchboard_harness::{
    FailureKind, MessageFailureKind, MessageId, NormalizedEvent, TextCapture, TurnId, TurnOutcome,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::lock;
use crate::workflow::ResolvedRecipient;
use crate::workflow_commands::{WorkflowProgressPayload, WorkflowProgressStatus};

use super::error_code;

/// How many sends, and how many runs, the ledger keeps.
pub const MAX_ENTRIES: usize = 500;
/// How long an entry is kept.
pub const MAX_AGE: Duration = Duration::hours(24);

/// A recipient's state within a send, as reported to callers.
///
/// `Accepted` through `NotDispatched` come from the ledger; `Started` and
/// `Unknown` only from the journal fallback a wait uses for a send the ledger
/// does not hold (after a restart, or once pruned).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SendStatus {
    /// The dispatcher took the send; its turn has not started.
    Accepted,
    Running,
    Completed,
    Failed,
    Cancelled,
    /// The dispatcher refused it (busy under `refuse`, or a per-recipient
    /// check failed); nothing ran.
    Rejected,
    /// An earlier recipient was refused, so this one was never offered.
    NotDispatched,
    /// The journal holds the send's start but no durable end.
    Started,
    /// Neither the ledger nor the journal knows this send.
    Unknown,
}

impl SendStatus {
    /// Whether no further change can come from the event stream.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Accepted | Self::Running)
    }
}

/// One recipient of a ledgered send, as a caller sees it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RecipientSnapshot {
    pub agent_id: AgentId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    pub status: SendStatus,
    /// The reply text once `Completed`; `None` before and for every other
    /// terminal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The wire code for a rejection or a typed failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
    /// Why a failed turn failed, as the harness classified it, so a caller can
    /// tell a quota wall or an auth failure from a crash and not retry into it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<FailureKind>,
    /// What the agent is doing while this recipient's own turn runs: how long
    /// since it last produced output, and the tool calls it has open. Absent
    /// while the recipient waits in line behind another turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<AgentActivity>,
    /// The end of what the agent has written in this turn so far, up to
    /// [`RECENT_TEXT_CHARS`] characters, while it runs: where it has got to,
    /// without a separate read. Answer text only, as the reply will hold it;
    /// tool output and thinking are left out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_text: Option<String>,
    /// The agent's context fullness after its newest finished turn, on a
    /// completed recipient in a finished wait: what decides whether to compact
    /// before the next step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<super::context::ContextFigures>,
}

impl RecipientSnapshot {
    /// A recipient known only by its status: nothing it wrote, no error.
    #[must_use]
    pub fn bare(agent_id: AgentId, status: SendStatus) -> Self {
        Self {
            agent_id,
            message_id: None,
            status,
            text: None,
            error: None,
            code: None,
            failure_kind: None,
            activity: None,
            recent_text: None,
            context: None,
        }
    }
}

/// How much of a running turn's text a send's snapshot carries.
pub const RECENT_TEXT_CHARS: usize = 500;

/// What an agent's running turn is doing, for telling a stuck turn from a
/// long one. Silence is measured as the window measures it for its "No
/// response" label: the time since the turn started or the agent last wrote
/// text, sent a heartbeat, or started or finished a tool call. A tool call
/// left open (a test suite running) is reported with how long it has run,
/// since it produces nothing until it ends. Reported only while a turn runs:
/// an idle agent has nothing to be stuck on.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentActivity {
    pub seconds_since_output: u64,
    /// Oldest first.
    pub open_tool_calls: Vec<OpenToolCall>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OpenToolCall {
    pub name: String,
    pub running_seconds: u64,
}

/// An agent's last output and its open tool calls, as the ledger tracks them.
struct ActivityState {
    last_output: Instant,
    open_tools: Vec<OpenTool>,
    /// A turn has started and not ended.
    turn_open: bool,
    /// The send the open turn answers, from its start event, for any turn:
    /// one the user typed in the window included. Kept beside `turn_open`
    /// rather than in its place, so a start event whose send cannot be read
    /// still reads as a running turn.
    open_send: Option<SendId>,
}

struct OpenTool {
    tool_use_id: String,
    name: String,
    started: Instant,
}

impl ActivityState {
    fn report(&self, now: Instant) -> AgentActivity {
        AgentActivity {
            seconds_since_output: now.saturating_duration_since(self.last_output).as_secs(),
            open_tool_calls: self
                .open_tools
                .iter()
                .map(|tool| OpenToolCall {
                    name: tool.name.clone(),
                    running_seconds: now.saturating_duration_since(tool.started).as_secs(),
                })
                .collect(),
        }
    }
}

/// The events that restart an agent's silence: a turn starting, which arms
/// the window's "No response" timer, and the ones that clear that label.
const OUTPUT_EVENTS: &[&str] = &[
    "turn_start",
    "content_chunk",
    "liveness",
    "turn_identity",
    "tool_started",
    "tool_completed",
];

/// The ledger's clock for activity durations: monotonic, so a change to the
/// system time cannot make an agent look silent or active.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// A ledgered send, as a caller sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct SendSnapshot {
    pub project_id: ProjectId,
    /// Whether the dispatch phase has finished. Until it has, recipients not
    /// yet offered read as `not_dispatched`, which is terminal, so a send
    /// still being prepared would otherwise look finished.
    pub settled: bool,
    pub recipients: Vec<RecipientSnapshot>,
}

impl SendSnapshot {
    /// Whether dispatch finished and every recipient reached a terminal status.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.settled && self.recipients.iter().all(|r| r.status.is_terminal())
    }
}

/// The receipt a send returns: the server-minted `send_id` and what happened
/// to each recipient at dispatch time.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SendReceipt {
    pub send_id: SendId,
    pub recipients: Vec<RecipientSnapshot>,
}

/// A workflow run's state as observed on its progress channel.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunSnapshot {
    pub run_id: Uuid,
    pub project_id: ProjectId,
    pub workflow: String,
    pub status: RunState,
    /// Zero-based index of the step in progress, or the step that failed.
    pub step: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The step's label and its agents as the engine resolved them, once it
    /// has.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recipients: Vec<ResolvedRecipient>,
    /// Every send the run's send steps made so far, in step order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sends: Vec<RunSend>,
}

/// One send a workflow step made: follow it with `wait_send`, and read any
/// recipient's reply with `read_reply` and this `send_id`, even after
/// the agent answered a later step.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunSend {
    pub step: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub send_id: Uuid,
    pub recipients: Vec<ResolvedRecipient>,
}

/// The outcome of a claim on an idempotency key.
pub enum Claim {
    /// This caller owns the request; `send_id` is freshly minted.
    Fresh { send_id: SendId },
    /// An identical request is in flight or already finished; await its result.
    /// Carries the original's `send_id`, so a joiner that gives up first can
    /// still tell the caller which send to watch.
    Joined {
        send_id: SendId,
        result: watch::Receiver<Option<ClaimResult>>,
    },
    /// The key is claimed by a different request.
    Mismatch,
}

/// What a joined claim resolves to: the original's receipt, or the code and
/// message of the error that released the key.
pub type ClaimResult = Result<SendReceipt, Arc<AppError>>;

/// What an entry is: a message to one or more agents, or one agent's
/// compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendKind {
    Message,
    Compaction,
}

/// The outcome of a fork request's claim: see the module doc's retry rule.
pub enum ForkClaim {
    /// No fork yet: create one and send to it under this id.
    Fresh { send_id: SendId },
    /// An attempt is in flight, or the fork's message went out or was
    /// cancelled: wait for, or take, its result.
    Joined {
        send_id: SendId,
        result: watch::Receiver<Option<ClaimResult>>,
    },
    /// The fork exists and its message never dispatched: send it again under
    /// this fresh id.
    Resume { send_id: SendId, fork: AgentRecord },
    /// The key was used for a different request.
    Mismatch,
}

/// The outcome of a compaction's claim: see the module doc's merge rule.
pub enum CompactionClaim {
    /// Queue a new compaction under this id.
    Fresh { send_id: SendId },
    /// A compaction for the agent has not settled, or this key was claimed by
    /// the same request: wait for its result.
    Joined {
        send_id: SendId,
        result: watch::Receiver<Option<ClaimResult>>,
    },
    /// A compaction is already queued or running for the agent: its receipt.
    Reused { receipt: SendReceipt },
    /// The key was used for a different request.
    Mismatch,
}

struct RecipientEntry {
    agent_id: AgentId,
    message_id: Option<MessageId>,
    turn_id: Option<TurnId>,
    /// When the recipient was admitted to the agent's queue (marked accepted
    /// at dispatch); the order the compaction merge rule reads.
    admitted_at: Option<DateTime<Utc>>,
    /// What happened at dispatch time: `Accepted`, `Rejected`, or
    /// `NotDispatched`. The receipt reports this; `status` moves on with the
    /// event stream.
    dispatch: SendStatus,
    status: SendStatus,
    text: Option<String>,
    error: Option<String>,
    code: Option<&'static str>,
    failure_kind: Option<FailureKind>,
    capture: TextCapture,
    /// `cancel_send` named this recipient alone: the send skips it if not yet
    /// offered, and cancels it if offered meanwhile.
    cancel_requested: bool,
}

impl RecipientEntry {
    fn snapshot(&self) -> RecipientSnapshot {
        RecipientSnapshot {
            agent_id: self.agent_id,
            activity: None,
            recent_text: None,
            context: None,
            message_id: self.message_id,
            status: self.status,
            text: self.text.clone(),
            error: self.error.clone(),
            code: self.code,
            failure_kind: self.failure_kind.clone(),
        }
    }

    /// The recipient as the receipt reports it: its dispatch-time outcome,
    /// without reply text.
    fn receipt(&self) -> RecipientSnapshot {
        RecipientSnapshot {
            agent_id: self.agent_id,
            activity: None,
            recent_text: None,
            context: None,
            message_id: self.message_id,
            status: self.dispatch,
            text: None,
            error: self.error.clone(),
            code: self.code,
            failure_kind: None,
        }
    }
}

struct SendEntry {
    project_id: ProjectId,
    created_at: DateTime<Utc>,
    kind: SendKind,
    settled: bool,
    /// Set by `cancel_send`. The send task reads it before each dispatch and
    /// after each admission, so a cancel that lands while the send is still
    /// preparing, or between the check and the dispatcher's answer, still
    /// stops it.
    cancel_requested: bool,
    /// Fired with `cancel_requested`, so preparation (a forward waiting on a
    /// busy source, a slow prompt provider) stops at once instead of running
    /// to its deadline. A child of the call's own cancel token, so server
    /// shutdown fires it too; the flag tells the two apart.
    cancel: CancellationToken,
    recipients: Vec<RecipientEntry>,
    /// The fork this send created, or was resumed for. An entry holding one
    /// is settled, never released, however its task ends.
    fork: Option<AgentRecord>,
    /// What a caller that joined this entry without a key of its own waits
    /// on: the receipt, or the error that released it.
    result: watch::Sender<Option<ClaimResult>>,
}

impl SendEntry {
    /// Settled and nothing still running: safe to drop by count.
    fn is_retired(&self) -> bool {
        self.settled && self.recipients.iter().all(|r| r.status.is_terminal())
    }
}

struct KeyClaim {
    send_id: SendId,
    request_hash: String,
    result: watch::Sender<Option<ClaimResult>>,
    /// The fork a request under this key created. Once set, the key is never
    /// released: a repeat must find this fork, not make another.
    fork: Option<AgentRecord>,
    claimed_at: DateTime<Utc>,
}

struct RunEntry {
    snapshot: RunSnapshot,
    recorded_at: DateTime<Utc>,
}

/// The ledger entry an agent's newest turn (as seen on its channel) belongs
/// to: `None` when the ledger never registered that turn.
type LatestTurn = Option<(SendId, usize)>;

#[derive(Default)]
struct Inner {
    sends: HashMap<SendId, SendEntry>,
    send_order: VecDeque<SendId>,
    claims: HashMap<(ProjectId, String), KeyClaim>,
    by_message: HashMap<MessageId, (SendId, usize)>,
    by_turn: HashMap<TurnId, (SendId, usize)>,
    runs: HashMap<Uuid, RunEntry>,
    run_order: VecDeque<Uuid>,
    latest_turn: HashMap<AgentId, LatestTurn>,
    /// Each agent's last output and open tool calls.
    activity: HashMap<AgentId, ActivityState>,
}

pub struct SendLedger {
    inner: Mutex<Inner>,
    /// Bumped on every change to a send or run; waits on either park here.
    changed: watch::Sender<u64>,
    /// Bumped on any event on an agent's channel; `wait_agent` parks here.
    agent_signals: Mutex<HashMap<AgentId, watch::Sender<u64>>>,
    clock: Clock,
}

impl Default for SendLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl SendLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(Arc::new(Instant::now))
    }

    /// A ledger whose activity durations read `clock`; tests set the time.
    #[must_use]
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            changed: watch::Sender::new(0),
            agent_signals: Mutex::new(HashMap::new()),
            clock,
        }
    }

    // --- claims and registration ------------------------------------------

    /// Claim `key` for a request with `request_hash` in `project_id`, or mint a
    /// fresh send with no key. Atomic under the ledger lock, so two concurrent
    /// repeats cannot both come out `Fresh`.
    pub fn claim(&self, project_id: ProjectId, key: Option<&str>, request_hash: &str) -> Claim {
        let Some(key) = key else {
            return Claim::Fresh {
                send_id: Uuid::now_v7(),
            };
        };
        let mut inner = lock(&self.inner);
        if let Some(existing) = inner.claims.get(&(project_id, key.to_owned())) {
            if existing.request_hash == request_hash {
                return Claim::Joined {
                    send_id: existing.send_id,
                    result: existing.result.subscribe(),
                };
            }
            return Claim::Mismatch;
        }
        let send_id = Uuid::now_v7();
        claim_key(
            &mut inner,
            project_id,
            Some(key),
            request_hash,
            send_id,
            None,
        );
        Claim::Fresh { send_id }
    }

    /// Create the entry for a fresh send with every recipient `NotDispatched`.
    /// Must precede the first dispatch, so the actor's first event finds it.
    pub fn register_send(
        &self,
        send_id: SendId,
        project_id: ProjectId,
        recipients: &[AgentId],
        cancel: CancellationToken,
    ) {
        let mut inner = lock(&self.inner);
        insert_send(
            &mut inner,
            send_id,
            project_id,
            SendKind::Message,
            recipients,
            None,
            cancel,
        );
        drop(inner);
        self.bump();
    }

    /// Claim a fork request: see [`ForkClaim`] and the module doc's retry
    /// rule. A fresh claim's entry is created here, with no recipient, so
    /// `cancel_send` and a joiner that gives up can name the send before the
    /// fork exists.
    pub fn claim_fork(
        &self,
        project_id: ProjectId,
        key: Option<&str>,
        request_hash: &str,
        cancel: CancellationToken,
    ) -> ForkClaim {
        let mut inner = lock(&self.inner);
        if let Some(key) = key
            && let Some(existing) = inner.claims.get(&(project_id, key.to_owned()))
        {
            if existing.request_hash != request_hash {
                return ForkClaim::Mismatch;
            }
            let joined = ForkClaim::Joined {
                send_id: existing.send_id,
                result: existing.result.subscribe(),
            };
            let fork = existing.fork.clone();
            let outcome = (*existing.result.borrow()).clone();
            if let Some(fork) = fork
                && never_dispatched(outcome.as_ref())
            {
                let send_id = Uuid::now_v7();
                insert_send(
                    &mut inner,
                    send_id,
                    project_id,
                    SendKind::Message,
                    &[fork.id],
                    Some(fork.clone()),
                    cancel,
                );
                // The key moves to the new attempt with a result slot of its
                // own: a repeat that joined the refused attempt keeps that
                // attempt's receipt, however the two are scheduled.
                if let Some(claim) = inner.claims.get_mut(&(project_id, key.to_owned())) {
                    claim.send_id = send_id;
                    claim.claimed_at = Utc::now();
                    claim.result = watch::Sender::new(None);
                }
                drop(inner);
                self.bump();
                return ForkClaim::Resume { send_id, fork };
            }
            return joined;
        }
        let send_id = Uuid::now_v7();
        claim_key(&mut inner, project_id, key, request_hash, send_id, None);
        insert_send(
            &mut inner,
            send_id,
            project_id,
            SendKind::Message,
            &[],
            None,
            cancel,
        );
        drop(inner);
        self.bump();
        ForkClaim::Fresh { send_id }
    }

    /// Record the fork a request created: as the send's recipient, and on
    /// its key, which is kept from now on whatever happens to the message.
    pub fn attach_fork(&self, send_id: SendId, fork: &AgentRecord) {
        let mut inner = lock(&self.inner);
        if let Some(entry) = inner.sends.get_mut(&send_id) {
            if entry.recipients.is_empty() {
                entry.recipients.push(recipient_entry(fork.id));
            }
            entry.fork = Some(fork.clone());
        }
        for claim in inner.claims.values_mut().filter(|c| c.send_id == send_id) {
            claim.fork = Some(fork.clone());
        }
        drop(inner);
        self.bump();
    }

    /// The fork a key names, if a request under it created one.
    #[must_use]
    pub fn fork_for_key(&self, project_id: ProjectId, key: &str) -> Option<AgentRecord> {
        lock(&self.inner)
            .claims
            .get(&(project_id, key.to_owned()))?
            .fork
            .clone()
    }

    /// Whether the send created, or was resumed for, a fork.
    #[must_use]
    pub fn fork_attached(&self, send_id: SendId) -> bool {
        lock(&self.inner)
            .sends
            .get(&send_id)
            .is_some_and(|entry| entry.fork.is_some())
    }

    /// Mark every recipient not yet offered to the dispatcher `Rejected` with
    /// the code and message of the refusal that ended the send's preparation,
    /// for a send that is settled rather than released.
    pub fn reject_undispatched(&self, send_id: SendId, code: &'static str, message: &str) {
        let mut inner = lock(&self.inner);
        if let Some(entry) = inner.sends.get_mut(&send_id) {
            for recipient in &mut entry.recipients {
                if recipient.dispatch == SendStatus::NotDispatched {
                    recipient.dispatch = SendStatus::Rejected;
                    recipient.status = SendStatus::Rejected;
                    recipient.code = Some(code);
                    recipient.error = Some(message.to_owned());
                }
            }
        }
        drop(inner);
        self.bump();
    }

    /// Claim a compaction for `agent_id`: the key's own claim first, then the
    /// module doc's merge rule, then a fresh entry, created here under the same
    /// lock, so two requests arriving at once queue one compaction. `cancel`
    /// is the fresh entry's; a joined or reused request's is unused.
    pub fn claim_compaction(
        &self,
        project_id: ProjectId,
        agent_id: AgentId,
        key: Option<&str>,
        request_hash: &str,
        cancel: CancellationToken,
    ) -> CompactionClaim {
        let mut inner = lock(&self.inner);
        if let Some(key) = key
            && let Some(existing) = inner.claims.get(&(project_id, key.to_owned()))
        {
            if existing.request_hash == request_hash {
                return CompactionClaim::Joined {
                    send_id: existing.send_id,
                    result: existing.result.subscribe(),
                };
            }
            return CompactionClaim::Mismatch;
        }
        // A compaction for the agent that has not settled, admitted or not:
        // join it. Until `settle`, its dispatch can still fail and release it,
        // so a receipt handed out now could name an id the ledger drops.
        let preparing = inner.send_order.iter().rev().find_map(|id| {
            let entry = inner.sends.get(id)?;
            (entry.kind == SendKind::Compaction
                && entry.project_id == project_id
                && !entry.settled
                && !entry.cancel_requested
                && entry
                    .recipients
                    .first()
                    .is_some_and(|r| r.agent_id == agent_id))
            .then_some(*id)
        });
        if let Some(send_id) = preparing {
            let result = inner.sends[&send_id].result.subscribe();
            claim_key(&mut inner, project_id, key, request_hash, send_id, None);
            return CompactionClaim::Joined { send_id, result };
        }
        // The newest entry admitted to the agent's queue and not yet finished.
        let newest_admitted = inner
            .sends
            .iter()
            .filter(|(_, entry)| entry.project_id == project_id && !entry.cancel_requested)
            .filter_map(|(id, entry)| {
                let recipient = entry.recipients.iter().find(|r| r.agent_id == agent_id)?;
                let admitted_at = recipient.admitted_at?;
                (!recipient.status.is_terminal()).then_some((admitted_at, *id))
            })
            .max_by_key(|(admitted_at, _)| *admitted_at)
            .map(|(_, id)| id);
        if let Some(send_id) = newest_admitted
            && let Some(entry) = inner.sends.get(&send_id)
            && entry.kind == SendKind::Compaction
        {
            let receipt = receipt_of(send_id, entry);
            claim_key(
                &mut inner,
                project_id,
                key,
                request_hash,
                send_id,
                Some(Ok(receipt.clone())),
            );
            return CompactionClaim::Reused { receipt };
        }
        let send_id = Uuid::now_v7();
        claim_key(&mut inner, project_id, key, request_hash, send_id, None);
        // The entry exists before the lock is released, so the next claim
        // finds it.
        insert_send(
            &mut inner,
            send_id,
            project_id,
            SendKind::Compaction,
            &[agent_id],
            None,
            cancel,
        );
        drop(inner);
        self.bump();
        CompactionClaim::Fresh { send_id }
    }

    /// Mint and register the `message_id` for one recipient, marking it
    /// `Accepted`. Called immediately before that recipient's dispatch.
    pub fn mark_accepted(&self, send_id: SendId, agent_id: AgentId) -> Option<MessageId> {
        let message_id = Uuid::now_v7();
        let mut inner = lock(&self.inner);
        let index = inner
            .sends
            .get(&send_id)?
            .recipients
            .iter()
            .position(|r| r.agent_id == agent_id)?;
        {
            let recipient = &mut inner.sends.get_mut(&send_id)?.recipients[index];
            recipient.message_id = Some(message_id);
            recipient.admitted_at = Some(Utc::now());
            recipient.dispatch = SendStatus::Accepted;
            recipient.status = SendStatus::Accepted;
        }
        inner.by_message.insert(message_id, (send_id, index));
        drop(inner);
        self.bump();
        Some(message_id)
    }

    /// Mark one recipient `Rejected` with the code and message of the refusal.
    /// Undoes [`Self::mark_accepted`]'s registration when the dispatcher
    /// refused the message after it was minted.
    pub fn mark_rejected(
        &self,
        send_id: SendId,
        agent_id: AgentId,
        code: &'static str,
        message: &str,
    ) {
        let mut inner = lock(&self.inner);
        let Some(entry) = inner.sends.get_mut(&send_id) else {
            return;
        };
        let Some(recipient) = entry.recipients.iter_mut().find(|r| r.agent_id == agent_id) else {
            return;
        };
        recipient.dispatch = SendStatus::Rejected;
        recipient.status = SendStatus::Rejected;
        recipient.code = Some(code);
        recipient.error = Some(message.to_owned());
        let stale = recipient.message_id.take();
        if let Some(message_id) = stale {
            inner.by_message.remove(&message_id);
        }
        drop(inner);
        self.bump();
    }

    /// The send's receipt as of now, from its dispatch-time statuses.
    #[must_use]
    pub fn receipt(&self, send_id: SendId) -> Option<SendReceipt> {
        let inner = lock(&self.inner);
        let entry = inner.sends.get(&send_id)?;
        Some(receipt_of(send_id, entry))
    }

    /// Record that the send's dispatch phase is over with at least one
    /// recipient dispatched: the receipt is final and any joiner gets it. A
    /// send that dispatched nothing is [`Self::release`]d instead.
    pub fn settle(&self, send_id: SendId) -> Option<SendReceipt> {
        let receipt = self.receipt(send_id)?;
        let mut inner = lock(&self.inner);
        if let Some(entry) = inner.sends.get_mut(&send_id) {
            entry.settled = true;
            entry.result.send_replace(Some(Ok(receipt.clone())));
        }
        // Every key associated with the send, not only the one it was
        // registered under: a compaction request merged into this one left its
        // own key pointing here.
        for claim in inner.claims.values().filter(|c| c.send_id == send_id) {
            claim.result.send_replace(Some(Ok(receipt.clone())));
        }
        // An entry becomes retirable here, so this is where the count bound
        // can first apply to it.
        prune(&mut inner, Utc::now());
        drop(inner);
        self.bump();
        Some(receipt)
    }

    /// The send a key currently names.
    #[cfg(test)]
    #[must_use]
    pub fn send_id_for_key(&self, project_id: ProjectId, key: &str) -> Option<SendId> {
        let inner = lock(&self.inner);
        let send_id = inner.claims.get(&(project_id, key.to_owned()))?.send_id;
        inner.sends.contains_key(&send_id).then_some(send_id)
    }

    /// Ask a send to stop: flag it and fire its cancel signal. Returns the
    /// recipients the dispatcher may own — accepted or running — which the
    /// caller cancels there; the send task cancels the rest itself.
    pub fn request_cancel(&self, send_id: SendId) -> Option<Vec<AgentId>> {
        let mut inner = lock(&self.inner);
        let entry = inner.sends.get_mut(&send_id)?;
        entry.cancel_requested = true;
        entry.cancel.cancel();
        Some(
            entry
                .recipients
                .iter()
                .filter(|r| !r.status.is_terminal())
                .map(|r| r.agent_id)
                .collect(),
        )
    }

    /// Whether `cancel_send` has asked this send to stop.
    #[must_use]
    pub fn cancel_requested(&self, send_id: SendId) -> bool {
        lock(&self.inner)
            .sends
            .get(&send_id)
            .is_some_and(|e| e.cancel_requested)
    }

    /// Ask for one recipient's reply to stop: flag it, so the send skips the
    /// recipient if it has not offered it the message yet and cancels it if it
    /// is offering it now. `false` when the ledger does not hold the send or
    /// the recipient.
    pub fn request_cancel_recipient(&self, send_id: SendId, agent_id: AgentId) -> bool {
        let mut inner = lock(&self.inner);
        let Some(recipient) = inner
            .sends
            .get_mut(&send_id)
            .and_then(|entry| entry.recipients.iter_mut().find(|r| r.agent_id == agent_id))
        else {
            return false;
        };
        recipient.cancel_requested = true;
        true
    }

    /// Whether `cancel_send` asked this send to stop, whole or for any one of
    /// its recipients. A workflow's send stops dispatching on either, since a
    /// cancelled reply ends its run.
    #[must_use]
    pub fn any_cancel_requested(&self, send_id: SendId) -> bool {
        lock(&self.inner).sends.get(&send_id).is_some_and(|entry| {
            entry.cancel_requested || entry.recipients.iter().any(|r| r.cancel_requested)
        })
    }

    /// Whether `cancel_send` asked for this recipient's reply alone to stop.
    #[must_use]
    pub fn recipient_cancel_requested(&self, send_id: SendId, agent_id: AgentId) -> bool {
        lock(&self.inner).sends.get(&send_id).is_some_and(|entry| {
            entry
                .recipients
                .iter()
                .any(|r| r.agent_id == agent_id && r.cancel_requested)
        })
    }

    /// Whether every recipient of the send has ended or been cancelled one by
    /// one, so nobody is left to receive it. A recipient not yet offered the
    /// message by a send still dispatching is still to receive it.
    #[must_use]
    pub fn no_recipient_left(&self, send_id: SendId) -> bool {
        lock(&self.inner).sends.get(&send_id).is_some_and(|entry| {
            entry.recipients.iter().all(|r| {
                r.cancel_requested
                    || (r.status.is_terminal()
                        && (r.status != SendStatus::NotDispatched || entry.settled))
            })
        })
    }

    /// Mark one recipient not yet offered its message as cancelled, at
    /// dispatch time and now: the send skipped it.
    pub fn cancel_recipient_undispatched(&self, send_id: SendId, agent_id: AgentId) {
        let mut inner = lock(&self.inner);
        if let Some(recipient) = inner
            .sends
            .get_mut(&send_id)
            .and_then(|entry| entry.recipients.iter_mut().find(|r| r.agent_id == agent_id))
            && recipient.dispatch == SendStatus::NotDispatched
        {
            recipient.dispatch = SendStatus::Cancelled;
            recipient.status = SendStatus::Cancelled;
        }
        drop(inner);
        self.bump();
    }

    /// Whether a send this ledger holds is still to reach `agent_id` or is
    /// running on it: queued, being offered, or not yet offered by a send still
    /// dispatching. What a read of the agent's latest answer checks, beside its
    /// running turn, since a send's turn is not open until the agent starts it.
    #[must_use]
    pub fn has_unfinished_send_to(&self, agent_id: AgentId) -> bool {
        lock(&self.inner).sends.values().any(|entry| {
            entry.recipients.iter().any(|r| {
                r.agent_id == agent_id
                    && (!r.status.is_terminal()
                        || (r.status == SendStatus::NotDispatched && !entry.settled))
            })
        })
    }

    /// Mark every recipient not yet offered to the dispatcher as cancelled —
    /// at dispatch time and now — for a send stopped by `cancel_send`.
    pub fn cancel_undispatched(&self, send_id: SendId) {
        let mut inner = lock(&self.inner);
        if let Some(entry) = inner.sends.get_mut(&send_id) {
            for recipient in &mut entry.recipients {
                if recipient.dispatch == SendStatus::NotDispatched {
                    recipient.dispatch = SendStatus::Cancelled;
                    recipient.status = SendStatus::Cancelled;
                }
            }
        }
        drop(inner);
        self.bump();
    }

    /// Whether any recipient of the send was accepted by the dispatcher.
    #[must_use]
    pub fn dispatched_any(&self, send_id: SendId) -> bool {
        lock(&self.inner).sends.get(&send_id).is_some_and(|entry| {
            entry
                .recipients
                .iter()
                .any(|r| r.dispatch == SendStatus::Accepted)
        })
    }

    /// Release a claimed key because the request ended with nothing
    /// dispatched: the entry is dropped and any joiner receives the error.
    pub fn release(&self, send_id: SendId, error: &Arc<AppError>) {
        let mut inner = lock(&self.inner);
        release_locked(&mut inner, send_id, error);
        drop(inner);
        self.bump();
    }

    // --- queries -----------------------------------------------------------

    #[must_use]
    pub fn send(&self, send_id: SendId) -> Option<SendSnapshot> {
        let now = (self.clock)();
        let inner = lock(&self.inner);
        let entry = inner.sends.get(&send_id)?;
        Some(SendSnapshot {
            project_id: entry.project_id,
            settled: entry.settled,
            recipients: entry
                .recipients
                .iter()
                .map(|recipient| {
                    let mut snapshot = recipient.snapshot();
                    // Only a running recipient's own turn: one still waiting in
                    // line would otherwise show the turn ahead of it, which may
                    // be the user's, as if it were this send's.
                    if snapshot.status == SendStatus::Running {
                        snapshot.activity = inner
                            .activity
                            .get(&recipient.agent_id)
                            .filter(|state| state.turn_open)
                            .map(|state| state.report(now));
                        snapshot.recent_text = Some(recipient.capture.tail(RECENT_TEXT_CHARS))
                            .filter(|text| !text.is_empty());
                    }
                    snapshot
                })
                .collect(),
        })
    }

    /// What the agent's running turn is doing; `None` while no turn runs.
    #[must_use]
    pub fn activity(&self, agent_id: AgentId) -> Option<AgentActivity> {
        let now = (self.clock)();
        lock(&self.inner)
            .activity
            .get(&agent_id)
            .filter(|state| state.turn_open)
            .map(|state| state.report(now))
    }

    /// `Some` while the agent has a turn running, whoever sent it, holding the
    /// send it answers when its start event named one; `None` while no turn
    /// runs. A send still waiting for the agent has no turn yet.
    #[must_use]
    pub fn open_turn(&self, agent_id: AgentId) -> Option<Option<SendId>> {
        lock(&self.inner)
            .activity
            .get(&agent_id)
            .filter(|state| state.turn_open)
            .map(|state| state.open_send)
    }

    /// Note an agent-channel event against the agent's activity: a turn's
    /// start or the agent's output restarts the silence, a tool call opens or
    /// closes, and a turn's end closes the turn and whatever it left open.
    fn note_activity(&self, agent_id: AgentId, kind: &str, payload: &serde_json::Value) {
        let output = OUTPUT_EVENTS.contains(&kind);
        if !output && kind != "turn_end" {
            return;
        }
        let now = (self.clock)();
        let mut inner = lock(&self.inner);
        if !output {
            if let Some(state) = inner.activity.get_mut(&agent_id) {
                state.open_tools.clear();
                state.turn_open = false;
                state.open_send = None;
            }
            return;
        }
        let state = inner
            .activity
            .entry(agent_id)
            .or_insert_with(|| ActivityState {
                last_output: now,
                open_tools: Vec::new(),
                turn_open: false,
                open_send: None,
            });
        state.last_output = now;
        if kind == "turn_start" {
            state.turn_open = true;
            state.open_send = field_uuid(payload, "send_id");
            state.open_tools.clear();
        }
        let tool_use_id = payload
            .get("tool_use_id")
            .and_then(serde_json::Value::as_str);
        match (kind, tool_use_id) {
            ("tool_started", Some(id)) => state.open_tools.push(OpenTool {
                tool_use_id: id.to_owned(),
                name: payload
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("tool")
                    .to_owned(),
                started: now,
            }),
            ("tool_completed", Some(id)) => state.open_tools.retain(|tool| tool.tool_use_id != id),
            _ => {}
        }
    }

    /// The agent's newest turn when the ledger holds it, with its send.
    #[must_use]
    pub fn latest_turn(&self, agent_id: AgentId) -> Option<(SendId, RecipientSnapshot)> {
        let inner = lock(&self.inner);
        let (send_id, index) = (*inner.latest_turn.get(&agent_id)?)?;
        let entry = inner.sends.get(&send_id)?;
        Some((send_id, entry.recipients.get(index)?.snapshot()))
    }

    #[must_use]
    pub fn run(&self, run_id: Uuid) -> Option<RunSnapshot> {
        lock(&self.inner)
            .runs
            .get(&run_id)
            .map(|r| r.snapshot.clone())
    }

    #[must_use]
    pub fn runs_for_project(&self, project_id: ProjectId) -> Vec<RunSnapshot> {
        lock(&self.inner)
            .runs
            .values()
            .filter(|r| r.snapshot.project_id == project_id)
            .map(|r| r.snapshot.clone())
            .collect()
    }

    /// Subscribe to the change signal for sends and runs. Subscribe **before**
    /// reading the state a wait depends on, so a change between the read and
    /// the park is not lost.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// Subscribe to any event on `agent_id`'s channel. Same ordering rule as
    /// [`Self::subscribe`].
    #[must_use]
    pub fn subscribe_agent(&self, agent_id: AgentId) -> watch::Receiver<u64> {
        lock(&self.agent_signals)
            .entry(agent_id)
            .or_insert_with(|| watch::Sender::new(0))
            .subscribe()
    }

    /// Drop every send and run older than `MAX_AGE` as of `now`. Called on
    /// insert with the current time; exposed so a test can age the ledger
    /// without a clock.
    pub fn prune(&self, now: DateTime<Utc>) {
        prune(&mut lock(&self.inner), now);
    }

    /// Forget one send as pruning does, keeping its run: the state a run is
    /// in once newer sends have pushed its own out past the count bound,
    /// reached here without registering hundreds of them.
    #[cfg(test)]
    pub fn forget_send(&self, send_id: SendId) {
        drop_send(&mut lock(&self.inner), send_id);
    }

    // --- event observation --------------------------------------------------

    /// Feed one emitted event. Routes on the channel name and the event's
    /// `type` first, then parses only the events it acts on into their typed
    /// forms. Content is parsed outside the lock.
    pub fn observe(&self, channel: &str, payload: &serde_json::Value) {
        if let Some(agent_id) = channel.strip_prefix("agent:").and_then(parse_uuid) {
            self.observe_agent(agent_id, payload);
        } else if let Some(project_id) = channel.strip_prefix("workflow:").and_then(parse_uuid) {
            self.observe_run(project_id, payload);
        }
    }

    fn observe_agent(&self, agent_id: AgentId, payload: &serde_json::Value) {
        let Some(kind) = payload.get("type").and_then(serde_json::Value::as_str) else {
            return;
        };
        self.note_activity(agent_id, kind, payload);
        match kind {
            // Content can only change text, never admission or completion:
            // feed the capture and wake nobody.
            "content_chunk" | "tool_started" | "tool_completed" => {
                self.on_content(payload);
                return;
            }
            "turn_start" | "turn_end" | "message_failed" | "message_cancelled" | "agent_idle" => {}
            _ => return,
        }
        let changed = match serde_json::from_value::<NormalizedEvent>(payload.clone()) {
            Ok(NormalizedEvent::TurnStart {
                turn_id,
                message_id,
                ..
            }) => self.on_turn_start(agent_id, message_id, turn_id),
            Ok(NormalizedEvent::TurnEnd {
                turn_id, outcome, ..
            }) => self.on_turn_end(turn_id, &outcome),
            Ok(NormalizedEvent::MessageFailed {
                message_id,
                error,
                kind,
                ..
            }) => self.on_message_failed(message_id, error, kind),
            Ok(NormalizedEvent::MessageCancelled { message_id, .. }) => {
                self.on_message_cancelled(message_id)
            }
            Ok(_) => false,
            Err(e) => {
                tracing::warn!(%agent_id, kind, error = %e, "ledger could not parse a lifecycle event");
                false
            }
        };
        // Admission can change on every lifecycle event (an `agent_idle`
        // included), so the agent's waiters are woken after the state moved.
        self.bump_agent(agent_id);
        if changed {
            self.bump();
        }
    }

    fn on_turn_start(&self, agent_id: AgentId, message_id: MessageId, turn_id: TurnId) -> bool {
        let mut inner = lock(&self.inner);
        let registered: LatestTurn = inner.by_message.get(&message_id).copied();
        // A compaction is not an answer: the agent's newest answer stays
        // whatever it was, as it does for a compaction the window ran.
        let compaction = registered.is_some_and(|(send_id, _)| {
            inner
                .sends
                .get(&send_id)
                .is_some_and(|e| e.kind == SendKind::Compaction)
        });
        if !compaction {
            inner.latest_turn.insert(agent_id, registered);
        }
        let Some((send_id, index)) = registered else {
            return false;
        };
        inner.by_turn.insert(turn_id, (send_id, index));
        if let Some(recipient) = recipient_mut(&mut inner, send_id, index) {
            recipient.turn_id = Some(turn_id);
            recipient.status = SendStatus::Running;
        }
        true
    }

    fn on_content(&self, payload: &serde_json::Value) {
        let Some(turn_id) = field_uuid(payload, "turn_id") else {
            return;
        };
        let registered = lock(&self.inner).by_turn.get(&turn_id).copied();
        let Some((send_id, index)) = registered else {
            return;
        };
        let Ok(event) = serde_json::from_value::<NormalizedEvent>(payload.clone()) else {
            return;
        };
        let mut inner = lock(&self.inner);
        if let Some(recipient) = recipient_mut(&mut inner, send_id, index) {
            recipient.capture.observe_normalized(&event);
        }
    }

    fn on_turn_end(&self, turn_id: TurnId, outcome: &TurnOutcome) -> bool {
        let mut inner = lock(&self.inner);
        let Some((send_id, index)) = inner.by_turn.get(&turn_id).copied() else {
            return false;
        };
        let Some(recipient) = recipient_mut(&mut inner, send_id, index) else {
            return false;
        };
        match outcome {
            TurnOutcome::Completed => {
                recipient.status = SendStatus::Completed;
                recipient.text = Some(std::mem::take(&mut recipient.capture).finish());
            }
            TurnOutcome::Cancelled { .. } => recipient.status = SendStatus::Cancelled,
            TurnOutcome::Failed { kind, message } => {
                recipient.status = SendStatus::Failed;
                recipient.error = Some(message.clone());
                recipient.failure_kind = Some(kind.clone());
            }
            // A turn end is always terminal, whatever a future outcome variant
            // means: leaving the recipient running would make a wait never
            // finish.
            _ => {
                tracing::warn!(%turn_id, "unrecognized turn outcome; recording the turn as failed");
                recipient.status = SendStatus::Failed;
                recipient.error = Some("the turn ended with an unrecognized outcome".to_owned());
            }
        }
        true
    }

    fn on_message_failed(
        &self,
        message_id: MessageId,
        error: String,
        kind: Option<MessageFailureKind>,
    ) -> bool {
        let mut inner = lock(&self.inner);
        let Some((send_id, index)) = inner.by_message.get(&message_id).copied() else {
            return false;
        };
        let Some(recipient) = recipient_mut(&mut inner, send_id, index) else {
            return false;
        };
        recipient.status = SendStatus::Failed;
        recipient.error = Some(error);
        recipient.code = match kind {
            Some(MessageFailureKind::SessionInUse) => Some(error_code::AGENT_BUSY_ELSEWHERE),
            _ => None,
        };
        true
    }

    fn on_message_cancelled(&self, message_id: MessageId) -> bool {
        let mut inner = lock(&self.inner);
        let Some((send_id, index)) = inner.by_message.get(&message_id).copied() else {
            return false;
        };
        let Some(recipient) = recipient_mut(&mut inner, send_id, index) else {
            return false;
        };
        recipient.status = SendStatus::Cancelled;
        true
    }

    fn observe_run(&self, project_id: ProjectId, payload: &serde_json::Value) {
        let progress = match serde_json::from_value::<WorkflowProgressPayload>(payload.clone()) {
            Ok(progress) => progress,
            Err(e) => {
                tracing::warn!(%project_id, error = %e, "ledger could not parse a workflow progress event");
                return;
            }
        };
        let Some(run_id) = parse_uuid(&progress.run_id) else {
            return;
        };
        let status = match progress.status {
            WorkflowProgressStatus::Running => RunState::Running,
            WorkflowProgressStatus::Complete => RunState::Completed,
            WorkflowProgressStatus::Cancelled => RunState::Cancelled,
            // A status added later lands here as a compile error, not as a
            // silent `failed`: the enum is this crate's own.
            WorkflowProgressStatus::Failed => RunState::Failed,
        };
        let snapshot = RunSnapshot {
            run_id,
            project_id,
            workflow: progress.workflow,
            status,
            step: progress.step,
            total: progress.total,
            reason: progress.reason,
            label: progress.label,
            recipients: progress.recipients,
            sends: Vec::new(),
        };
        let now = Utc::now();
        let mut inner = lock(&self.inner);
        let mut snapshot = snapshot;
        snapshot.sends = inner
            .runs
            .get(&run_id)
            .map(|entry| entry.snapshot.sends.clone())
            .unwrap_or_default();
        if let Some(send_id) = progress.send_id {
            let send = RunSend {
                step: snapshot.step,
                label: snapshot.label.clone(),
                send_id,
                recipients: snapshot.recipients.clone(),
            };
            if let Some(existing) = snapshot.sends.iter_mut().find(|s| s.step == send.step) {
                *existing = send;
            } else {
                snapshot.sends.push(send);
                snapshot.sends.sort_by_key(|s| s.step);
            }
        }
        if !inner.runs.contains_key(&run_id) {
            inner.run_order.push_back(run_id);
        }
        inner.runs.insert(
            run_id,
            RunEntry {
                snapshot,
                recorded_at: now,
            },
        );
        prune(&mut inner, now);
        drop(inner);
        self.bump();
    }

    fn bump(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    fn bump_agent(&self, agent_id: AgentId) {
        if let Some(signal) = lock(&self.agent_signals).get(&agent_id) {
            signal.send_modify(|v| *v += 1);
        }
    }
}

fn release_locked(inner: &mut Inner, send_id: SendId, error: &Arc<AppError>) {
    let Some(entry) = inner.sends.remove(&send_id) else {
        return;
    };
    inner.send_order.retain(|id| *id != send_id);
    for recipient in &entry.recipients {
        if let Some(message_id) = recipient.message_id {
            inner.by_message.remove(&message_id);
        }
        if let Some(turn_id) = recipient.turn_id {
            inner.by_turn.remove(&turn_id);
        }
    }
    entry.result.send_replace(Some(Err(Arc::clone(error))));
    let released: Vec<(ProjectId, String)> = inner
        .claims
        .iter()
        .filter(|(_, c)| c.send_id == send_id)
        .map(|(k, _)| k.clone())
        .collect();
    for key in released {
        if let Some(claim) = inner.claims.remove(&key) {
            claim.result.send_replace(Some(Err(Arc::clone(error))));
        }
    }
}

/// Record `key`, when given, as a claim pointing at `send_id`, settled already
/// when `preset` carries the receipt it reuses.
fn claim_key(
    inner: &mut Inner,
    project_id: ProjectId,
    key: Option<&str>,
    request_hash: &str,
    send_id: SendId,
    preset: Option<ClaimResult>,
) {
    if let Some(key) = key {
        inner.claims.insert(
            (project_id, key.to_owned()),
            KeyClaim {
                send_id,
                request_hash: request_hash.to_owned(),
                result: watch::Sender::new(preset),
                fork: None,
                claimed_at: Utc::now(),
            },
        );
    }
}

/// Whether a fork attempt sent nothing: its message was refused before it
/// dispatched, or the attempt ended abnormally. An attempt still in flight
/// has not, nor has one whose message went out or was cancelled.
fn never_dispatched(outcome: Option<&ClaimResult>) -> bool {
    match outcome {
        None => false,
        Some(Ok(receipt)) => !receipt
            .recipients
            .iter()
            .any(|r| matches!(r.status, SendStatus::Accepted | SendStatus::Cancelled)),
        Some(Err(_)) => true,
    }
}

/// Insert a fresh entry with every recipient `NotDispatched`, under the
/// caller's lock, so a claim and its entry appear together. A fork's entry
/// starts with no recipient: the fork is the recipient, and it does not
/// exist yet.
fn insert_send(
    inner: &mut Inner,
    send_id: SendId,
    project_id: ProjectId,
    kind: SendKind,
    recipients: &[AgentId],
    fork: Option<AgentRecord>,
    cancel: CancellationToken,
) {
    let now = Utc::now();
    inner.sends.insert(
        send_id,
        SendEntry {
            project_id,
            created_at: now,
            kind,
            settled: false,
            cancel_requested: false,
            cancel,
            recipients: recipients
                .iter()
                .map(|&agent_id| recipient_entry(agent_id))
                .collect(),
            fork,
            result: watch::Sender::new(None),
        },
    );
    inner.send_order.push_back(send_id);
    prune(inner, now);
}

fn recipient_entry(agent_id: AgentId) -> RecipientEntry {
    RecipientEntry {
        agent_id,
        message_id: None,
        turn_id: None,
        admitted_at: None,
        dispatch: SendStatus::NotDispatched,
        status: SendStatus::NotDispatched,
        text: None,
        error: None,
        code: None,
        failure_kind: None,
        capture: TextCapture::default(),
        cancel_requested: false,
    }
}

/// The entry's receipt from its dispatch-time statuses.
fn receipt_of(send_id: SendId, entry: &SendEntry) -> SendReceipt {
    SendReceipt {
        send_id,
        recipients: entry
            .recipients
            .iter()
            .map(RecipientEntry::receipt)
            .collect(),
    }
}

fn recipient_mut(inner: &mut Inner, send_id: SendId, index: usize) -> Option<&mut RecipientEntry> {
    inner.sends.get_mut(&send_id)?.recipients.get_mut(index)
}

/// Forget one send: its entry, its message and turn indexes, and its retry
/// key's claim. A key naming a fork outlives its entry, up to the age bound
/// in [`prune`], so a repeat under it finds the fork rather than making
/// another.
fn drop_send(inner: &mut Inner, send_id: SendId) {
    inner.send_order.retain(|id| *id != send_id);
    if let Some(entry) = inner.sends.remove(&send_id) {
        for recipient in &entry.recipients {
            if let Some(message_id) = recipient.message_id {
                inner.by_message.remove(&message_id);
            }
            if let Some(turn_id) = recipient.turn_id {
                inner.by_turn.remove(&turn_id);
            }
        }
        inner
            .claims
            .retain(|_, claim| claim.send_id != send_id || claim.fork.is_some());
    }
}

/// Drop sends and runs older than [`MAX_AGE`], and retired ones past the
/// count bound. Live sends are stepped past, never dropped by count. A dropped
/// send's claim stays only while the entry does: once the receipt is gone, a
/// retry with the same key is a new request.
fn prune(inner: &mut Inner, now: DateTime<Utc>) {
    let cutoff = now - MAX_AGE;
    let retired = inner
        .send_order
        .iter()
        .filter(|id| inner.sends.get(*id).is_none_or(SendEntry::is_retired))
        .count();
    let mut to_drop_by_count = retired.saturating_sub(MAX_ENTRIES);
    let mut dropped: Vec<SendId> = Vec::new();
    for &send_id in &inner.send_order {
        let Some(entry) = inner.sends.get(&send_id) else {
            dropped.push(send_id);
            continue;
        };
        if entry.created_at < cutoff {
            dropped.push(send_id);
        } else if to_drop_by_count > 0 && entry.is_retired() {
            to_drop_by_count -= 1;
            dropped.push(send_id);
        }
    }
    for send_id in dropped {
        drop_send(inner, send_id);
    }
    inner
        .claims
        .retain(|_, claim| claim.fork.is_none() || claim.claimed_at >= cutoff);
    let terminal_runs = inner
        .run_order
        .iter()
        .filter(|id| {
            inner
                .runs
                .get(*id)
                .is_none_or(|r| r.snapshot.status != RunState::Running)
        })
        .count();
    let mut runs_to_drop = terminal_runs.saturating_sub(MAX_ENTRIES);
    let mut dropped_runs: Vec<Uuid> = Vec::new();
    for &run_id in &inner.run_order {
        let Some(run) = inner.runs.get(&run_id) else {
            dropped_runs.push(run_id);
            continue;
        };
        if run.recorded_at < cutoff {
            dropped_runs.push(run_id);
        } else if runs_to_drop > 0 && run.snapshot.status != RunState::Running {
            runs_to_drop -= 1;
            dropped_runs.push(run_id);
        }
    }
    for run_id in dropped_runs {
        inner.run_order.retain(|id| *id != run_id);
        inner.runs.remove(&run_id);
    }
}

fn parse_uuid(text: &str) -> Option<Uuid> {
    Uuid::parse_str(text).ok()
}

fn field_uuid(payload: &serde_json::Value, key: &str) -> Option<Uuid> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .and_then(parse_uuid)
}

/// Wraps the app's event sink: feeds every event to the ledger, then forwards
/// it unchanged. Outermost in the chain, so the ledger sees exactly what the
/// window sees.
pub struct OpsLedgerEmitter {
    inner: std::sync::Arc<dyn EventEmitter>,
    ledger: std::sync::Arc<SendLedger>,
    context: std::sync::Arc<super::context::ContextStore>,
}

impl OpsLedgerEmitter {
    pub fn new(
        inner: std::sync::Arc<dyn EventEmitter>,
        ledger: std::sync::Arc<SendLedger>,
        context: std::sync::Arc<super::context::ContextStore>,
    ) -> Self {
        Self {
            inner,
            ledger,
            context,
        }
    }
}

impl EventEmitter for OpsLedgerEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        // Context first: the ledger wakes a send's waiters on its turn's end,
        // and the reply they read carries the agent's context after that turn.
        self.context.observe(name, &payload);
        self.ledger.observe(name, &payload);
        self.inner.emit(name, payload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use switchboard_dispatcher::RecordingEmitter;

    fn channel(agent_id: AgentId) -> String {
        format!("agent:{agent_id}")
    }

    fn turn_start(message_id: MessageId, turn_id: TurnId, send_id: SendId) -> serde_json::Value {
        json!({
            "type": "turn_start",
            "turn_id": turn_id,
            "message_id": message_id,
            "send_id": send_id,
            "started_at": "2026-06-17T00:00:00Z",
        })
    }

    fn chunk(turn_id: TurnId, text: &str) -> serde_json::Value {
        json!({ "type": "content_chunk", "turn_id": turn_id, "kind": "text", "text": text })
    }

    fn turn_end(turn_id: TurnId, outcome: &serde_json::Value) -> serde_json::Value {
        json!({
            "type": "turn_end",
            "turn_id": turn_id,
            "outcome": outcome,
            "ended_at": "2026-06-17T00:00:01Z",
            "usage": null, "spend": null, "model": null, "effort": null, "hydration_key": null,
        })
    }

    /// A registered send with one accepted recipient.
    fn registered(ledger: &SendLedger) -> (ProjectId, SendId, AgentId, MessageId) {
        let project_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let Claim::Fresh { send_id } = ledger.claim(project_id, Some("k"), "h") else {
            panic!("fresh");
        };
        ledger.register_send(send_id, project_id, &[agent_id], CancellationToken::new());
        let message_id = ledger.mark_accepted(send_id, agent_id).unwrap();
        ledger.settle(send_id);
        (project_id, send_id, agent_id, message_id)
    }

    /// A clock the test moves by hand, and the handle that moves it.
    fn hand_clock() -> (Clock, Arc<Mutex<Instant>>) {
        let now = Arc::new(Mutex::new(Instant::now()));
        let reader = Arc::clone(&now);
        (Arc::new(move || *reader.lock().unwrap()), now)
    }

    fn advance(now: &Arc<Mutex<Instant>>, seconds: u64) {
        *now.lock().unwrap() += std::time::Duration::from_secs(seconds);
    }

    fn tool_started(turn_id: TurnId, id: &str, name: &str) -> serde_json::Value {
        json!({ "type": "tool_started", "turn_id": turn_id, "tool_use_id": id, "kind": "other", "name": name, "input": {} })
    }

    fn tool_completed(turn_id: TurnId, id: &str) -> serde_json::Value {
        json!({ "type": "tool_completed", "turn_id": turn_id, "tool_use_id": id, "output": "", "is_error": false })
    }

    #[test]
    fn silence_counts_only_the_turns_start_and_the_agents_own_output() {
        let (clock, now) = hand_clock();
        let ledger = SendLedger::with_clock(clock);
        let (_, send_id, agent_id, message_id) = registered(&ledger);
        let turn_id = Uuid::now_v7();
        ledger.observe(
            &channel(agent_id),
            &turn_start(message_id, turn_id, send_id),
        );
        assert_eq!(
            ledger.activity(agent_id).unwrap().seconds_since_output,
            0,
            "a turn starting restarts the silence"
        );
        advance(&now, 30);
        ledger.observe(&channel(agent_id), &chunk(turn_id, "working"));
        advance(&now, 90);
        assert_eq!(ledger.activity(agent_id).unwrap().seconds_since_output, 90);
        // Queue traffic for the agent is not the agent producing anything.
        ledger.observe(
            &channel(agent_id),
            &json!({ "type": "send_accepted", "message_id": Uuid::now_v7(), "send_id": Uuid::now_v7() }),
        );
        ledger.observe(
            &channel(agent_id),
            &json!({ "type": "message_cancelled", "message_id": Uuid::now_v7() }),
        );
        assert_eq!(ledger.activity(agent_id).unwrap().seconds_since_output, 90);
        ledger.observe(
            &channel(agent_id),
            &json!({ "type": "liveness", "turn_id": turn_id }),
        );
        assert_eq!(ledger.activity(agent_id).unwrap().seconds_since_output, 0);
    }

    #[test]
    fn a_turn_counts_from_its_own_start_and_an_idle_agent_reports_nothing() {
        let (clock, now) = hand_clock();
        let ledger = SendLedger::with_clock(clock);
        let (_, send_id, agent_id, message_id) = registered(&ledger);
        // A first turn that never produces anything still counts its silence.
        let first = Uuid::now_v7();
        ledger.observe(&channel(agent_id), &turn_start(message_id, first, send_id));
        advance(&now, 1_500);
        assert_eq!(
            ledger.activity(agent_id).unwrap().seconds_since_output,
            1_500
        );
        ledger.observe(
            &channel(agent_id),
            &turn_end(first, &json!({ "status": "completed" })),
        );
        // Between turns there is nothing to be stuck on.
        advance(&now, 3_600);
        assert_eq!(ledger.activity(agent_id), None);
        // A new turn after an hour idle starts from zero, not from the idle time.
        ledger.observe(
            &channel(agent_id),
            &turn_start(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()),
        );
        assert_eq!(ledger.activity(agent_id).unwrap().seconds_since_output, 0);
        advance(&now, 60);
        assert_eq!(ledger.activity(agent_id).unwrap().seconds_since_output, 60);
    }

    #[test]
    fn an_open_tool_call_is_reported_until_it_or_its_turn_ends() {
        let (clock, now) = hand_clock();
        let ledger = SendLedger::with_clock(clock);
        let agent_id = Uuid::now_v7();
        let turn_id = Uuid::now_v7();
        ledger.observe(
            &channel(agent_id),
            &turn_start(Uuid::now_v7(), turn_id, Uuid::now_v7()),
        );
        ledger.observe(&channel(agent_id), &tool_started(turn_id, "t1", "Bash"));
        advance(&now, 600);
        assert_eq!(
            ledger.activity(agent_id).unwrap(),
            AgentActivity {
                seconds_since_output: 600,
                open_tool_calls: vec![OpenToolCall {
                    name: "Bash".to_owned(),
                    running_seconds: 600
                }],
            },
            "a test suite running is silence with a tool call open"
        );
        ledger.observe(&channel(agent_id), &tool_completed(turn_id, "t1"));
        assert_eq!(
            ledger.activity(agent_id).unwrap(),
            AgentActivity {
                seconds_since_output: 0,
                open_tool_calls: Vec::new()
            }
        );
        ledger.observe(&channel(agent_id), &tool_started(turn_id, "t2", "Read"));
        ledger.observe(
            &channel(agent_id),
            &turn_end(turn_id, &json!({ "status": "cancelled" })),
        );
        assert_eq!(
            ledger.activity(agent_id),
            None,
            "a turn's end closes the turn and what it left open"
        );
    }

    #[test]
    fn an_unfinished_recipient_carries_its_agents_activity() {
        let (clock, _now) = hand_clock();
        let ledger = SendLedger::with_clock(clock);
        let (_, send_id, agent_id, message_id) = registered(&ledger);
        let turn_id = Uuid::now_v7();
        ledger.observe(
            &channel(agent_id),
            &turn_start(message_id, turn_id, send_id),
        );
        ledger.observe(&channel(agent_id), &tool_started(turn_id, "t1", "Bash"));
        let running = ledger.send(send_id).unwrap();
        assert_eq!(
            running.recipients[0]
                .activity
                .as_ref()
                .unwrap()
                .open_tool_calls[0]
                .name,
            "Bash"
        );
        ledger.observe(
            &channel(agent_id),
            &turn_end(turn_id, &json!({ "status": "completed" })),
        );
        let done = ledger.send(send_id).unwrap();
        assert_eq!(
            done.recipients[0].activity, None,
            "a finished recipient carries its reply instead"
        );
    }

    #[test]
    fn a_recipient_waiting_in_line_carries_no_activity_of_the_turn_ahead() {
        let ledger = SendLedger::new();
        let (_, send_id, agent_id, message_id) = registered(&ledger);
        // Another turn holds the agent, one this ledger did not send (the
        // user's, say): its silence is not this send's.
        let other_turn = Uuid::now_v7();
        ledger.observe(
            &channel(agent_id),
            &turn_start(Uuid::now_v7(), other_turn, Uuid::now_v7()),
        );
        assert!(ledger.activity(agent_id).is_some(), "the agent is busy");
        let waiting = ledger.send(send_id).unwrap();
        assert_eq!(waiting.recipients[0].status, SendStatus::Accepted);
        assert_eq!(waiting.recipients[0].activity, None);

        ledger.observe(
            &channel(agent_id),
            &turn_end(other_turn, &json!({ "status": "completed" })),
        );
        let own_turn = Uuid::now_v7();
        ledger.observe(
            &channel(agent_id),
            &turn_start(message_id, own_turn, send_id),
        );
        let running = ledger.send(send_id).unwrap();
        assert!(
            running.recipients[0].activity.is_some(),
            "its own turn's activity, once it runs"
        );
    }

    #[test]
    fn a_running_recipient_carries_the_end_of_its_text_so_far() {
        let ledger = SendLedger::new();
        let (_, send_id, agent_id, message_id) = registered(&ledger);
        let queued = ledger.send(send_id).unwrap();
        assert_eq!(
            queued.recipients[0].recent_text, None,
            "nothing written yet is nothing to show"
        );
        let turn_id = Uuid::now_v7();
        ledger.observe(
            &channel(agent_id),
            &turn_start(message_id, turn_id, send_id),
        );
        let long = "x".repeat(RECENT_TEXT_CHARS);
        ledger.observe(&channel(agent_id), &chunk(turn_id, &long));
        ledger.observe(&channel(agent_id), &chunk(turn_id, "Running the tests."));
        let running = ledger.send(send_id).unwrap();
        let recent = running.recipients[0].recent_text.as_deref().unwrap();
        assert_eq!(recent.chars().count(), RECENT_TEXT_CHARS);
        assert!(recent.ends_with("Running the tests."));
        ledger.observe(
            &channel(agent_id),
            &turn_end(turn_id, &json!({ "status": "completed" })),
        );
        let done = ledger.send(send_id).unwrap();
        assert_eq!(
            done.recipients[0].recent_text, None,
            "a finished recipient carries its whole reply instead"
        );
        assert!(
            done.recipients[0]
                .text
                .as_deref()
                .unwrap()
                .ends_with("Running the tests.")
        );
    }

    #[test]
    fn an_immediate_turn_start_lands_on_the_registered_entry() {
        let ledger = SendLedger::new();
        let (_, send_id, agent_id, message_id) = registered(&ledger);
        let turn_id = Uuid::now_v7();
        ledger.observe(
            &channel(agent_id),
            &turn_start(message_id, turn_id, send_id),
        );
        ledger.observe(&channel(agent_id), &chunk(turn_id, "hel"));
        ledger.observe(&channel(agent_id), &chunk(turn_id, "lo"));
        let snapshot = ledger.send(send_id).unwrap();
        assert_eq!(snapshot.recipients[0].status, SendStatus::Running);
        assert!(!snapshot.is_done());
        ledger.observe(
            &channel(agent_id),
            &turn_end(turn_id, &json!({ "status": "completed" })),
        );
        let snapshot = ledger.send(send_id).unwrap();
        assert_eq!(snapshot.recipients[0].status, SendStatus::Completed);
        assert_eq!(snapshot.recipients[0].text.as_deref(), Some("hello"));
        assert!(snapshot.is_done());
        let (latest_send, latest) = ledger.latest_turn(agent_id).unwrap();
        assert_eq!(latest_send, send_id);
        assert_eq!(latest.text.as_deref(), Some("hello"));
    }

    #[test]
    fn an_immediate_message_failed_lands_on_the_registered_entry_with_its_kind() {
        let ledger = SendLedger::new();
        let (_, send_id, agent_id, message_id) = registered(&ledger);
        ledger.observe(
            &channel(agent_id),
            &json!({
                "type": "message_failed",
                "message_id": message_id,
                "send_id": null,
                "agent_id": agent_id,
                "error": "held elsewhere",
                "kind": "session_in_use",
                "at": "2026-06-17T00:00:00Z",
            }),
        );
        let recipient = &ledger.send(send_id).unwrap().recipients[0];
        assert_eq!(recipient.status, SendStatus::Failed);
        assert_eq!(recipient.code, Some(error_code::AGENT_BUSY_ELSEWHERE));
        assert_eq!(recipient.error.as_deref(), Some("held elsewhere"));
    }

    #[test]
    fn cancelled_and_failed_terminals_are_recorded() {
        let ledger = SendLedger::new();
        let (_, send_id, agent_id, message_id) = registered(&ledger);
        let turn_id = Uuid::now_v7();
        ledger.observe(
            &channel(agent_id),
            &turn_start(message_id, turn_id, send_id),
        );
        ledger.observe(&channel(agent_id), &chunk(turn_id, "partial"));
        ledger.observe(
            &channel(agent_id),
            &turn_end(turn_id, &json!({ "status": "cancelled", "source": "user" })),
        );
        let recipient = &ledger.send(send_id).unwrap().recipients[0];
        assert_eq!(recipient.status, SendStatus::Cancelled);
        assert!(recipient.text.is_none(), "a cancelled turn has no answer");

        let (_, send_id, agent_id, message_id) = registered(&ledger);
        let turn_id = Uuid::now_v7();
        ledger.observe(
            &channel(agent_id),
            &turn_start(message_id, turn_id, send_id),
        );
        ledger.observe(
            &channel(agent_id),
            &turn_end(
                turn_id,
                &json!({ "status": "failed", "kind": "harness_error", "message": "boom" }),
            ),
        );
        let recipient = &ledger.send(send_id).unwrap().recipients[0];
        assert_eq!(recipient.status, SendStatus::Failed);
        assert_eq!(recipient.error.as_deref(), Some("boom"));

        let (_, send_id, agent_id, message_id) = registered(&ledger);
        ledger.observe(
            &channel(agent_id),
            &json!({
                "type": "message_cancelled",
                "message_id": message_id,
                "send_id": send_id,
                "agent_id": agent_id,
                "at": "2026-06-17T00:00:00Z",
            }),
        );
        assert_eq!(
            ledger.send(send_id).unwrap().recipients[0].status,
            SendStatus::Cancelled
        );
    }

    #[test]
    fn events_for_unregistered_messages_are_ignored_but_the_latest_turn_is_noted() {
        let ledger = SendLedger::new();
        let (_, send_id, agent_id, message_id) = registered(&ledger);
        let turn_id = Uuid::now_v7();
        ledger.observe(
            &channel(agent_id),
            &turn_start(message_id, turn_id, send_id),
        );
        ledger.observe(
            &channel(agent_id),
            &turn_end(turn_id, &json!({ "status": "completed" })),
        );
        assert!(ledger.latest_turn(agent_id).is_some());
        // A compose send the ledger never registered.
        let foreign = Uuid::now_v7();
        ledger.observe(
            &channel(agent_id),
            &turn_start(Uuid::now_v7(), foreign, Uuid::now_v7()),
        );
        ledger.observe(&channel(agent_id), &chunk(foreign, "not ours"));
        assert!(
            ledger.latest_turn(agent_id).is_none(),
            "the agent's newest turn is not one the ledger holds"
        );
        assert_eq!(
            ledger.send(send_id).unwrap().recipients[0].text.as_deref(),
            Some(""),
            "the foreign turn's text did not land on the registered entry"
        );
    }

    #[test]
    fn the_open_turn_names_its_send_even_one_the_ledger_never_registered() {
        let ledger = SendLedger::new();
        let agent_id = Uuid::now_v7();
        assert_eq!(ledger.open_turn(agent_id), None, "no turn has run");
        // A compose send the ledger never registered.
        let typed = Uuid::now_v7();
        let turn_id = Uuid::now_v7();
        ledger.observe(
            &channel(agent_id),
            &turn_start(Uuid::now_v7(), turn_id, typed),
        );
        assert_eq!(ledger.open_turn(agent_id), Some(Some(typed)));
        ledger.observe(
            &channel(agent_id),
            &turn_end(turn_id, &json!({ "status": "completed" })),
        );
        assert_eq!(ledger.open_turn(agent_id), None, "the turn ended");
    }

    #[test]
    fn a_turn_whose_start_names_no_send_still_reads_as_open() {
        let ledger = SendLedger::new();
        let agent_id = Uuid::now_v7();
        let mut start = turn_start(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        start["send_id"] = json!("not a send id");
        ledger.observe(&channel(agent_id), &start);
        assert_eq!(ledger.open_turn(agent_id), Some(None));
        assert!(ledger.activity(agent_id).is_some());
    }

    #[test]
    fn a_live_send_survives_the_count_bound_but_not_the_age_bound() {
        let ledger = SendLedger::new();
        let project_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let Claim::Fresh { send_id: live } = ledger.claim(project_id, Some("live"), "h") else {
            panic!("fresh");
        };
        ledger.register_send(live, project_id, &[agent_id], CancellationToken::new());
        ledger.mark_accepted(live, agent_id).unwrap();
        for _ in 0..(MAX_ENTRIES + 10) {
            let send_id = Uuid::now_v7();
            ledger.register_send(
                send_id,
                project_id,
                &[Uuid::now_v7()],
                CancellationToken::new(),
            );
            ledger.settle(send_id);
        }
        assert!(
            ledger.send(live).is_some(),
            "a running send is never dropped by count"
        );
        assert!(matches!(
            ledger.claim(project_id, Some("live"), "h"),
            Claim::Joined { .. }
        ));
        ledger.prune(Utc::now() + MAX_AGE + Duration::seconds(1));
        assert!(
            ledger.send(live).is_none(),
            "the age bound applies to everything"
        );
    }

    #[test]
    fn bounded_by_count_and_age() {
        let ledger = SendLedger::new();
        let project_id = Uuid::now_v7();
        let mut ids = Vec::new();
        for _ in 0..(MAX_ENTRIES + 10) {
            let send_id = Uuid::now_v7();
            ledger.register_send(
                send_id,
                project_id,
                &[Uuid::now_v7()],
                CancellationToken::new(),
            );
            ledger.settle(send_id);
            ids.push(send_id);
        }
        assert!(ledger.send(ids[0]).is_none(), "the oldest was pruned");
        assert!(ledger.send(ids[9]).is_none());
        assert!(ledger.send(ids[10]).is_some());
        assert!(ledger.send(*ids.last().unwrap()).is_some());

        ledger.prune(Utc::now() + MAX_AGE + Duration::seconds(1));
        assert!(
            ledger.send(*ids.last().unwrap()).is_none(),
            "everything aged out"
        );
        // A pruned key is a fresh request again.
        let Claim::Fresh { .. } = ledger.claim(project_id, Some("k"), "h") else {
            panic!("fresh");
        };
    }

    #[test]
    fn a_run_terminal_is_kept_after_its_file_is_gone() {
        let ledger = SendLedger::new();
        let project_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let progress = |status: &str, step: usize| {
            json!({
                "run_id": run_id, "workflow": "pair", "step": step, "total": 2,
                "status": status, "reason": null,
            })
        };
        ledger.observe(&format!("workflow:{project_id}"), &progress("running", 0));
        assert_eq!(ledger.run(run_id).unwrap().status, RunState::Running);
        ledger.observe(&format!("workflow:{project_id}"), &progress("running", 1));
        assert_eq!(ledger.run(run_id).unwrap().step, 1);
        // The run file is deleted at this point in production; the ledger is
        // the only record left.
        ledger.observe(&format!("workflow:{project_id}"), &progress("complete", 1));
        let run = ledger.run(run_id).unwrap();
        assert_eq!(run.status, RunState::Completed);
        assert_eq!(ledger.runs_for_project(project_id).len(), 1);
    }

    #[tokio::test]
    async fn the_per_agent_signal_fires_on_any_event_for_that_agent() {
        let ledger = SendLedger::new();
        let agent_id = Uuid::now_v7();
        let other = Uuid::now_v7();
        let mut signal = ledger.subscribe_agent(agent_id);
        ledger.observe(
            &channel(other),
            &json!({ "type": "agent_idle", "agent_id": other }),
        );
        assert!(
            !signal.has_changed().unwrap(),
            "another agent's event is not this agent's signal"
        );
        ledger.observe(
            &channel(agent_id),
            &json!({ "type": "agent_idle", "agent_id": agent_id }),
        );
        assert!(signal.has_changed().unwrap());
        signal.changed().await.unwrap();
        ledger.observe(&channel(agent_id), &chunk(Uuid::now_v7(), "streaming"));
        assert!(
            !signal.has_changed().unwrap(),
            "a content chunk cannot change admission, so it wakes nobody"
        );
        ledger.observe(
            &channel(agent_id),
            &turn_start(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()),
        );
        assert!(signal.has_changed().unwrap(), "a turn boundary does");
    }

    #[tokio::test]
    async fn claims_join_and_release() {
        let ledger = SendLedger::new();
        let project_id = Uuid::now_v7();
        let Claim::Fresh { send_id } = ledger.claim(project_id, Some("k"), "h") else {
            panic!("fresh");
        };
        ledger.register_send(
            send_id,
            project_id,
            &[Uuid::now_v7()],
            CancellationToken::new(),
        );
        let Claim::Joined {
            send_id: joined_id,
            result: mut joined,
        } = ledger.claim(project_id, Some("k"), "h")
        else {
            panic!("joined");
        };
        assert_eq!(joined_id, send_id, "a join names the original send");
        assert!(matches!(
            ledger.claim(project_id, Some("k"), "other"),
            Claim::Mismatch
        ));
        assert!(matches!(
            ledger.claim(Uuid::now_v7(), Some("k"), "h"),
            Claim::Fresh { .. }
        ));
        ledger.release(
            send_id,
            &Arc::new(AppError::UnknownRecipient {
                project_id,
                agent_id: Uuid::now_v7(),
            }),
        );
        let settled = joined.wait_for(Option::is_some).await.unwrap().clone();
        let original = settled.unwrap().unwrap_err();
        assert_eq!(
            error_code::error_code(&original),
            error_code::UNKNOWN_RECIPIENT
        );
        assert!(
            ledger.send(send_id).is_none(),
            "a released send leaves no entry"
        );
        assert!(matches!(
            ledger.claim(project_id, Some("k"), "h"),
            Claim::Fresh { .. }
        ));
    }

    #[test]
    fn the_decorator_forwards_every_event_unchanged() {
        let inner = std::sync::Arc::new(RecordingEmitter::new());
        let ledger = std::sync::Arc::new(SendLedger::new());
        let emitter = OpsLedgerEmitter::new(
            std::sync::Arc::clone(&inner) as std::sync::Arc<dyn EventEmitter>,
            ledger,
            std::sync::Arc::new(super::super::context::ContextStore::new()),
        );
        let payload = json!({ "type": "agent_idle", "agent_id": Uuid::now_v7() });
        emitter.emit("agent:x", payload.clone());
        emitter.emit("not-a-channel", json!("string"));
        let recorded = inner.snapshot();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].1, payload);
    }

    fn compaction_claim(
        ledger: &SendLedger,
        project_id: ProjectId,
        agent_id: AgentId,
        key: Option<&str>,
    ) -> CompactionClaim {
        ledger.claim_compaction(
            project_id,
            agent_id,
            key,
            "compact",
            CancellationToken::new(),
        )
    }

    #[test]
    fn two_compaction_claims_in_a_row_queue_one() {
        let ledger = SendLedger::new();
        let (project_id, agent_id) = (Uuid::now_v7(), Uuid::now_v7());
        let CompactionClaim::Fresh { send_id } =
            compaction_claim(&ledger, project_id, agent_id, Some("k1"))
        else {
            panic!("fresh");
        };
        // Nothing ran between the two claims: the entry already exists.
        let CompactionClaim::Joined {
            send_id: joined, ..
        } = compaction_claim(&ledger, project_id, agent_id, Some("k2"))
        else {
            panic!("the second claim joins the first");
        };
        assert_eq!(joined, send_id);
        let CompactionClaim::Joined {
            send_id: joined, ..
        } = compaction_claim(&ledger, project_id, agent_id, None)
        else {
            panic!("a keyless claim joins too");
        };
        assert_eq!(joined, send_id);
        // Another agent is unaffected.
        assert!(matches!(
            compaction_claim(&ledger, project_id, Uuid::now_v7(), None),
            CompactionClaim::Fresh { .. }
        ));
    }

    #[test]
    fn a_compaction_marked_accepted_is_joined_until_it_settles_and_its_release_reaches_the_joiner()
    {
        let ledger = SendLedger::new();
        let (project_id, agent_id) = (Uuid::now_v7(), Uuid::now_v7());
        let CompactionClaim::Fresh { send_id } =
            compaction_claim(&ledger, project_id, agent_id, None)
        else {
            panic!("fresh");
        };
        ledger.mark_accepted(send_id, agent_id).unwrap();
        // Admitted but not settled: a dispatch can still fail and release it.
        let CompactionClaim::Joined { result, .. } =
            compaction_claim(&ledger, project_id, agent_id, Some("k"))
        else {
            panic!("joined, not reused");
        };
        ledger.release(send_id, &Arc::new(AppError::ProjectLoadTimeout(project_id)));
        assert!(
            matches!(result.borrow().as_ref(), Some(Err(e)) if matches!(**e, AppError::ProjectLoadTimeout(_))),
            "the joiner receives the release's error"
        );
        assert!(ledger.send(send_id).is_none());
        // Settled and still queued: reused.
        let CompactionClaim::Fresh { send_id } =
            compaction_claim(&ledger, project_id, agent_id, None)
        else {
            panic!("fresh after the release");
        };
        ledger.mark_accepted(send_id, agent_id).unwrap();
        ledger.settle(send_id);
        assert!(matches!(
            compaction_claim(&ledger, project_id, agent_id, Some("k2")),
            CompactionClaim::Reused { receipt } if receipt.send_id == send_id
        ));
    }

    #[test]
    fn a_compactions_turn_start_leaves_the_agents_newest_answer() {
        let ledger = SendLedger::new();
        let (project_id, send_id, agent_id, message_id) = registered(&ledger);
        let turn_id = Uuid::now_v7();
        ledger.observe(
            &channel(agent_id),
            &turn_start(message_id, turn_id, send_id),
        );
        ledger.observe(&channel(agent_id), &chunk(turn_id, "hello"));
        ledger.observe(
            &channel(agent_id),
            &turn_end(turn_id, &json!({ "status": "completed" })),
        );
        let CompactionClaim::Fresh {
            send_id: compaction,
        } = compaction_claim(&ledger, project_id, agent_id, None)
        else {
            panic!("fresh");
        };
        let compaction_message = ledger.mark_accepted(compaction, agent_id).unwrap();
        ledger.settle(compaction);
        let compaction_turn = Uuid::now_v7();
        ledger.observe(
            &channel(agent_id),
            &turn_start(compaction_message, compaction_turn, compaction),
        );
        let (latest_send, latest) = ledger.latest_turn(agent_id).unwrap();
        assert_eq!(
            latest_send, send_id,
            "the answer before the compaction stays newest"
        );
        assert_eq!(latest.text.as_deref(), Some("hello"));
        // The compaction's own lifecycle is still tracked.
        assert_eq!(
            ledger.send(compaction).unwrap().recipients[0].status,
            SendStatus::Running
        );
    }
}
