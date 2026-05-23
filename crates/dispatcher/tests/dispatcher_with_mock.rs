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
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use switchboard_core::{AgentId, AgentRecord, HarnessKind};
use switchboard_dispatcher::{
    AgentStatus, CancelOutcome, ConversationJournal, Dispatcher, DispatcherError, EventEmitter,
    JournalError, NoopJournal, RecordingEmitter,
};
use switchboard_harness::{
    CancelSource, DispatchOptions, MockHarnessAdapter, MockScenario, TurnId, TurnOutcome,
};
use uuid::Uuid;

/// These tests don't assert on journaling — the journal is exercised directly
/// in the journal-specific tests below and in core. A no-op keeps the
/// `send_message` call sites focused on dispatch/emit behavior.
fn noop_journal() -> Arc<dyn ConversationJournal> {
    Arc::new(NoopJournal)
}

/// Captures journal calls so tests can assert the send/outcome partition
/// (send at turn-start for every turn; outcome only for non-completed turns).
#[derive(Default)]
struct RecordingJournal {
    sends: Mutex<Vec<(TurnId, String)>>,
    outcomes: Mutex<Vec<(TurnId, TurnOutcome)>>,
}

impl ConversationJournal for RecordingJournal {
    fn record_send(
        &self,
        turn_id: TurnId,
        _agent_id: AgentId,
        prompt: &str,
        _at: DateTime<Utc>,
    ) -> Result<(), JournalError> {
        self.sends
            .lock()
            .unwrap()
            .push((turn_id, prompt.to_owned()));
        Ok(())
    }
    fn record_outcome(
        &self,
        turn_id: TurnId,
        _agent_id: AgentId,
        outcome: &TurnOutcome,
        _started_at: DateTime<Utc>,
        _ended_at: DateTime<Utc>,
    ) {
        self.outcomes
            .lock()
            .unwrap()
            .push((turn_id, outcome.clone()));
    }
}

/// A journal whose fail-closed `record_send` always errors — used to assert
/// the dispatcher refuses to start a turn it can't persist.
struct FailingJournal;

impl ConversationJournal for FailingJournal {
    fn record_send(
        &self,
        _turn_id: TurnId,
        _agent_id: AgentId,
        _prompt: &str,
        _at: DateTime<Utc>,
    ) -> Result<(), JournalError> {
        Err(JournalError("disk on fire".into()))
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
            noop_journal(),
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
            noop_journal(),
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
            noop_journal(),
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
            noop_journal(),
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
            noop_journal(),
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
            noop_journal(),
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
            noop_journal(),
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
            noop_journal(),
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
    // (e.g., ClaudeCodeAdapter) synthesize TurnEnd(Failed) on truncation;
    // the dispatcher trusts that and does not repair.
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
            noop_journal(),
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
    // Pre-stream-failure invariant: when `adapter.dispatch()` returns
    // Err, `send_message` lifts it to `DispatcherError::Dispatch`, the
    // `AgentIdleGuard` drops on early return, and NO `TurnStart` is
    // emitted (the wire stays clean — consumers see the error from
    // `send_message`, never a half-stream). Distinct from the
    // "stream established then broke" paths covered by Panic /
    // TruncatedStream above.
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
            noop_journal(),
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
            noop_journal(),
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
            noop_journal(),
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
            noop_journal(),
        )
        .await
        .expect("second send must not return Busy after AgentIdle");
    handle2.join.await.unwrap();
}

