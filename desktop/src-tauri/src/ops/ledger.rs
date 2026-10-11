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
//! `AppState.emitter` is built, observes, and forwards unchanged.
//!
//! **Correlation.** `ops` chooses each send's `message_id` and registers it
//! here *before* dispatching, so the first event the actor emits for it lands
//! on an entry that already exists: no orphan map, no fallback matching.
//! `turn_start`, `message_failed`, and `message_cancelled` carry `message_id`;
//! `turn_end` and content carry the `turn_id` that `turn_start` announced.
//! Events for message ids the ledger never registered (compose sends,
//! workflow steps) are ignored, except that every agent's latest `turn_start`
//! is noted so a reader can tell whether the agent's newest turn is one the
//! ledger holds.
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
//! **Idempotency.** A caller's `idempotency_key`, scoped to the project, is
//! claimed here under the ledger lock together with a hash of the request. A
//! concurrent or later repeat with the same hash waits for and returns the
//! original receipt; a different hash is refused; a request that failed before
//! dispatching anything releases the key so the same request can be retried.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use switchboard_core::{AgentId, ProjectId, SendId};
use switchboard_dispatcher::EventEmitter;
use switchboard_harness::{
    FailureKind, MessageFailureKind, MessageId, NormalizedEvent, TextCapture, TurnId, TurnOutcome,
};
use tokio::sync::watch;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::lock;
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
}

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
}

/// The outcome of a claim on an idempotency key.
pub enum Claim {
    /// This caller owns the request; `send_id` is freshly minted.
    Fresh { send_id: SendId },
    /// An identical request is in flight or already finished; await its result.
    Joined(watch::Receiver<Option<ClaimResult>>),
    /// The key is claimed by a different request.
    Mismatch,
}

/// What a joined claim resolves to: the original's receipt, or the code and
/// message of the error that released the key.
pub type ClaimResult = Result<SendReceipt, Arc<AppError>>;

struct RecipientEntry {
    agent_id: AgentId,
    message_id: Option<MessageId>,
    turn_id: Option<TurnId>,
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
}

impl RecipientEntry {
    fn snapshot(&self) -> RecipientSnapshot {
        RecipientSnapshot {
            agent_id: self.agent_id,
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
    key: Option<String>,
    settled: bool,
    recipients: Vec<RecipientEntry>,
}

impl SendEntry {
    /// Settled and nothing still running: safe to drop by count.
    fn is_retired(&self) -> bool {
        self.settled && self.recipients.iter().all(|r| r.status.is_terminal())
    }
}

struct KeyClaim {
    request_hash: String,
    result: watch::Sender<Option<ClaimResult>>,
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
}

pub struct SendLedger {
    inner: Mutex<Inner>,
    /// Bumped on every change to a send or run; waits on either park here.
    changed: watch::Sender<u64>,
    /// Bumped on any event on an agent's channel; `wait_agent` parks here.
    agent_signals: Mutex<HashMap<AgentId, watch::Sender<u64>>>,
}

impl Default for SendLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl SendLedger {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            changed: watch::Sender::new(0),
            agent_signals: Mutex::new(HashMap::new()),
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
                return Claim::Joined(existing.result.subscribe());
            }
            return Claim::Mismatch;
        }
        let send_id = Uuid::now_v7();
        inner.claims.insert(
            (project_id, key.to_owned()),
            KeyClaim {
                request_hash: request_hash.to_owned(),
                result: watch::Sender::new(None),
            },
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
        key: Option<&str>,
    ) {
        let now = Utc::now();
        let mut inner = lock(&self.inner);
        inner.sends.insert(
            send_id,
            SendEntry {
                project_id,
                created_at: now,
                key: key.map(str::to_owned),
                settled: false,
                recipients: recipients
                    .iter()
                    .map(|&agent_id| RecipientEntry {
                        agent_id,
                        message_id: None,
                        turn_id: None,
                        dispatch: SendStatus::NotDispatched,
                        status: SendStatus::NotDispatched,
                        text: None,
                        error: None,
                        code: None,
                        failure_kind: None,
                        capture: TextCapture::default(),
                    })
                    .collect(),
            },
        );
        inner.send_order.push_back(send_id);
        prune(&mut inner, now);
        drop(inner);
        self.bump();
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
        Some(SendReceipt {
            send_id,
            recipients: entry
                .recipients
                .iter()
                .map(RecipientEntry::receipt)
                .collect(),
        })
    }

