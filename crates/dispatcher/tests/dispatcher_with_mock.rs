//! End-to-end dispatcher behavior, exercised through the public API as an
//! external consumer would. `MockHarnessAdapter` stands in for a real harness
//! so these run hermetically in `make check` — no subprocess, no real CLI.
//!
//! Compiling as an integration test (`tests/<file>.rs`) means the dispatcher
//! crate is linked here the same way a downstream consumer would link it.
//! If any of these tests accidentally relied on a private item, the compile
//! would fail — a useful external-consumer-contract check on the public
//! surface.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_dispatcher::{
    AgentStatus, Dispatcher, DispatcherError, EventEmitter, RecordingEmitter,
};
use switchboard_harness::{DispatchOptions, MockHarnessAdapter, MockScenario, TurnId};
use uuid::Uuid;

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

/// Trait-object coercion for `Arc<RecordingEmitter>` → `Arc<dyn EventEmitter>`.
/// Pure visual noise reduction — the explicit `as` cast is required at every
/// `send_message` call site because Rust won't infer the unsizing coercion
/// across the trait-object boundary in a generic context.
fn as_emitter(e: &Arc<RecordingEmitter>) -> Arc<dyn EventEmitter> {
    Arc::clone(e) as Arc<dyn EventEmitter>
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
            as_emitter(&emitter),
            DispatchOptions::default(),
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
            as_emitter(&emitter),
            DispatchOptions::default(),
        )
        .await
        .unwrap();
    handle.join.await.unwrap();

    let events = emitter.snapshot();
    // MockScenario::Streaming → 3 chunks + TurnEnd, plus dispatcher's
    // TurnStart and dispatcher's AgentIdle = 6.
    assert_eq!(events.len(), 6);
    assert_eq!(event_type(&events[0].1), "turn_start");
    assert_eq!(event_type(&events[1].1), "content_chunk");
    assert_eq!(event_type(&events[2].1), "content_chunk");
    assert_eq!(event_type(&events[3].1), "content_chunk");
    assert_eq!(event_type(&events[4].1), "turn_end");
    assert_eq!(event_type(&events[5].1), "agent_idle");

    let expected_channel = format!("agent:{}", agent.id);
    for (name, _) in &events {
        assert_eq!(name, &expected_channel);
    }

    // turn_id is consistent across the whole sequence — across turn-scoped
    // variants only. AgentIdle is agent-scoped and carries `agent_id`
    // instead of `turn_id`, so it's excluded from this check.
    let turn_id = extract_turn_id(&events[0].1);
    assert_eq!(turn_id, handle.turn_id);
    for (_, payload) in &events {
        if event_type(payload) == "agent_idle" {
            continue;
        }
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
            as_emitter(&emitter),
            DispatchOptions::default(),
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
            as_emitter(&emitter),
            DispatchOptions::default(),
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
            as_emitter(&emitter),
            DispatchOptions::default(),
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
            as_emitter(&emitter),
            DispatchOptions::default(),
        )
        .await
        .unwrap();
    let handle_b = dispatcher
        .send_message(
            &agent_b,
            Path::new("/tmp/project-b"),
            "B's prompt",
            &adapter,
            as_emitter(&emitter),
            DispatchOptions::default(),
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
    // Per channel: TurnStart + 3 ContentChunks + TurnEnd + AgentIdle = 6.
    assert_eq!(a_count, 6);
    assert_eq!(b_count, 6);
    // Total = sum of per-channel counts — proves no events leaked to a
    // phantom third channel (which would be silently allowed by the
    // per-channel asserts alone).
    assert_eq!(events.len(), 12);
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
            as_emitter(&emitter),
            DispatchOptions::default(),
        )
        .await
        .unwrap();

    // The drain task itself doesn't panic; it observes the producer's
    // sender dropping (panic kills the producer task) and exits cleanly.
    // AgentIdleGuard's Drop restores state to Idle either way.
    handle.join.await.unwrap();
    assert_eq!(dispatcher.agent_status(agent.id), Some(AgentStatus::Idle));

    // Confirm the guard was actually held during dispatch (not bypassed).
    // TurnStart is emitted synchronously inside `send_message` only after
    // `AgentIdleGuard::acquire` succeeds — its presence proves the guard
    // path was taken before the producer panicked. Asserting only the
    // end-state Idle would still pass a hypothetical regression where
    // the guard was never acquired (no state to restore).
    let events = emitter.snapshot();
    assert!(
        events
            .iter()
            .any(|(_, payload)| event_type(payload) == "turn_start"),
        "TurnStart should have been emitted before the producer panicked, \
         confirming the guard was acquired"
    );
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
    // not okay — it's a visible bug that the frontend reducer must handle
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
            as_emitter(&emitter),
            DispatchOptions::default(),
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