#[tokio::test]
async fn agent_idle_is_last_after_codex_post_terminal_enrichment_sequence() {
    // Pins the load-bearing ordering invariant for Codex's post-terminal
    // enrichment: the dispatcher's drain task must preserve the adapter's
    // event order `TurnEnd → RateLimitEvent → SessionMeta` and emit
    // `AgentIdle` strictly after all of them.
    //
    // Why this matters: AgentIdle is the sole sendability signal on the
    // frontend (`AGENTS.md` stream contract; `AgentRuntime.run_status`
    // state machine). If the dispatcher accidentally emitted AgentIdle
    // mid-sequence — e.g., promptly on `TurnEnd` rather than on
    // `AgentIdleGuard` drop after the drain loop drains the channel —
    // the compose-bar would re-enable while RateLimitEvent / SessionMeta
    // were still racing through IPC. A user sending immediately would
    // either get a `Busy` from the dispatcher (worst case) or, more
    // subtly, see sidebar metadata update *after* the next turn's
    // user message appeared — a confusing inversion the user would read
    // as a bug.
    //
    // The existing `agent_idle_is_last_event_and_unblocks_next_send` test
    // exercises only the `ContentChunk → TurnEnd` path. This one extends
    // coverage to the Codex shape, which is the path with the most
    // events between TurnEnd and AgentIdle and thus the most chances for
    // a re-ordering regression to land undetected.
    let dispatcher = Arc::new(Dispatcher::new());
    let adapter = MockHarnessAdapter::with_scenario(MockScenario::CodexPostTerminalEnrichment);
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
            noop_journal(),
        )
        .await
        .unwrap();
    handle.join.await.unwrap();

    let events = emitter.snapshot();
    let type_sequence: Vec<&str> = events.iter().map(|(_, v)| event_type(v)).collect();

    // The dispatcher synthesizes TurnStart at the front and AgentIdle at
    // the back; everything in between is the adapter's stream in arrival
    // order. The exact expected sequence:
    assert_eq!(
        type_sequence,
        vec![
            "turn_start",
            "content_chunk",
            "turn_end",
            "rate_limit_event",
            "session_meta",
            "agent_idle",
        ],
        "post-terminal events must preserve adapter order, and AgentIdle must come after all of them"
    );

    // Tighten the contract: the index of agent_idle is the LAST index. A
    // regression that appended a stray event after AgentIdle would pass
    // the sequence check above but fail this.
    let last_idx = type_sequence
        .iter()
        .rposition(|t| *t == "agent_idle")
        .unwrap();
    assert_eq!(
        last_idx,
        type_sequence.len() - 1,
        "AgentIdle must be strictly the final event — no trailing events allowed"
    );
}

#[tokio::test]
async fn cancel_in_flight_synthesizes_cancelled_then_idle_and_unblocks_next_send() {
    // The adapter (mock) cooperates: it parks until the token fires, then
    // ends its stream WITHOUT a terminal event (mirroring a real adapter's
    // cancel path). The dispatcher must synthesize TurnEnd { Cancelled }.
    let dispatcher = Arc::new(Dispatcher::new());
    let adapter = MockHarnessAdapter::with_scenario(MockScenario::AwaitCancellation);
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();

    let handle = dispatcher
        .send_message(
            &agent,
            Path::new("/tmp/project"),
            "long task",
            &adapter,
            as_emitter(&emitter),
            DispatchOptions::default(),
            noop_journal(),
        )
        .await
        .unwrap();

    assert_eq!(
        dispatcher.cancel(agent.id, CancelSource::User),
        CancelOutcome::Requested,
        "cancelling an in-flight turn requests cancellation"
    );
    handle.join.await.unwrap();

    let events = emitter.snapshot();
    let types: Vec<&str> = events.iter().map(|(_, v)| event_type(v)).collect();
    assert_eq!(
        types,
        vec!["turn_start", "content_chunk", "turn_end", "agent_idle"],
        "dispatcher synthesizes the cancelled terminal then AgentIdle"
    );
    let terminal = &events[2].1;
    assert_eq!(terminal["outcome"]["status"], "cancelled");
    assert_eq!(
        terminal["outcome"]["source"], "user",
        "the dispatcher stamps the source it was cancelled with"
    );
    assert_eq!(dispatcher.agent_status(agent.id), Some(AgentStatus::Idle));

    // Re-promptable immediately: a fresh send is not refused as Busy.
    let streaming = MockHarnessAdapter::new();
    let next = dispatcher
        .send_message(
            &agent,
            Path::new("/tmp/project"),
            "next",
            &streaming,
            as_emitter(&emitter),
            DispatchOptions::default(),
            noop_journal(),
        )
        .await
        .expect("agent is re-promptable after cancellation");
    next.join.await.unwrap();
    assert_eq!(dispatcher.agent_status(agent.id), Some(AgentStatus::Idle));
}

