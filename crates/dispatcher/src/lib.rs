//! Switchboard dispatcher — drives harness adapters.
//!
//! The `Dispatcher` is the single chokepoint for sending a turn to an agent.
//! It owns per-agent in-memory state, enforces the "one turn in flight per
//! agent" invariant via `AgentIdleGuard`, generates `TurnId`s, and forwards
//! adapter events to consumers through the `EventEmitter` abstraction.
//!
//! The `EventEmitter` trait makes the dispatcher unit-testable without a
//! running Tauri app — `RecordingEmitter` collects emissions for assertions;
//! production wiring (in `crates/app`) provides a Tauri-backed implementation.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use futures::StreamExt;
use switchboard_core::{AgentId, AgentRecord};
use switchboard_harness::{
    CancelSource, DispatchError, EventStream, FailureKind, HarnessAdapter, NormalizedEvent, TurnId,
    TurnOutcome,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Per-agent dispatch state. Two states cover the current contention
/// model; structured contention reasons are future work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentStatus {
    #[default]
    Idle,
    InFlight,
}

/// The per-agent entry the dispatcher keeps under its state lock. Beyond the
/// `status` flag it carries the in-flight turn's cancellation handle:
/// `cancel_token` is `Some` exactly while a turn is live and pre-terminal
/// (set on dispatch, cleared the moment the drain task observes a terminal
/// event or finishes). `cancel_source` records *why* a cancel was requested,
/// so the drain task can stamp the synthesized `TurnEnd { Cancelled { source } }`.
///
/// **Token presence is the cancellability signal.** A `cancel` call no-ops
/// when `cancel_token` is `None` — which covers both "agent not in flight"
/// and "already past its terminal event" (e.g. Codex's post-terminal
/// enrichment window before `AgentIdle`). No separate `terminal_seen` flag is
/// needed: clearing the token on the terminal event *is* the post-terminal
/// no-op guard.
///
/// M4.4 extends this entry with the per-agent FIFO queue + dispatch-context
/// factory; the queue is intentionally absent here.
#[derive(Default)]
struct AgentState {
    status: AgentStatus,
    cancel_token: Option<CancellationToken>,
    cancel_source: Option<CancelSource>,
    /// Notified when a turn's drain task finishes (status returns to `Idle`).
    /// Lets `wait_until_idle` await drain completion without polling — the
    /// cancel-and-drain lifecycle helper releases project locks only after
    /// this fires. Stable per agent across turns (the entry is insert-only).
    idle_notify: Arc<Notify>,
}

/// Errors returned by `Dispatcher::send_message`. `Busy` is the local
/// "agent already running a turn" guardrail; `Dispatch` lifts pre-stream
/// adapter failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DispatcherError {
    #[error("agent is busy")]
    Busy,
    #[error(transparent)]
    Dispatch(#[from] DispatchError),
    /// The user's *send* could not be persisted to the conversation journal,
    /// so the turn was **not started**. Persisting the send is a precondition
    /// (see [`ConversationJournal::record_send`]): a turn we can't record would
    /// surface after restart as an assistant reply with no user message above
    /// it. Returned before any subprocess is spawned — nothing to clean up.
    #[error(transparent)]
    Journal(#[from] JournalError),
}

/// A fail-closed conversation-journal write (`record_send`) failed. Wraps the
/// underlying persistence error for diagnostics; the dispatcher lifts it to
/// `DispatcherError::Journal` and refuses to start the turn.
#[derive(Debug, thiserror::Error)]
#[error("conversation journal write failed: {0}")]
pub struct JournalError(pub Box<dyn std::error::Error + Send + Sync + 'static>);

/// A sink for outbound events. The dispatcher emits one event per
/// `NormalizedEvent` on the channel `agent:<agent_id>` — the same channel for
/// the lifetime of the agent (per-agent, not per-turn).
pub trait EventEmitter: Send + Sync {
    fn emit(&self, name: &str, payload: serde_json::Value);
}

/// Test double — records emissions for assertion.
#[derive(Default)]
pub struct RecordingEmitter {
    events: Mutex<Vec<(String, serde_json::Value)>>,
}

impl RecordingEmitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of all emissions since construction.
    pub fn snapshot(&self) -> Vec<(String, serde_json::Value)> {
        lock(&self.events).clone()
    }

    pub fn len(&self) -> usize {
        lock(&self.events).len()
    }

    pub fn is_empty(&self) -> bool {
        lock(&self.events).is_empty()
    }
}

impl EventEmitter for RecordingEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        lock(&self.events).push((name.to_owned(), payload));
    }
}