#[tokio::test]
async fn dispatch_failure_emits_no_turn_start_and_leaves_agent_idle() {
    // Per the M1.4 invariant: when `adapter.dispatch()` returns Err,
    // `send_message` lifts it to `DispatcherError::Dispatch`, the
    // `AgentIdleGuard` drops on early return, and NO `TurnStart` is
    // emitted (the wire stays clean — consumers see the error from
    // `send_message`, never a half-stream). This is the "pre-stream
    // failure" path, distinct from the "stream established then broke"
    // paths covered by Panic / TruncatedStream above.
    let dispatcher = Arc::new(Dispatcher::new());
    let adapter = MockHarnessAdapter::with_scenario(MockScenario::DispatchFails);
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();

    let result = dispatcher
        .send_message(
            &agent,
            Path::new("/tmp/project"),
            "won't dispatch",
            &adapter,
            as_emitter(&emitter),
            DispatchOptions::default(),
        )
        .await;

    // Error lifts through the From impl: DispatchError::BinaryNotFound
    // → DispatcherError::Dispatch.
    assert!(
        matches!(result, Err(DispatcherError::Dispatch(_))),
        "expected DispatcherError::Dispatch, got: {result:?}"
    );

    // No events emitted — the wire stays clean.
    assert!(
        emitter.is_empty(),
        "dispatch failure must not emit TurnStart or anything else; got: {:?}",
        emitter.snapshot()
    );

    // Agent state: the guard acquired Idle → InFlight, then dropped on
    // early return → restored to Idle. So agent_status returns Some(Idle)
    // (the guard touched the map, leaving an entry behind), NOT None.
    assert_eq!(
        dispatcher.agent_status(agent.id),
        Some(AgentStatus::Idle),
        "AgentIdleGuard must restore state to Idle on early return"
    );

    // And the next send_message must succeed — the agent isn't stuck Busy.
    let healthy_adapter = MockHarnessAdapter::new();
    let handle = dispatcher
        .send_message(
            &agent,
            Path::new("/tmp/project"),
            "now works",
            &healthy_adapter,
            as_emitter(&emitter),
            DispatchOptions::default(),
        )
        .await
        .unwrap();
    handle.join.await.unwrap();
    assert_eq!(dispatcher.agent_status(agent.id), Some(AgentStatus::Idle));
}

#[tokio::test]
async fn agent_idle_is_last_event_and_unblocks_next_send() {
    // Pins two load-bearing contracts of NormalizedEvent::AgentIdle (see
    // `crates/harness/src/events.rs`):
    //
    //   (1) Channel-ordering: AgentIdle is the LAST event on the per-agent
    //       channel for a dispatch — nothing follows it.
    //   (2) Sendability: when the frontend processes AgentIdle, a fresh
    //       send to the same agent succeeds without returning Busy. (The
    //       dispatcher's guard drop happens after emit, but
    //       fire-and-forget IPC means by the time the consumer reacts,
    //       the drop has executed.)
    //
    // A regression that emitted AgentIdle out of order — or omitted it —
    // would surface here as either (1) the last-event check failing or
    // (2) the second send returning Busy.
    let dispatcher = Arc::new(Dispatcher::new());
    let adapter = MockHarnessAdapter::new();
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();

    let handle = dispatcher
        .send_message(
            &agent,
            Path::new("/tmp/project"),
            "first",
            &adapter,
            as_emitter(&emitter),
            DispatchOptions::default(),
        )
        .await
        .unwrap();
    handle.join.await.unwrap();

    let events = emitter.snapshot();
    // Contract (1): last event on the channel is agent_idle.
    let last = events.last().expect("at least one event emitted");
    assert_eq!(
        event_type(&last.1),
        "agent_idle",
        "AgentIdle must be the last event on the per-agent channel; got {:?}",
        events
            .iter()
            .map(|(_, v)| event_type(v))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        last.1["agent_id"].as_str().unwrap(),
        agent.id.to_string(),
        "AgentIdle agent_id matches the dispatched agent"
    );
    // AgentIdle is also the ONLY agent_idle in the sequence.
    let agent_idle_count = events
        .iter()
        .filter(|(_, v)| event_type(v) == "agent_idle")
        .count();
    assert_eq!(agent_idle_count, 1, "exactly one AgentIdle per dispatch");

    // Contract (2): a fresh send to the same agent succeeds — no Busy.
    let handle2 = dispatcher
        .send_message(
            &agent,
            Path::new("/tmp/project"),
            "second",
            &adapter,
            as_emitter(&emitter),
            DispatchOptions::default(),
        )
        .await
        .expect("second send must not return Busy after AgentIdle");
    handle2.join.await.unwrap();
}