#[tokio::test]
async fn cancel_when_not_in_flight_is_a_typed_no_op() {
    let dispatcher = Arc::new(Dispatcher::new());
    let agent = agent_record();
    // Never dispatched.
    assert_eq!(
        dispatcher.cancel(agent.id, CancelSource::User),
        CancelOutcome::NothingToCancel
    );
}

#[tokio::test]
async fn cancel_after_terminal_is_a_no_op_and_emits_no_extra_cancelled() {
    // A completed turn clears its token on the terminal event, so a cancel
    // arriving afterwards (the post-terminal enrichment window, or just late)
    // is a typed no-op — no synthesized Cancelled, the turn stays completed.
    let dispatcher = Arc::new(Dispatcher::new());
    let adapter = MockHarnessAdapter::new(); // Streaming → completes
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();

    let handle = dispatcher
        .send_message(
            &agent,
            Path::new("/tmp/project"),
            "hi",
            &adapter,
            as_emitter(&emitter),
            DispatchOptions::default(),
            noop_journal(),
        )
        .await
        .unwrap();
    handle.join.await.unwrap();

    assert_eq!(
        dispatcher.cancel(agent.id, CancelSource::User),
        CancelOutcome::NothingToCancel,
        "token was cleared on the completed terminal → nothing to cancel"
    );

    // Exactly one terminal, and it is `completed` — no spurious cancelled.
    let terminal_statuses: Vec<String> = emitter
        .snapshot()
        .into_iter()
        .filter(|(_, v)| event_type(v) == "turn_end")
        .map(|(_, v)| v["outcome"]["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(terminal_statuses, vec!["completed".to_owned()]);
}

#[tokio::test]
async fn completed_turn_journals_send_but_no_outcome() {
    let dispatcher = Arc::new(Dispatcher::new());
    let adapter = MockHarnessAdapter::new(); // Streaming → completes
    let emitter = Arc::new(RecordingEmitter::new());
    let journal = Arc::new(RecordingJournal::default());
    let agent = agent_record();

    let handle = dispatcher
        .send_message(
            &agent,
            Path::new("/tmp/project"),
            "hello world",
            &adapter,
            as_emitter(&emitter),
            DispatchOptions::default(),
            Arc::clone(&journal) as Arc<dyn ConversationJournal>,
        )
        .await
        .unwrap();
    handle.join.await.unwrap();

    let sends = journal.sends.lock().unwrap();
    assert_eq!(sends.len(), 1, "the send is journaled at turn-start");
    assert_eq!(sends[0].0, handle.turn_id);
    assert_eq!(sends[0].1, "hello world");
    assert!(
        journal.outcomes.lock().unwrap().is_empty(),
        "a completed turn writes no outcome record — its content lives in the harness file"
    );
}

#[tokio::test]
async fn cancelled_turn_journals_send_and_a_cancelled_outcome() {
    let dispatcher = Arc::new(Dispatcher::new());
    let adapter = MockHarnessAdapter::with_scenario(MockScenario::AwaitCancellation);
    let emitter = Arc::new(RecordingEmitter::new());
    let journal = Arc::new(RecordingJournal::default());
    let agent = agent_record();

    let handle = dispatcher
        .send_message(
            &agent,
            Path::new("/tmp/project"),
            "long task",
            &adapter,
            as_emitter(&emitter),
            DispatchOptions::default(),
            Arc::clone(&journal) as Arc<dyn ConversationJournal>,
        )
        .await
        .unwrap();
    dispatcher.cancel(agent.id, CancelSource::Workflow);
    handle.join.await.unwrap();

    assert_eq!(journal.sends.lock().unwrap().len(), 1);
    let outcomes = journal.outcomes.lock().unwrap();
    assert_eq!(
        outcomes.len(),
        1,
        "a cancelled turn writes one outcome record"
    );
    assert_eq!(outcomes[0].0, handle.turn_id);
    assert_eq!(
        outcomes[0].1,
        TurnOutcome::Cancelled {
            source: CancelSource::Workflow
        },
        "the journaled outcome carries the stamped source"
    );
}

#[tokio::test]
async fn wait_until_idle_returns_after_cancellation_drains() {
    let dispatcher = Arc::new(Dispatcher::new());
    let adapter = MockHarnessAdapter::with_scenario(MockScenario::AwaitCancellation);
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();

    let handle = dispatcher
        .send_message(
            &agent,
            Path::new("/tmp/project"),
            "long task",
            &adapter,
            as_emitter(&emitter),
            DispatchOptions::default(),
            noop_journal(),
        )
        .await
        .unwrap();
    assert_eq!(
        dispatcher.agent_status(agent.id),
        Some(AgentStatus::InFlight)
    );

    dispatcher.cancel(agent.id, CancelSource::Shutdown);
    // Without joining the handle directly, wait for the drain to complete.
    dispatcher.wait_until_idle(agent.id).await;
    assert_eq!(dispatcher.agent_status(agent.id), Some(AgentStatus::Idle));
    handle.join.await.unwrap();
}

#[tokio::test]
async fn wait_until_idle_returns_immediately_when_idle_or_unknown() {
    let dispatcher = Arc::new(Dispatcher::new());
    let agent = agent_record();
    // Unknown agent — returns immediately (no panic, no hang).
    dispatcher.wait_until_idle(agent.id).await;
}

#[tokio::test]
async fn failed_turn_journals_send_and_a_failed_outcome() {
    let dispatcher = Arc::new(Dispatcher::new());
    let adapter = MockHarnessAdapter::with_scenario(MockScenario::Fails);
    let emitter = Arc::new(RecordingEmitter::new());
    let journal = Arc::new(RecordingJournal::default());
    let agent = agent_record();

    let handle = dispatcher
        .send_message(
            &agent,
            Path::new("/tmp/project"),
            "will fail",
            &adapter,
            as_emitter(&emitter),
            DispatchOptions::default(),
            Arc::clone(&journal) as Arc<dyn ConversationJournal>,
        )
        .await
        .unwrap();
    handle.join.await.unwrap();

    assert_eq!(journal.sends.lock().unwrap().len(), 1);
    let outcomes = journal.outcomes.lock().unwrap();
    assert_eq!(outcomes.len(), 1, "a failed turn writes one outcome record");
    assert_eq!(outcomes[0].0, handle.turn_id);
    assert!(
        matches!(outcomes[0].1, TurnOutcome::Failed { .. }),
        "the journaled outcome is the failure"
    );
    // Token cleared on the failed terminal → cancel is now a no-op.
    assert_eq!(
        dispatcher.cancel(agent.id, CancelSource::User),
        CancelOutcome::NothingToCancel
    );
}

#[tokio::test]
async fn cancellation_latches_and_drops_a_late_real_terminal() {
    // The adapter emits a real `Completed` terminal *after* cancel fires
    // (a buffered result that lost the race). The dispatcher must drop it and
    // the synthesized `Cancelled` must win — the user pressed stop, so the
    // turn is cancelled, not completed.
    let dispatcher = Arc::new(Dispatcher::new());
    let adapter = MockHarnessAdapter::with_scenario(MockScenario::TerminalAfterCancel);
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();

    let handle = dispatcher
        .send_message(
            &agent,
            Path::new("/tmp/project"),
            "racing turn",
            &adapter,
            as_emitter(&emitter),
            DispatchOptions::default(),
            noop_journal(),
        )
        .await
        .unwrap();
    dispatcher.cancel(agent.id, CancelSource::User);
    handle.join.await.unwrap();

    let snapshot = emitter.snapshot();
    let terminal_statuses: Vec<String> = snapshot
        .iter()
        .filter(|(_, v)| event_type(v) == "turn_end")
        .map(|(_, v)| v["outcome"]["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        terminal_statuses,
        vec!["cancelled".to_owned()],
        "exactly one terminal, and the late real Completed was dropped in favor of Cancelled"
    );

    // Partial output is preserved past the cancel (system-design §7): the
    // content the agent produced *after* the token fired but before the kill
    // is still emitted — only the late terminal is suppressed, not content.
    let chunks: Vec<String> = snapshot
        .iter()
        .filter(|(_, v)| event_type(v) == "content_chunk")
        .map(|(_, v)| v["text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        chunks,
        vec!["before-cancel".to_owned(), "after-cancel".to_owned()]
    );

    // Agent-scoped enrichment (rate-limit / session-meta) arriving after the
    // cancel but before stream-close reflects the agent's *real* state, so it
    // is forwarded as-is — the latch suppresses only the terminal, not content
    // or metadata. Contract: the dispatcher owns exactly the synthesized
    // `Cancelled` terminal; everything the adapter emitted before the stream
    // closed flows through.
    assert!(
        snapshot
            .iter()
            .any(|(_, v)| event_type(v) == "rate_limit_event"),
        "post-cancel agent-scoped enrichment must still be forwarded"
    );
}

#[tokio::test]
async fn send_record_failure_aborts_the_turn_without_starting_it() {
    // Fail-closed precondition: if the user's send can't be journaled, the turn
    // must not start — no events, no subprocess, agent left idle.
    let dispatcher = Arc::new(Dispatcher::new());
    let adapter = MockHarnessAdapter::new();
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();

    let result = dispatcher
        .send_message(
            &agent,
            Path::new("/tmp/project"),
            "unpersistable",
            &adapter,
            as_emitter(&emitter),
            DispatchOptions::default(),
            Arc::new(FailingJournal),
        )
        .await;

    assert!(matches!(result, Err(DispatcherError::Journal(_))));
    assert!(
        emitter.is_empty(),
        "no TurnStart (or any event) is emitted when the send can't be journaled"
    );
    assert_eq!(
        dispatcher.agent_status(agent.id),
        Some(AgentStatus::Idle),
        "guard dropped on the early return → agent back to Idle"
    );
}

#[tokio::test]
async fn dispatch_failure_after_send_journals_a_failed_outcome() {
    // The send is journaled before dispatch; if dispatch then fails, a Failed
    // marker is recorded so the turn isn't an orphan user message on restart.
    let dispatcher = Arc::new(Dispatcher::new());
    let adapter = MockHarnessAdapter::with_scenario(MockScenario::DispatchFails);
    let emitter = Arc::new(RecordingEmitter::new());
    let journal = Arc::new(RecordingJournal::default());
    let agent = agent_record();

    let result = dispatcher
        .send_message(
            &agent,
            Path::new("/tmp/project"),
            "send then fail to start",
            &adapter,
            as_emitter(&emitter),
            DispatchOptions::default(),
            Arc::clone(&journal) as Arc<dyn ConversationJournal>,
        )
        .await;

    assert!(matches!(result, Err(DispatcherError::Dispatch(_))));
    assert!(
        emitter.is_empty(),
        "no TurnStart on a pre-stream dispatch failure"
    );
    assert_eq!(
        journal.sends.lock().unwrap().len(),
        1,
        "the send was journaled before dispatch"
    );
    let outcomes = journal.outcomes.lock().unwrap();
    assert_eq!(
        outcomes.len(),
        1,
        "the dispatch failure is journaled as a Failed marker"
    );
    assert!(matches!(outcomes[0].1, TurnOutcome::Failed { .. }));
    assert_eq!(
        dispatcher.agent_status(agent.id),
        Some(AgentStatus::Idle),
        "guard dropped → agent back to Idle"
    );
}