/// Switchboard's user-side conversation persistence, injected like
/// [`EventEmitter`] so the dispatcher stays app-agnostic. The dispatcher owns
/// the *timing* (it knows when a turn starts and how it ends); the app-side
/// implementation owns the *path* (which project's `journal.jsonl`) and the
/// `send_id`; core owns the *file format*.
///
/// This refines the "Switchboard stores no transcript" invariant: Switchboard
/// owns the user-send journal + non-completed-turn outcomes; harness session
/// files own completed-turn agent content. The two partition cleanly.
///
/// **The two methods have deliberately different failure contracts**, because
/// they fire at points with different reversibility:
///
/// - `record_send` is **fail-closed**: it is written *before* the turn's
///   subprocess is spawned, and returns `Result`. If it fails, the dispatcher
///   refuses to start the turn. Rationale: the unified transcript sources the
///   user's messages from the journal, so a silently-lost send record becomes
///   an assistant reply with no user message above it after restart. If we
///   can't record that a turn is starting, we don't start it.
/// - `record_outcome` is **best-effort**: by the time it fires, the turn has
///   already run (it's in the detached drain task, with no return path). It
///   returns `()`; the implementation logs on failure and proceeds. The
///   degradation if it's lost is a mislabel (a cancelled/failed turn renders
///   as a generic "interrupted" turn after restart), not data loss.
///
/// `record_send`'s prompt is the user's side of the conversation (which
/// Switchboard legitimately owns); `record_outcome` carries no agent content,
/// only outcome metadata.
pub trait ConversationJournal: Send + Sync {
    /// Written when a turn *starts* (immediately before dispatch), one per
    /// recipient. The implementation supplies the `send_id` that groups a
    /// fan-out's recipients. **Fail-closed** — `Err` aborts the turn-start.
    fn record_send(
        &self,
        turn_id: TurnId,
        agent_id: AgentId,
        prompt: &str,
        at: DateTime<Utc>,
    ) -> Result<(), JournalError>;

    /// Written on a **non-completed** terminal (failed or cancelled) — never
    /// for a completed turn (whose content lives in the harness session file).
    /// **Best-effort** — the implementation logs on failure and proceeds.
    fn record_outcome(
        &self,
        turn_id: TurnId,
        agent_id: AgentId,
        outcome: &TurnOutcome,
        started_at: DateTime<Utc>,
        ended_at: DateTime<Utc>,
    );
}

/// No-op journal for tests and any caller that doesn't persist the user side.
pub struct NoopJournal;

impl ConversationJournal for NoopJournal {
    fn record_send(
        &self,
        _: TurnId,
        _: AgentId,
        _: &str,
        _: DateTime<Utc>,
    ) -> Result<(), JournalError> {
        Ok(())
    }
    fn record_outcome(
        &self,
        _: TurnId,
        _: AgentId,
        _: &TurnOutcome,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) {
    }
}

/// Outcome of a [`Dispatcher::cancel`] call. `NothingToCancel` is the typed
/// no-op for "agent not in flight" or "already past its terminal event."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CancelOutcome {
    /// A live, pre-terminal turn's token was fired with the given source.
    Requested,
    /// No cancellable in-flight turn (idle, unknown agent, or post-terminal).
    NothingToCancel,
}

/// Returned by `Dispatcher::send_message`. The `join` handle owns the drain
/// task; production callers can drop it (the task detaches and keeps running)
/// while tests can `await` it for deterministic assertions.
#[derive(Debug)]
pub struct DispatchHandle {
    pub turn_id: TurnId,
    pub join: JoinHandle<()>,
}

/// The single chokepoint for sending turns to agents. Globally keyed by
/// `AgentId` (which is itself globally unique — UUID v7), so no project
/// context is needed at this layer.
pub struct Dispatcher {
    state: Mutex<HashMap<AgentId, AgentState>>,
}