    /// Record that the send's dispatch phase is over with at least one
    /// recipient dispatched: the receipt is final and any joiner gets it. A
    /// send that dispatched nothing is [`Self::release`]d instead.
    pub fn settle(&self, send_id: SendId) -> Option<SendReceipt> {
        let receipt = self.receipt(send_id)?;
        let mut inner = lock(&self.inner);
        if let Some(entry) = inner.sends.get_mut(&send_id) {
            entry.settled = true;
        }
        if let Some(key) = inner.sends.get(&send_id).and_then(|e| e.key.clone())
            && let Some(project_id) = inner.sends.get(&send_id).map(|e| e.project_id)
            && let Some(claim) = inner.claims.get(&(project_id, key))
        {
            claim.result.send_replace(Some(Ok(receipt.clone())));
        }
        // An entry becomes retirable here, so this is where the count bound
        // can first apply to it.
        prune(&mut inner, Utc::now());
        drop(inner);
        self.bump();
        Some(receipt)
    }

    /// The send a key currently names, for a joiner reporting that the
    /// original is still in progress.
    #[must_use]
    pub fn send_id_for_key(&self, project_id: ProjectId, key: &str) -> Option<SendId> {
        let inner = lock(&self.inner);
        inner.claims.get(&(project_id, key.to_owned()))?;
        inner
            .sends
            .iter()
            .find(|(_, e)| e.project_id == project_id && e.key.as_deref() == Some(key))
            .map(|(id, _)| *id)
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
    pub fn release(&self, send_id: SendId, error: Arc<AppError>) {
        let mut inner = lock(&self.inner);
        release_locked(&mut inner, send_id, error);
        drop(inner);
        self.bump();
    }

    // --- queries -----------------------------------------------------------

    #[must_use]
    pub fn send(&self, send_id: SendId) -> Option<SendSnapshot> {
        let inner = lock(&self.inner);
        let entry = inner.sends.get(&send_id)?;
        Some(SendSnapshot {
            project_id: entry.project_id,
            settled: entry.settled,
            recipients: entry
                .recipients
                .iter()
                .map(RecipientEntry::snapshot)
                .collect(),
        })
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
        inner.latest_turn.insert(agent_id, registered);
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
        };
        let now = Utc::now();
        let mut inner = lock(&self.inner);
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

fn release_locked(inner: &mut Inner, send_id: SendId, error: Arc<AppError>) {
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
    if let Some(key) = entry.key
        && let Some(claim) = inner.claims.remove(&(entry.project_id, key))
    {
        claim.result.send_replace(Some(Err(error)));
    }
}

fn recipient_mut(inner: &mut Inner, send_id: SendId, index: usize) -> Option<&mut RecipientEntry> {
    inner.sends.get_mut(&send_id)?.recipients.get_mut(index)
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
            if let Some(key) = entry.key {
                inner.claims.remove(&(entry.project_id, key));
            }
        }
    }
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
}

impl OpsLedgerEmitter {
    pub fn new(
        inner: std::sync::Arc<dyn EventEmitter>,
        ledger: std::sync::Arc<SendLedger>,
    ) -> Self {
        Self { inner, ledger }
    }
}

impl EventEmitter for OpsLedgerEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
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
        ledger.register_send(send_id, project_id, &[agent_id], Some("k"));
        let message_id = ledger.mark_accepted(send_id, agent_id).unwrap();
        ledger.settle(send_id);
        (project_id, send_id, agent_id, message_id)
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
    fn a_live_send_survives_the_count_bound_but_not_the_age_bound() {
        let ledger = SendLedger::new();
        let project_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let Claim::Fresh { send_id: live } = ledger.claim(project_id, Some("live"), "h") else {
            panic!("fresh");
        };
        ledger.register_send(live, project_id, &[agent_id], Some("live"));
        ledger.mark_accepted(live, agent_id).unwrap();
        for _ in 0..(MAX_ENTRIES + 10) {
            let send_id = Uuid::now_v7();
            ledger.register_send(send_id, project_id, &[Uuid::now_v7()], None);
            ledger.settle(send_id);
        }
        assert!(
            ledger.send(live).is_some(),
            "a running send is never dropped by count"
        );
        assert!(matches!(
            ledger.claim(project_id, Some("live"), "h"),
            Claim::Joined(_)
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
            ledger.register_send(send_id, project_id, &[Uuid::now_v7()], None);
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
        ledger.register_send(send_id, project_id, &[Uuid::now_v7()], Some("k"));
        let Claim::Joined(mut joined) = ledger.claim(project_id, Some("k"), "h") else {
            panic!("joined");
        };
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
            Arc::new(AppError::UnknownRecipient {
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
        );
        let payload = json!({ "type": "agent_idle", "agent_id": Uuid::now_v7() });
        emitter.emit("agent:x", payload.clone());
        emitter.emit("not-a-channel", json!("string"));
        let recorded = inner.snapshot();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].1, payload);
    }
}
