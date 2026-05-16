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

use chrono::Utc;
use futures::StreamExt;
use switchboard_core::{AgentId, AgentRecord};
use switchboard_harness::{DispatchError, EventStream, HarnessAdapter, NormalizedEvent, TurnId};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Per-agent dispatch state. Two states are enough for M1 — M4 may extend
/// this with structured contention reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentStatus {
    Idle,
    InFlight,
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
}

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
    state: Mutex<HashMap<AgentId, AgentStatus>>,
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
    ///    `InFlight`, returns `Err(Busy)` and emits nothing.
    /// 2. Generate a fresh `TurnId` (UUID v7).
    /// 3. Call `adapter.dispatch(.., turn_id)`. On `Err`, the guard drops on
    ///    early return → state restored to `Idle` via RAII. No `TurnStart` is
    ///    emitted — the wire stays clean.
    /// 4. Emit `TurnStart` with the same `turn_id`.
    /// 5. Spawn the drain task with ownership of the stream, guard, and
    ///    emitter. Each `AdapterEvent` lifts to `NormalizedEvent`; the guard
    ///    drops when the drain task ends (terminal event observed, stream
    ///    truncated, or task panic — all paths restore `Idle` via Drop).
    ///
    /// Takes `self: &Arc<Self>` because the spawned drain task owns an
    /// `AgentIdleGuard` that holds `Arc<Dispatcher>` — the dispatcher must
    /// outlive the task. Callers must wrap `Dispatcher` in `Arc` before
    /// invoking this method.
    pub async fn send_message(
        self: &Arc<Self>,
        agent: &AgentRecord,
        cwd: &Path,
        prompt: &str,
        adapter: &dyn HarnessAdapter,
        emitter: Arc<dyn EventEmitter>,
        options: switchboard_harness::DispatchOptions,
    ) -> Result<DispatchHandle, DispatcherError> {
        let guard = AgentIdleGuard::acquire(Arc::clone(self), agent.id)?;
        let turn_id: TurnId = Uuid::now_v7();
        let stream = adapter
            .dispatch(agent, cwd, prompt, turn_id, options)
            .await?;

        let channel = channel_name(agent.id);
        let start = NormalizedEvent::TurnStart {
            turn_id,
            started_at: Utc::now(),
        };
        // Serialization of a well-formed NormalizedEvent is infallible
        // (pure data, no IO, no custom serializers that can fail), but
        // panicking inside the drain task on the cosmic edge case would
        // leave the agent stuck in `InFlight` until the guard drops.
        // Log-and-skip is safer.
        match serde_json::to_value(&start) {
            Ok(payload) => emitter.emit(&channel, payload),
            Err(e) => {
                tracing::error!(
                    %turn_id,
                    error = %e,
                    "failed to serialize TurnStart — skipping emit (should be unreachable)"
                );
            }
        }

        let join = tokio::spawn(drain_stream(stream, channel, emitter, guard));
        Ok(DispatchHandle { turn_id, join })
    }

    /// Status of the given agent, or `None` if it has never been dispatched.
    /// Primarily for tests and future telemetry — production flow doesn't
    /// need to inspect agent state directly.
    pub fn agent_status(&self, agent_id: AgentId) -> Option<AgentStatus> {
        lock(&self.state).get(&agent_id).copied()
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
    fn acquire(dispatcher: Arc<Dispatcher>, agent_id: AgentId) -> Result<Self, DispatcherError> {
        let mut state = lock(&dispatcher.state);
        let entry = state.entry(agent_id).or_insert(AgentStatus::Idle);
        if *entry == AgentStatus::InFlight {
            return Err(DispatcherError::Busy);
        }
        *entry = AgentStatus::InFlight;
        drop(state);
        Ok(Self {
            dispatcher,
            agent_id,
        })
    }
}

impl Drop for AgentIdleGuard {
    fn drop(&mut self) {
        let mut state = lock(&self.dispatcher.state);
        if let Some(entry) = state.get_mut(&self.agent_id) {
            *entry = AgentStatus::Idle;
        }
    }
}

fn channel_name(agent_id: AgentId) -> String {
    format!("agent:{agent_id}")
}

/// Drains the adapter event stream, lifting each into a `NormalizedEvent`
/// and emitting it. Holds `AgentIdleGuard` for the lifetime of the task —
/// dropping at function end restores agent state to `Idle`.
///
/// Stream-contract ownership: the **adapter** is responsible for ensuring
/// exactly one terminal `TurnEnd` per turn. The dispatcher trusts that
/// contract and does not synthesize a fallback — single ownership is the
/// design (see `AGENTS.md`, M1.3 stream contract + M1.4 dispatcher rules).
///
/// If a terminal event is missing on stream close, that is an adapter bug
/// (the upstream subprocess died silent AND the adapter failed to
/// synthesize `TurnEnd(Failed)` per M1.3 step 7). The dispatcher's only
/// response is to log a warning so the regression is visible, restore
/// agent state, and let the failure surface to the M1.5 frontend reducer
/// (which is responsible for handling "no terminal event observed within
/// N seconds" as an error state).
async fn drain_stream(
    mut stream: EventStream,
    channel: String,
    emitter: Arc<dyn EventEmitter>,
    guard: AgentIdleGuard,
) {
    let agent_id = guard.agent_id;
    let mut terminal_seen = false;
    while let Some(event) = stream.next().await {
        if matches!(event, switchboard_harness::AdapterEvent::TurnEnd { .. }) {
            terminal_seen = true;
        }
        let normalized: NormalizedEvent = event.into();
        // Log-and-skip on cosmic serialization failures rather than panic
        // the drain task — see `send_message` for rationale.
        match serde_json::to_value(&normalized) {
            Ok(payload) => emitter.emit(&channel, payload),
            Err(e) => {
                tracing::error!(
                    agent_id = %agent_id,
                    error = %e,
                    "failed to serialize NormalizedEvent — skipping emit (should be unreachable)"
                );
            }
        }
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
    match serde_json::to_value(&idle) {
        Ok(payload) => emitter.emit(&channel, payload),
        Err(e) => {
            tracing::error!(
                agent_id = %agent_id,
                error = %e,
                "failed to serialize AgentIdle — skipping emit (should be unreachable)"
            );
        }
    }
    // `guard` drops here → agent returns to Idle via RAII.
    drop(guard);
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