impl Dispatcher {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Dispatch a turn to `agent`. The lifecycle is:
    ///
    /// 1. Acquire `AgentIdleGuard` under the state lock. If the agent is
    ///    `InFlight`, returns `Err(Busy)` and emits nothing. Acquisition mints
    ///    this turn's `CancellationToken` and stores it in the agent's state.
    /// 2. Generate a fresh `TurnId` (UUID v7).
    /// 3. **Journal the user's send (fail-closed).** This is written *before*
    ///    dispatch: if it fails, the guard drops on early return → `Idle`, and
    ///    `Err(Journal)` is returned with **no subprocess spawned**. Persisting
    ///    the send is a precondition for starting the turn (see
    ///    [`ConversationJournal::record_send`]).
    /// 4. Call `adapter.dispatch(.., options)` with the token on `options`. On
    ///    `Err`, the send was already journaled, so a `Failed { AdapterFailure }`
    ///    outcome marker is recorded for the same `turn_id` (so the turn isn't
    ///    an orphan user message after restart), the guard drops → `Idle`, and
    ///    the dispatch error is returned. No `TurnStart` is emitted.
    /// 5. Emit `TurnStart`, then spawn the drain task with ownership of the
    ///    stream, guard, emitter, token, and journal. The guard drops when the
    ///    drain task ends (terminal event observed, stream truncated/cancelled,
    ///    or task panic — all paths restore `Idle` via Drop).
    ///
    /// Takes `self: &Arc<Self>` because the spawned drain task owns an
    /// `AgentIdleGuard` that holds `Arc<Dispatcher>` — the dispatcher must
    /// outlive the task. Callers must wrap `Dispatcher` in `Arc` before
    /// invoking this method.
    // The per-dispatch inputs (adapter, cwd, emitter, options, journal) are
    // each genuinely per-call and passed explicitly; a wrapper struct here
    // would be a premature abstraction (these get consolidated behind a
    // per-agent dispatch-context factory once queued auto-dispatch needs to
    // reconstruct them). An explicit list reads more clearly until then.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_message(
        self: &Arc<Self>,
        agent: &AgentRecord,
        cwd: &Path,
        prompt: &str,
        adapter: &dyn HarnessAdapter,
        emitter: Arc<dyn EventEmitter>,
        mut options: switchboard_harness::DispatchOptions,
        journal: Arc<dyn ConversationJournal>,
    ) -> Result<DispatchHandle, DispatcherError> {
        let (guard, token) = AgentIdleGuard::acquire(Arc::clone(self), agent.id)?;
        let turn_id: TurnId = Uuid::now_v7();
        let started_at = Utc::now();

        // Durable history begins at turn-start. Journal the user's send
        // *before* spawning the subprocess (fail-closed): if we can't persist
        // it, refuse to start the turn (`?` drops the guard → Idle, no spawn).
        journal.record_send(turn_id, agent.id, prompt, started_at)?;

        // The adapter watches this token (via `select!`) to cancel the turn.
        options.cancel_token = token.clone();
        let stream = match adapter.dispatch(agent, cwd, prompt, turn_id, options).await {
            Ok(stream) => stream,
            Err(dispatch_err) => {
                // The send is already journaled but the turn never started.
                // Record a Failed marker for the same turn so it renders as a
                // failed turn (not an orphan user message) after restart.
                journal.record_outcome(
                    turn_id,
                    agent.id,
                    &TurnOutcome::Failed {
                        kind: FailureKind::AdapterFailure,
                        message: dispatch_err.to_string(),
                    },
                    started_at,
                    Utc::now(),
                );
                return Err(dispatch_err.into());
            }
        };

        let channel = channel_name(agent.id);
        let start = NormalizedEvent::TurnStart {
            turn_id,
            started_at,
        };
        emit_event(emitter.as_ref(), &channel, &start, agent.id);

        let join = tokio::spawn(drain_stream(DrainContext {
            stream,
            channel,
            emitter,
            guard,
            turn_id,
            token,
            journal,
            started_at,
        }));
        Ok(DispatchHandle { turn_id, join })
    }

    /// Request cancellation of `agent`'s in-flight turn, stamping `source` so
    /// the synthesized terminal event can attribute it. A typed no-op
    /// (`NothingToCancel`) when the agent is idle, unknown, or already past
    /// its terminal event — token-presence is the cancellability signal, so a
    /// post-terminal cancel (e.g. during Codex's enrichment window before
    /// `AgentIdle`) is harmless. The adapter, watching the token, performs the
    /// harness-specific kill; the drain task synthesizes
    /// `TurnEnd { Cancelled { source } }`.
    pub fn cancel(&self, agent_id: AgentId, source: CancelSource) -> CancelOutcome {
        let mut state = lock(&self.state);
        let Some(entry) = state.get_mut(&agent_id) else {
            return CancelOutcome::NothingToCancel;
        };
        let Some(token) = entry.cancel_token.as_ref() else {
            return CancelOutcome::NothingToCancel;
        };
        // First-cancel-wins: a later cancel (e.g. shutdown after the user
        // already pressed stop) must not overwrite the original intent. The
        // token fire is idempotent, so re-firing is harmless.
        if entry.cancel_source.is_none() {
            entry.cancel_source = Some(source);
        }
        token.cancel();
        CancelOutcome::Requested
    }

    /// Status of the given agent, or `None` if it has never been dispatched.
    /// Primarily for tests and future telemetry — production flow doesn't
    /// need to inspect agent state directly.
    pub fn agent_status(&self, agent_id: AgentId) -> Option<AgentStatus> {
        lock(&self.state).get(&agent_id).map(|e| e.status)
    }

    /// Resolve once the agent's in-flight turn (if any) has fully drained —
    /// i.e. its drain task has emitted `AgentIdle` and dropped its guard, so
    /// status is `Idle`. Returns immediately if the agent is already idle or
    /// unknown. Used by the cancel-and-drain lifecycle helper so a project
    /// lock is released only after its turns stop driving the harness session.
    pub async fn wait_until_idle(&self, agent_id: AgentId) {
        let notify = {
            let state = lock(&self.state);
            match state.get(&agent_id) {
                Some(entry) if entry.status != AgentStatus::Idle => Arc::clone(&entry.idle_notify),
                _ => return,
            }
        };
        loop {
            // `enable()` registers this waiter *before* the status re-check, so
            // a `notify_waiters()` racing between the check and the await is
            // not lost (the drain task notifies after flipping to Idle).
            let fut = notify.notified();
            tokio::pin!(fut);
            fut.as_mut().enable();
            if self.agent_status(agent_id) != Some(AgentStatus::InFlight) {
                return;
            }
            fut.await;
        }
    }
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII guard for the "agent is `InFlight`" state. Acquired before dispatch;
/// drops at the end of the drain task (or earlier on any failure path) and
/// restores the agent to `Idle`. Uses `std::sync::Mutex` because `Drop` runs
/// synchronously and `tokio::sync::Mutex::lock()` is async.
pub struct AgentIdleGuard {
    dispatcher: Arc<Dispatcher>,
    agent_id: AgentId,
}

