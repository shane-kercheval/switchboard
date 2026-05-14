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
    ) -> Result<DispatchHandle, DispatcherError> {
        let guard = AgentIdleGuard::acquire(Arc::clone(self), agent.id)?;
        let turn_id: TurnId = Uuid::now_v7();
        let stream = adapter.dispatch(agent, cwd, prompt, turn_id).await?;

        let channel = channel_name(agent.id);
        let start = NormalizedEvent::TurnStart {
            turn_id,
            started_at: Utc::now(),
        };
        emitter.emit(
            &channel,
            serde_json::to_value(&start).expect("NormalizedEvent serialization is infallible"),
        );

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
        let payload =
            serde_json::to_value(&normalized).expect("NormalizedEvent serialization is infallible");
        emitter.emit(&channel, payload);
    }
    if !terminal_seen {
        tracing::warn!(
            agent_id = %agent_id,
            channel = %channel,
            "drain_stream observed stream end without a terminal TurnEnd event — adapter contract violation; agent state restored to Idle"
        );
    }
    // `guard` drops here regardless → agent returns to Idle via RAII.
    drop(guard);
}

/// Recover from `Mutex` poisoning. Poisoning would mean a previous holder
/// panicked with the lock held; for this dispatcher the only holders are
/// `AgentIdleGuard::acquire` and `Drop`, neither of which can panic, so this
/// is defensive only.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::DateTime;
    use switchboard_core::HarnessKind;
    use switchboard_harness::{MockHarnessAdapter, MockScenario};

    fn agent_record() -> AgentRecord {
        AgentRecord {
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            name: "test-agent".to_owned(),
            harness: HarnessKind::ClaudeCode,
            session_id: Some(Uuid::now_v7()),
            created_at: Utc::now(),
        }
    }

    fn event_type(value: &serde_json::Value) -> &str {
        value["type"].as_str().expect("event has type tag")
    }

    fn extract_turn_id(value: &serde_json::Value) -> TurnId {
        let s = value["turn_id"].as_str().expect("event has turn_id");
        Uuid::parse_str(s).expect("turn_id parses as UUID")
    }

    #[tokio::test]
    async fn send_message_idle_then_inflight_then_idle() {
        let dispatcher = Arc::new(Dispatcher::new());
        let adapter = MockHarnessAdapter::new();
        let emitter = Arc::new(RecordingEmitter::new());
        let agent = agent_record();

        assert_eq!(dispatcher.agent_status(agent.id), None);
        let handle = dispatcher
            .send_message(
                &agent,
                Path::new("/tmp/project"),
                "hello",
                &adapter,
                Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            )
            .await
            .unwrap();

        assert_eq!(
            dispatcher.agent_status(agent.id),
            Some(AgentStatus::InFlight),
            "agent should be InFlight while drain task runs"
        );

        handle.join.await.unwrap();

        assert_eq!(
            dispatcher.agent_status(agent.id),
            Some(AgentStatus::Idle),
            "agent should return to Idle after drain"
        );
    }

    #[tokio::test]
    async fn send_message_emits_turn_start_before_content_chunks() {
        let dispatcher = Arc::new(Dispatcher::new());
        let adapter = MockHarnessAdapter::new();
        let emitter = Arc::new(RecordingEmitter::new());
        let agent = agent_record();

        let handle = dispatcher
            .send_message(
                &agent,
                Path::new("/tmp/project"),
                "hello",
                &adapter,
                Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            )
            .await
            .unwrap();
        handle.join.await.unwrap();

        let events = emitter.snapshot();
        // MockScenario::Streaming → 3 chunks + TurnEnd, plus dispatcher's TurnStart = 5.
        assert_eq!(events.len(), 5);
        assert_eq!(event_type(&events[0].1), "turn_start");
        assert_eq!(event_type(&events[1].1), "content_chunk");
        assert_eq!(event_type(&events[2].1), "content_chunk");
        assert_eq!(event_type(&events[3].1), "content_chunk");
        assert_eq!(event_type(&events[4].1), "turn_end");

        let expected_channel = format!("agent:{}", agent.id);
        for (name, _) in &events {
            assert_eq!(name, &expected_channel);
        }

        // turn_id is consistent across the whole sequence.
        let turn_id = extract_turn_id(&events[0].1);
        assert_eq!(turn_id, handle.turn_id);
        for (_, payload) in &events {
            assert_eq!(extract_turn_id(payload), turn_id);
        }

        // started_at is a parseable RFC3339 timestamp.
        let started_at = events[0].1["started_at"].as_str().expect("started_at");
        assert!(DateTime::parse_from_rfc3339(started_at).is_ok());
    }

    #[tokio::test]
    async fn concurrent_send_to_same_agent_returns_busy() {
        let dispatcher = Arc::new(Dispatcher::new());
        let adapter = MockHarnessAdapter::new();
        let emitter = Arc::new(RecordingEmitter::new());
        let agent = agent_record();

        let handle1 = dispatcher
            .send_message(
                &agent,
                Path::new("/tmp/project"),
                "first",
                &adapter,
                Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            )
            .await
            .unwrap();

        // Second call while first is in flight → Busy.
        let result = dispatcher
            .send_message(
                &agent,
                Path::new("/tmp/project"),
                "second",
                &adapter,
                Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            )
            .await;
        assert!(matches!(result, Err(DispatcherError::Busy)));

        handle1.join.await.unwrap();

        // After first completes, a fresh send should succeed.
        let handle3 = dispatcher
            .send_message(
                &agent,
                Path::new("/tmp/project"),
                "third",
                &adapter,
                Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            )
            .await
            .unwrap();
        handle3.join.await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_send_to_different_agents_both_succeed() {
        let dispatcher = Arc::new(Dispatcher::new());
        let adapter = MockHarnessAdapter::new();
        let emitter = Arc::new(RecordingEmitter::new());
        let agent_a = agent_record();
        let agent_b = agent_record();

        let handle_a = dispatcher
            .send_message(
                &agent_a,
                Path::new("/tmp/project-a"),
                "A's prompt",
                &adapter,
                Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            )
            .await
            .unwrap();
        let handle_b = dispatcher
            .send_message(
                &agent_b,
                Path::new("/tmp/project-b"),
                "B's prompt",
                &adapter,
                Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            )
            .await
            .unwrap();

        handle_a.join.await.unwrap();
        handle_b.join.await.unwrap();

        assert_eq!(dispatcher.agent_status(agent_a.id), Some(AgentStatus::Idle));
        assert_eq!(dispatcher.agent_status(agent_b.id), Some(AgentStatus::Idle));

        // Events on each agent's channel — no cross-contamination.
        let channel_a = format!("agent:{}", agent_a.id);
        let channel_b = format!("agent:{}", agent_b.id);
        let events = emitter.snapshot();
        let a_count = events.iter().filter(|(n, _)| n == &channel_a).count();
        let b_count = events.iter().filter(|(n, _)| n == &channel_b).count();
        assert_eq!(a_count, 5);
        assert_eq!(b_count, 5);
    }

    #[tokio::test]
    async fn panic_in_producer_still_restores_agent_to_idle() {
        let dispatcher = Arc::new(Dispatcher::new());
        let adapter = MockHarnessAdapter::with_scenario(MockScenario::Panic);
        let emitter = Arc::new(RecordingEmitter::new());
        let agent = agent_record();

        let handle = dispatcher
            .send_message(
                &agent,
                Path::new("/tmp/project"),
                "will panic",
                &adapter,
                Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            )
            .await
            .unwrap();

        // The drain task itself doesn't panic; it observes the producer's
        // sender dropping (panic kills the producer task) and exits cleanly.
        // AgentIdleGuard's Drop restores state to Idle either way.
        handle.join.await.unwrap();
        assert_eq!(dispatcher.agent_status(agent.id), Some(AgentStatus::Idle));
    }

    #[tokio::test]
    async fn truncated_stream_without_turn_end_returns_to_idle() {
        // Fault-injection: MockScenario::TruncatedStream emits chunks and
        // drops the sender without TurnEnd — a deliberate contract
        // violation, NOT acceptable real-world behaviour. Real adapters
        // (e.g., ClaudeCodeAdapter, M1.3 step 7) synthesize TurnEnd(Failed)
        // on truncation; the dispatcher trusts that and does not repair.
        //
        // This test asserts only the dispatcher's state-recovery half of
        // the picture: agent returns to Idle even when the adapter
        // misbehaves. The "no terminal event reaches the wire" outcome is
        // not okay — it's a visible bug that the M1.5 reducer must handle
        // (timeout / error state) and any adapter bug surfacing this in
        // production is itself a bug to fix at the adapter layer.
        let dispatcher = Arc::new(Dispatcher::new());
        let adapter = MockHarnessAdapter::with_scenario(MockScenario::TruncatedStream);
        let emitter = Arc::new(RecordingEmitter::new());
        let agent = agent_record();

        let handle = dispatcher
            .send_message(
                &agent,
                Path::new("/tmp/project"),
                "prompt",
                &adapter,
                Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            )
            .await
            .unwrap();
        handle.join.await.unwrap();

        let events = emitter.snapshot();
        let kinds: Vec<&str> = events.iter().map(|(_, v)| event_type(v)).collect();
        assert_eq!(kinds.first().copied(), Some("turn_start"));
        assert!(
            !kinds.contains(&"turn_end"),
            "TruncatedStream emits no terminal event — got {kinds:?}"
        );
        assert_eq!(dispatcher.agent_status(agent.id), Some(AgentStatus::Idle));
    }
}