impl AgentIdleGuard {
    /// Reserve the agent (`Idle → InFlight`) and mint this turn's
    /// cancellation token, storing it in the agent's state so `cancel` can
    /// reach it. Returns the guard plus a clone of the token (the dispatcher
    /// hands it to the adapter and the drain task).
    fn acquire(
        dispatcher: Arc<Dispatcher>,
        agent_id: AgentId,
    ) -> Result<(Self, CancellationToken), DispatcherError> {
        let mut state = lock(&dispatcher.state);
        let entry = state.entry(agent_id).or_default();
        if entry.status == AgentStatus::InFlight {
            return Err(DispatcherError::Busy);
        }
        let token = CancellationToken::new();
        entry.status = AgentStatus::InFlight;
        entry.cancel_token = Some(token.clone());
        entry.cancel_source = None;
        drop(state);
        Ok((
            Self {
                dispatcher,
                agent_id,
            },
            token,
        ))
    }
}

impl Drop for AgentIdleGuard {
    fn drop(&mut self) {
        let mut state = lock(&self.dispatcher.state);
        if let Some(entry) = state.get_mut(&self.agent_id) {
            entry.status = AgentStatus::Idle;
            // Single teardown site for the turn's cancellation handle — every
            // terminal path (completed / failed / cancelled / truncated /
            // panic) runs Drop, so the token never leaks into the next turn.
            entry.cancel_token = None;
            entry.cancel_source = None;
            // Wake `wait_until_idle` waiters here, in the common teardown, so
            // *every* path that ends a turn notifies — including the ones that
            // never reach the drain task (pre-stream dispatch error, producer
            // panic). Notifying after the `Idle` flip above (under the same
            // lock) means a waiter that re-checks status sees `Idle`.
            entry.idle_notify.notify_waiters();
        }
    }
}

fn channel_name(agent_id: AgentId) -> String {
    format!("agent:{agent_id}")
}

/// Serialize and emit one `NormalizedEvent`. Log-and-skip on the cosmic
/// serialization failure rather than panic the drain task (which would strand
/// the agent `InFlight` until its guard drops) — well-formed events are pure
/// data and never fail to serialize.
fn emit_event(
    emitter: &dyn EventEmitter,
    channel: &str,
    event: &NormalizedEvent,
    agent_id: AgentId,
) {
    match serde_json::to_value(event) {
        Ok(payload) => emitter.emit(channel, payload),
        Err(e) => tracing::error!(
            agent_id = %agent_id,
            error = %e,
            "failed to serialize event — skipping emit (should be unreachable)"
        ),
    }
}

/// Everything the drain task owns for one turn. Bundled into a struct so the
/// `tokio::spawn` call site stays readable as the field set grows.
struct DrainContext {
    stream: EventStream,
    channel: String,
    emitter: Arc<dyn EventEmitter>,
    guard: AgentIdleGuard,
    turn_id: TurnId,
    token: CancellationToken,
    journal: Arc<dyn ConversationJournal>,
    started_at: DateTime<Utc>,
}

/// Drains the adapter event stream, lifting each into a `NormalizedEvent` and
/// emitting it. Holds `AgentIdleGuard` for the lifetime of the task — dropping
/// at function end restores agent state to `Idle` and clears the turn's token.
///
/// Stream-contract ownership splits by path:
/// - **Normal completion / failure:** the **adapter** owns the single terminal
///   `TurnEnd` (it must synthesize `TurnEnd(Failed)` if its subprocess dies
///   silent — see `AGENTS.md`). On observing it, the drain task clears the
///   cancellation token (so a late cancel during post-terminal enrichment is a
///   no-op) and journals the outcome if it is non-completed.
/// - **Cancellation:** the adapter, on cancel, kills its subprocess and ends
///   the stream **without** a terminal event; the **dispatcher** synthesizes
///   `TurnEnd { Cancelled { source } }` here from the source `cancel` recorded.
///   This is the one deliberate exception to adapter-owns-the-terminal — a
///   binary token can't carry intent, so the layer that fired it stamps the
///   outcome, unifying all four harnesses (incl. Codex, which exits 0 and
///   emits nothing on SIGTERM).
///
/// A stream that ends with neither a terminal event nor a fired token is an
/// adapter bug; the drain task logs a warning, still emits `AgentIdle`, and
/// lets the frontend's no-terminal handling take over.
async fn drain_stream(ctx: DrainContext) {
    let DrainContext {
        mut stream,
        channel,
        emitter,
        guard,
        turn_id,
        token,
        journal,
        started_at,
    } = ctx;
    let agent_id = guard.agent_id;
    let mut terminal_seen = false;
    while let Some(event) = stream.next().await {
        if let switchboard_harness::AdapterEvent::TurnEnd {
            outcome, ended_at, ..
        } = &event
        {
            // Cancellation latch: once cancel was requested, the dispatcher
            // owns the terminal. A real terminal that races in afterwards
            // (a result event already buffered when the token fired) is
            // dropped — not emitted, not journaled — so the synthesized
            // `Cancelled` below wins. Without this, a turn the user stopped
            // could still surface as completed/failed (a real M4.3 subprocess
            // race). See decision 2: "any late adapter terminal after cancel
            // is ignored — exactly-one-terminal preserved."
            if token.is_cancelled() {
                continue;
            }
            terminal_seen = true;
            // Clear the token under the state lock so a cancel arriving during
            // the post-terminal enrichment window (TurnEnd → … → AgentIdle) is
            // a typed no-op rather than a spurious synthesized Cancelled.
            clear_token(&guard.dispatcher, agent_id);
            // Non-completed outcomes (failed) are journaled so they survive
            // restart as markers; a completed turn's content comes from the
            // harness session file, so it gets no outcome record.
            if !matches!(outcome, TurnOutcome::Completed) {
                journal.record_outcome(turn_id, agent_id, outcome, started_at, *ended_at);
            }
        }
        let normalized: NormalizedEvent = event.into();
        emit_event(emitter.as_ref(), &channel, &normalized, agent_id);
    }

    if !terminal_seen && token.is_cancelled() {
        // Adapter ended the stream on cancel without a terminal event — the
        // dispatcher owns the cancelled terminal. `cancel` always records the
        // source before firing the token (under the state lock), so by the time
        // the token reads cancelled here the source is set; `unwrap_or` is for
        // the structurally-unreachable None, never a meaningful default.
        let source = cancel_source(&guard.dispatcher, agent_id).unwrap_or(CancelSource::User);
        let ended_at = Utc::now();
        let outcome = TurnOutcome::Cancelled { source };
        let synthesized = NormalizedEvent::TurnEnd {
            turn_id,
            outcome: outcome.clone(),
            ended_at,
            usage: None,
        };
        emit_event(emitter.as_ref(), &channel, &synthesized, agent_id);
        journal.record_outcome(turn_id, agent_id, &outcome, started_at, ended_at);
        terminal_seen = true;
    }

    if !terminal_seen {
        tracing::warn!(
            agent_id = %agent_id,
            channel = %channel,
            "drain_stream observed stream end without a terminal TurnEnd event — adapter contract violation; agent state restored to Idle"
        );
    }

    // Emit `AgentIdle` as the last event on the per-agent channel for this
    // dispatch, BEFORE the guard drops. Two invariants this ordering
    // upholds for the frontend (see `NormalizedEvent::AgentIdle` doc):
    // (1) channel-ordering — no further events arrive on this channel for
    //     this dispatch (we're past the stream-drain loop);
    // (2) sendability — by the time the frontend's IPC handler runs this
    //     event, the guard drop below has executed (the emit is
    //     fire-and-forget; the drop is synchronous), so a fresh send
    //     observes `Idle`.
    // Emitted unconditionally — even on the no-terminal-event path, the
    // agent is genuinely idle once the stream drains, and the frontend
    // needs the signal to re-enable Send for retry.
    let idle = NormalizedEvent::AgentIdle { agent_id };
    emit_event(emitter.as_ref(), &channel, &idle, agent_id);
    // `guard` drops here → agent returns to Idle, token/source clear, and
    // `wait_until_idle` waiters are notified — all in `AgentIdleGuard::Drop`.
    drop(guard);
}

/// Clear the in-flight turn's cancellation token (called when a terminal
/// event is observed, so a subsequent `cancel` no-ops).
fn clear_token(dispatcher: &Dispatcher, agent_id: AgentId) {
    if let Some(entry) = lock(&dispatcher.state).get_mut(&agent_id) {
        entry.cancel_token = None;
    }
}

/// Read the source a `cancel` recorded for the agent's in-flight turn.
fn cancel_source(dispatcher: &Dispatcher, agent_id: AgentId) -> Option<CancelSource> {
    lock(&dispatcher.state)
        .get(&agent_id)
        .and_then(|e| e.cancel_source)
}

/// Recover from `Mutex` poisoning. Poisoning would mean a previous holder
/// panicked with the lock held; for this dispatcher the only holders are
/// `AgentIdleGuard::acquire` and `Drop`, neither of which can panic, so this
/// is defensive only.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// End-to-end behavior is exercised from
// `crates/dispatcher/tests/dispatcher_with_mock.rs` (a Cargo integration test
// that compiles against this crate as an external consumer). No `#[cfg(test)]`
// unit-test module here — there are no genuinely private helpers worth
// testing in isolation; `lock()` is a one-line poison-recovery wrapper
// implicitly exercised by every dispatch.
