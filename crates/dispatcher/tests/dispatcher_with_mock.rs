//! End-to-end dispatcher behavior, exercised through the public API as an
//! external consumer would. `MockHarnessAdapter` stands in for a real harness
//! so these run hermetically in `make check` — no subprocess, no real CLI.
//!
//! Compiling as an integration test (`tests/<file>.rs`) means the dispatcher
//! crate is linked here the same way a downstream consumer would link it.
//! If any of these tests accidentally relied on a private item, the compile
//! would fail — a useful external-consumer-contract check on the public
//! surface.
//!
//! These tests drive the actor-model API: `send_message` is fire-and-forget
//! (returns a `MessageId` receipt), and turn/chain completion is awaited
//! deterministically off the event stream via `RecordingEmitter::wait_for*`.
//! Every wait is wrapped in `tokio::time::timeout` so a logic bug surfaces as
//! a bounded timeout rather than hanging the suite.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use switchboard_core::{AgentId, AgentRecord, HarnessKind, SendId, SessionLocator};
use switchboard_dispatcher::{
    CancelOutcome, ConversationJournal, DispatchContext, DispatchContextFactory, Dispatcher,
    EventEmitter, JournalError, MetadataCache, NoopJournal, NoopMetadataCache,
    NoopSessionLocatorSink, NotQueued, OnBusy, RecordingEmitter, SendOutcome, SessionLocatorError,
    SessionLocatorSink,
};
use switchboard_harness::{
    CancelSource, ContextWindowSource, DispatchOptions, HarnessAdapter, MessageId,
    MockHarnessAdapter, MockScenario, RateLimitSource, TurnId, TurnOutcome,
};
use uuid::Uuid;

/// Default deadline for any `wait_for*` so a logic bug fails as a bounded
/// timeout rather than hanging the suite.
const WAIT: Duration = Duration::from_secs(5);

/// Await an emitter wait-future under the shared timeout, panicking with a
/// snapshot of what *was* recorded if it doesn't resolve in time.
async fn within<F: std::future::Future<Output = ()>>(
    emitter: &RecordingEmitter,
    label: &str,
    fut: F,
) {
    assert!(
        tokio::time::timeout(WAIT, fut).await.is_ok(),
        "timed out waiting for {label}; recorded events: {:?}",
        emitter
            .snapshot()
            .iter()
            .map(|(n, v)| (n.clone(), v["type"].as_str().unwrap_or("?").to_owned()))
            .collect::<Vec<_>>()
    );
}

/// The capturing factory each test builds. It freezes the agent record, the
/// emitter, and the journal, handing the actor a fresh `DispatchContext` per
/// turn — mirroring the app-side factory's contract.
///
/// **Per-actor, not per-send (load-bearing):** the dispatcher uses the factory
/// passed to the *first* `send_message` for an agent and owns it thereafter;
/// later sends to the same actor ignore their passed factory. So a turn's
/// scenario is whatever this one factory hands back. To drive a *sequence* of
/// scenarios across an actor's chained turns (e.g. first turn fails, second
/// streams), the factory pops a scenario per `build`, defaulting to the last
/// once the script is exhausted.
struct TestFactory {
    /// Pre-built adapters, one popped per turn; the final one sticks for any
    /// further turns. A `MockHarnessAdapter` dispatches via `&self` (fresh
    /// channel per call), so reusing the last Arc across extra turns is safe.
    adapters: Mutex<std::collections::VecDeque<Arc<dyn HarnessAdapter>>>,
    last: Arc<dyn HarnessAdapter>,
    agent: AgentRecord,
    emitter: Arc<dyn EventEmitter>,
    journal: Arc<dyn ConversationJournal>,
    metadata: Arc<dyn MetadataCache>,
    locator_sink: Arc<dyn SessionLocatorSink>,
}

impl TestFactory {
    fn new(
        scenario: MockScenario,
        agent: AgentRecord,
        emitter: Arc<RecordingEmitter>,
        journal: Arc<dyn ConversationJournal>,
    ) -> Arc<Self> {
        Self::sequence([scenario], agent, emitter, journal)
    }

    /// Build a factory that hands back adapters for `scenarios` in order across
    /// successive turns; the final scenario's adapter sticks for any further
    /// turns. Lets one actor's chained turns run distinct scenarios (e.g. first
    /// turn fails, second streams) despite the factory being per-actor.
    fn sequence(
        scenarios: impl IntoIterator<Item = MockScenario>,
        agent: AgentRecord,
        emitter: Arc<RecordingEmitter>,
        journal: Arc<dyn ConversationJournal>,
    ) -> Arc<Self> {
        Self::sequence_with_metadata(
            scenarios,
            agent,
            emitter,
            journal,
            Arc::new(NoopMetadataCache),
        )
    }

    /// As [`Self::sequence`], but with an injected [`MetadataCache`] so the
    /// metadata-persistence durability-gate test can capture what the
    /// dispatcher records (or doesn't) per `RateLimitSource`.
    fn sequence_with_metadata(
        scenarios: impl IntoIterator<Item = MockScenario>,
        agent: AgentRecord,
        emitter: Arc<RecordingEmitter>,
        journal: Arc<dyn ConversationJournal>,
        metadata: Arc<dyn MetadataCache>,
    ) -> Arc<Self> {
        Self::sequence_with_locator_sink(
            scenarios,
            agent,
            emitter,
            journal,
            metadata,
            Arc::new(NoopSessionLocatorSink),
        )
    }

    /// As [`Self::sequence_with_metadata`], but with an injected
    /// [`SessionLocatorSink`] so the runtime-capture tests can capture what the
    /// dispatcher persists (or assert a persist failure fails the turn).
    fn sequence_with_locator_sink(
        scenarios: impl IntoIterator<Item = MockScenario>,
        agent: AgentRecord,
        emitter: Arc<RecordingEmitter>,
        journal: Arc<dyn ConversationJournal>,
        metadata: Arc<dyn MetadataCache>,
        locator_sink: Arc<dyn SessionLocatorSink>,
    ) -> Arc<Self> {
        let queue: std::collections::VecDeque<Arc<dyn HarnessAdapter>> = scenarios
            .into_iter()
            .map(|s| Arc::new(MockHarnessAdapter::with_scenario(s)) as Arc<dyn HarnessAdapter>)
            .collect();
        let last = Arc::clone(queue.back().expect("at least one scenario"));
        Arc::new(Self {
            adapters: Mutex::new(queue),
            last,
            agent,
            emitter: emitter as Arc<dyn EventEmitter>,
            journal,
            metadata,
            locator_sink,
        })
    }
}

impl DispatchContextFactory for TestFactory {
    fn build(&self, _send_id: SendId) -> DispatchContext {
        let adapter = {
            let mut q = self.adapters.lock().unwrap();
            if q.len() > 1 {
                q.pop_front().unwrap()
            } else {
                Arc::clone(&self.last)
            }
        };
        DispatchContext {
            adapter,
            cwd: PathBuf::from("/tmp/project"),
            agent: self.agent.clone(),
            emitter: Arc::clone(&self.emitter),
            options: DispatchOptions::default(),
            journal: Arc::clone(&self.journal),
            metadata: Arc::clone(&self.metadata),
            locator_sink: Arc::clone(&self.locator_sink),
        }
    }

    fn idle_emitter(&self) -> Arc<dyn EventEmitter> {
        Arc::clone(&self.emitter)
    }
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

/// Captures metadata-cache calls so the durability-gate tests can assert the
/// dispatcher persists `StreamOnly` rate-limit payloads (skipping
/// `SessionFileBacked` ones) and `StreamOnly` context windows, with a
/// roughly-now `captured_at`.
#[derive(Default)]
struct RecordingMetadataCache {
    calls: Mutex<Vec<(AgentId, serde_json::Value, DateTime<Utc>)>>,
    context_window_calls: Mutex<Vec<(AgentId, u32, DateTime<Utc>)>>,
}

impl MetadataCache for RecordingMetadataCache {
    fn record_rate_limit(
        &self,
        agent_id: AgentId,
        info: serde_json::Value,
        captured_at: DateTime<Utc>,
    ) {
        self.calls
            .lock()
            .unwrap()
            .push((agent_id, info, captured_at));
    }

    fn record_context_window(
        &self,
        agent_id: AgentId,
        context_window: u32,
        captured_at: DateTime<Utc>,
    ) {
        self.context_window_calls
            .lock()
            .unwrap()
            .push((agent_id, context_window, captured_at));
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

/// Records every captured locator the dispatcher persists, so tests can assert
/// the sink fired once per capture event (and is never deduped).
#[derive(Default)]
struct RecordingLocatorSink {
    persisted: Mutex<Vec<(AgentId, SessionLocator)>>,
}

impl SessionLocatorSink for RecordingLocatorSink {
    fn persist(
        &self,
        agent_id: AgentId,
        locator: SessionLocator,
    ) -> Result<(), SessionLocatorError> {
        self.persisted.lock().unwrap().push((agent_id, locator));
        Ok(())
    }
}

/// A locator sink that always fails — used to assert a first-capture persist
/// failure fails the turn (the load-bearing distinction from `MetadataCache`).
struct FailingLocatorSink;

impl SessionLocatorSink for FailingLocatorSink {
    fn persist(&self, _: AgentId, _: SessionLocator) -> Result<(), SessionLocatorError> {
        Err(SessionLocatorError("registry on fire".into()))
    }
}

fn agent_record() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "test-agent".to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_locator: Some(SessionLocator::Uuid(Uuid::now_v7())),
        created_at: Utc::now(),
    }
}

fn noop_journal() -> Arc<dyn ConversationJournal> {
    Arc::new(NoopJournal)
}

fn event_type(value: &serde_json::Value) -> &str {
    value["type"].as_str().expect("event has type tag")
}

fn extract_turn_id(value: &serde_json::Value) -> TurnId {
    let s = value["turn_id"].as_str().expect("event has turn_id");
    Uuid::parse_str(s).expect("turn_id parses as UUID")
}

fn extract_message_id(value: &serde_json::Value) -> MessageId {
    let s = value["message_id"].as_str().expect("event has message_id");
    Uuid::parse_str(s).expect("message_id parses as UUID")
}

/// Unwrap an `Accepted` send outcome to its message id (the Enqueue path never
/// returns `Busy`).
fn accepted(outcome: SendOutcome) -> MessageId {
    match outcome {
        SendOutcome::Accepted(id) => id,
        SendOutcome::Busy => panic!("expected Accepted, got Busy"),
    }
}

/// Count emitted events of a given wire `type`.
fn count_type(events: &[(String, serde_json::Value)], ty: &str) -> usize {
    events.iter().filter(|(_, v)| event_type(v) == ty).count()
}

#[tokio::test]
async fn send_message_emits_turn_start_before_content_chunks() {
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    let message_id = accepted(
        dispatcher
            .send_message(agent.id, "hello", Uuid::now_v7(), factory, OnBusy::Enqueue)
            .await,
    );

    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

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

    // TurnStart carries the message_id returned by send.
    assert_eq!(
        extract_message_id(&events[0].1),
        message_id,
        "TurnStart.message_id correlates to the accepted send"
    );

    // turn_id is consistent across the whole sequence — across turn-scoped
    // variants only. AgentIdle is agent-scoped and carries `agent_id`
    // instead of `turn_id`, so it's excluded from this check.
    let turn_id = extract_turn_id(&events[0].1);
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
async fn second_send_to_busy_agent_enqueues_and_auto_dispatches() {
    // Under the actor, a second send while busy ENQUEUES (Enqueue path) and
    // auto-dispatches after the first turn ends: two TurnStart/TurnEnd cycles,
    // FIFO. (Re-expresses the old `concurrent_send_to_same_agent_returns_busy`
    // for the queueing semantics — Busy is no longer the Enqueue outcome.)
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    let first = accepted(
        dispatcher
            .send_message(
                agent.id,
                "first",
                Uuid::now_v7(),
                Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
                OnBusy::Enqueue,
            )
            .await,
    );
    let second = accepted(
        dispatcher
            .send_message(agent.id, "second", Uuid::now_v7(), factory, OnBusy::Enqueue)
            .await,
    );

    // Both turns run to completion: two turn_start, two turn_end, one final idle.
    within(
        &emitter,
        "both turns + idle",
        emitter.wait_for(|e| count_type(e, "agent_idle") >= 1 && count_type(e, "turn_start") >= 2),
    )
    .await;

    let events = emitter.snapshot();
    let turn_starts: Vec<MessageId> = events
        .iter()
        .filter(|(_, v)| event_type(v) == "turn_start")
        .map(|(_, v)| extract_message_id(v))
        .collect();
    assert_eq!(turn_starts, vec![first, second], "FIFO dispatch order");
    assert_eq!(count_type(&events, "turn_end"), 2);
    assert_eq!(
        count_type(&events, "agent_idle"),
        1,
        "exactly one AgentIdle after the chain drains"
    );
}

#[tokio::test]
async fn concurrent_send_to_different_agents_both_succeed() {
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent_a = agent_record();
    let agent_b = agent_record();
    let factory_a = TestFactory::new(
        MockScenario::Streaming,
        agent_a.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );
    let factory_b = TestFactory::new(
        MockScenario::Streaming,
        agent_b.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(
            agent_a.id,
            "A's prompt",
            Uuid::now_v7(),
            factory_a,
            OnBusy::Enqueue,
        )
        .await;
    dispatcher
        .send_message(
            agent_b.id,
            "B's prompt",
            Uuid::now_v7(),
            factory_b,
            OnBusy::Enqueue,
        )
        .await;

    // Two actors → two idles.
    within(
        &emitter,
        "two agent_idles",
        emitter.wait_for_type("agent_idle", 2),
    )
    .await;

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
async fn panic_in_producer_recovers_and_a_later_send_completes() {
    // MockScenario::Panic kills the producer task mid-stream (no terminal).
    // The actor recovers — a subsequent send to the same agent runs to
    // agent_idle. (Re-expresses the old "restores to idle" as "a later send
    // completes," since there is no status flag to assert in the actor model.)
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    // One per-actor factory: first turn panics, the second (and any further)
    // streams. The actor must recover from the panicked producer and run the
    // queued healthy turn.
    let factory = TestFactory::sequence(
        [MockScenario::Panic, MockScenario::Streaming],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(
            agent.id,
            "will panic",
            Uuid::now_v7(),
            Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;

    // The panicked turn produced a TurnStart (proving the turn went live before
    // the producer died) but never a terminal — the actor drains to AgentIdle.
    within(
        &emitter,
        "agent_idle after panic",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;
    assert!(
        emitter
            .snapshot()
            .iter()
            .any(|(_, v)| event_type(v) == "turn_start"),
        "TurnStart should have been emitted before the producer panicked"
    );

    // A fresh, healthy send to the same agent runs to completion — the actor is
    // usable again, not stuck.
    dispatcher
        .send_message(
            agent.id,
            "now works",
            Uuid::now_v7(),
            factory,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "second agent_idle",
        emitter.wait_for_type("agent_idle", 2),
    )
    .await;
    assert!(
        count_type(&emitter.snapshot(), "turn_end") >= 1,
        "the later healthy send produced a terminal"
    );
}

#[tokio::test]
async fn truncated_stream_without_turn_end_returns_to_idle() {
    // Fault-injection: MockScenario::TruncatedStream emits chunks and
    // drops the sender without TurnEnd — a deliberate contract violation.
    // The dispatcher's actor still reaches AgentIdle (state recovery); the
    // missing terminal is a visible bug the frontend reducer must handle.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::TruncatedStream,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(agent.id, "prompt", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    let events = emitter.snapshot();
    let kinds: Vec<&str> = events.iter().map(|(_, v)| event_type(v)).collect();
    assert_eq!(kinds.first().copied(), Some("turn_start"));
    assert!(
        !kinds.contains(&"turn_end"),
        "TruncatedStream emits no terminal event — got {kinds:?}"
    );
    assert_eq!(kinds.last().copied(), Some("agent_idle"));
}

#[tokio::test]
async fn dispatch_failure_emits_message_failed_no_turn_start_and_stays_usable() {
    // MockScenario::DispatchFails → `adapter.dispatch()` returns Err before any
    // stream. The actor emits NO TurnStart; instead it emits MessageFailed
    // keyed by the returned message_id. The agent remains usable.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    // First turn fails to dispatch; the second (and beyond) streams — proving
    // the actor stays usable after a dispatch failure.
    let factory = TestFactory::sequence(
        [MockScenario::DispatchFails, MockScenario::Streaming],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    let message_id = accepted(
        dispatcher
            .send_message(
                agent.id,
                "won't dispatch",
                Uuid::now_v7(),
                Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
                OnBusy::Enqueue,
            )
            .await,
    );

    within(
        &emitter,
        "message_failed",
        emitter.wait_for_type("message_failed", 1),
    )
    .await;
    // The actor also drains to idle after the failed (un-started) turn.
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    let events = emitter.snapshot();
    assert_eq!(
        count_type(&events, "turn_start"),
        0,
        "dispatch failure emits no TurnStart"
    );
    let failed = events
        .iter()
        .find(|(_, v)| event_type(v) == "message_failed")
        .expect("a message_failed event");
    assert_eq!(
        extract_message_id(&failed.1),
        message_id,
        "message_failed is keyed by the accepted send's message_id"
    );

    // The next send must run to completion — the agent isn't stuck.
    dispatcher
        .send_message(
            agent.id,
            "now works",
            Uuid::now_v7(),
            factory,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "turn_start after recovery",
        emitter.wait_for_type("turn_start", 1),
    )
    .await;
    within(
        &emitter,
        "second agent_idle",
        emitter.wait_for_type("agent_idle", 2),
    )
    .await;
}

#[tokio::test]
async fn agent_idle_is_last_event_and_unblocks_next_send() {
    // (1) AgentIdle is the LAST event on the per-agent channel for a dispatch.
    // (2) A fresh send afterward works (no Busy / no stuck state).
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(
            agent.id,
            "first",
            Uuid::now_v7(),
            Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    let events = emitter.snapshot();
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
    assert_eq!(
        count_type(&events, "agent_idle"),
        1,
        "exactly one AgentIdle per dispatch"
    );

    // A fresh send to the same agent runs to a second idle.
    dispatcher
        .send_message(agent.id, "second", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "second agent_idle",
        emitter.wait_for_type("agent_idle", 2),
    )
    .await;
}

#[tokio::test]
async fn agent_idle_is_last_after_codex_post_terminal_enrichment_sequence() {
    // Pins the ordering invariant for Codex's post-terminal enrichment:
    // TurnEnd → RateLimit → SessionMeta → agent_idle, agent_idle strictly last.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::CodexPostTerminalEnrichment,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(agent.id, "hello", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    let events = emitter.snapshot();
    let type_sequence: Vec<&str> = events.iter().map(|(_, v)| event_type(v)).collect();
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
async fn stream_only_rate_limit_is_persisted_to_metadata_cache() {
    // Durability gate: a RateLimitEvent whose source is StreamOnly (class-C,
    // no on-disk equivalent — Claude) must be recorded to the metadata cache
    // so it survives restart.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let metadata = Arc::new(RecordingMetadataCache::default());
    let before = Utc::now();
    let factory = TestFactory::sequence_with_metadata(
        [MockScenario::RateLimitWithSource(
            RateLimitSource::StreamOnly,
        )],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
        Arc::clone(&metadata) as Arc<dyn MetadataCache>,
    );

    dispatcher
        .send_message(agent.id, "hello", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;
    let after = Utc::now();

    let calls = metadata.calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "StreamOnly rate-limit must be persisted exactly once"
    );
    let (recorded_agent, payload, captured_at) = &calls[0];
    assert_eq!(*recorded_agent, agent.id);
    assert_eq!(payload["primary"]["used_percent"], 42.0);
    assert!(
        *captured_at >= before && *captured_at <= after,
        "captured_at must be stamped at record time (roughly now)"
    );
}

#[tokio::test]
async fn session_file_backed_rate_limit_is_not_persisted() {
    // Durability gate, negative case: a SessionFileBacked rate-limit (class-B,
    // already durable in the harness's own session file — Codex) must NOT be
    // re-persisted to the metadata cache.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let metadata = Arc::new(RecordingMetadataCache::default());
    let factory = TestFactory::sequence_with_metadata(
        [MockScenario::RateLimitWithSource(
            RateLimitSource::SessionFileBacked,
        )],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
        Arc::clone(&metadata) as Arc<dyn MetadataCache>,
    );

    dispatcher
        .send_message(agent.id, "hello", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    // The wire event still reaches the frontend (the reducer overwrites the
    // live value); only Switchboard-side *persistence* is gated.
    assert!(
        emitter
            .snapshot()
            .iter()
            .any(|(_, v)| event_type(v) == "rate_limit_event"),
        "the rate_limit_event must still be emitted on the wire"
    );
    assert!(
        metadata.calls.lock().unwrap().is_empty(),
        "SessionFileBacked rate-limit must NOT be persisted (harness file is canonical)"
    );
}

#[tokio::test]
async fn stream_only_context_window_is_persisted_to_metadata_cache() {
    // Restart-continuity gate: a TurnEnd carrying a StreamOnly context window
    // (class-C — Claude's `result.modelUsage`) must be recorded to the metadata
    // cache so the context bar renders on reopen. Keyed on the running agent.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let metadata = Arc::new(RecordingMetadataCache::default());
    let before = Utc::now();
    let factory = TestFactory::sequence_with_metadata(
        [MockScenario::CompletesWithContextWindow {
            context_window: 200_000,
            source: ContextWindowSource::StreamOnly,
        }],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
        Arc::clone(&metadata) as Arc<dyn MetadataCache>,
    );

    dispatcher
        .send_message(agent.id, "hello", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;
    let after = Utc::now();

    let calls = metadata.context_window_calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "a StreamOnly context window must be persisted exactly once"
    );
    let (recorded_agent, context_window, captured_at) = &calls[0];
    assert_eq!(*recorded_agent, agent.id);
    assert_eq!(*context_window, 200_000);
    assert!(
        *captured_at >= before && *captured_at <= after,
        "captured_at must be stamped at record time (roughly now)"
    );
}

#[tokio::test]
async fn session_file_backed_context_window_is_not_persisted() {
    // Durability gate, negative case: a SessionFileBacked context window
    // (class-B, already durable in Codex's own session file) must NOT be
    // shadow-cached to the metadata sidecar — mirrors the rate-limit gate.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let metadata = Arc::new(RecordingMetadataCache::default());
    let factory = TestFactory::sequence_with_metadata(
        [MockScenario::CompletesWithContextWindow {
            context_window: 272_000,
            source: ContextWindowSource::SessionFileBacked,
        }],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
        Arc::clone(&metadata) as Arc<dyn MetadataCache>,
    );

    dispatcher
        .send_message(agent.id, "hello", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    assert!(
        metadata.context_window_calls.lock().unwrap().is_empty(),
        "SessionFileBacked context window must NOT be persisted (harness file is canonical)"
    );
}

#[tokio::test]
async fn cancel_in_flight_synthesizes_cancelled_then_idle_and_unblocks_next_send() {
    // The mock adapter parks until the token fires, then ends its stream
    // WITHOUT a terminal — the dispatcher must synthesize TurnEnd { Cancelled }.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    // First turn parks-until-cancel; the second streams — proving the agent is
    // re-promptable after a cancellation.
    let factory = TestFactory::sequence(
        [MockScenario::AwaitCancellation, MockScenario::Streaming],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(
            agent.id,
            "long task",
            Uuid::now_v7(),
            Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    // Wait until the turn is actually live before cancelling.
    within(
        &emitter,
        "turn_start (in flight)",
        emitter.wait_for_type("turn_start", 1),
    )
    .await;

    assert_eq!(
        dispatcher.cancel(agent.id, CancelSource::User),
        CancelOutcome::Requested,
        "cancelling an in-flight turn requests cancellation"
    );
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

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

    // Re-promptable immediately: a fresh send runs to a second idle.
    dispatcher
        .send_message(agent.id, "next", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "second agent_idle",
        emitter.wait_for_type("agent_idle", 2),
    )
    .await;
}

#[tokio::test]
async fn cancel_when_not_in_flight_emits_no_cancelled_terminal() {
    // Cancel delivered to an idle (but existing) actor is an in-actor no-op: no
    // synthesized Cancelled terminal is emitted. For a truly unknown agent,
    // cancel returns NothingToCancel.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();

    // Unknown agent → no actor exists.
    assert_eq!(
        dispatcher.cancel(agent.id, CancelSource::User),
        CancelOutcome::NothingToCancel,
        "no actor for an agent that was never dispatched to"
    );

    // Run a turn to completion so the actor exists and is idle.
    let factory = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );
    dispatcher
        .send_message(agent.id, "hi", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    // Cancel the now-idle actor: delivered (Requested) but a no-op internally.
    dispatcher.cancel(agent.id, CancelSource::User);

    // Behaviorally: no cancelled terminal ever appears, only the one completed.
    let statuses: Vec<String> = emitter
        .snapshot()
        .into_iter()
        .filter(|(_, v)| event_type(v) == "turn_end")
        .map(|(_, v)| v["outcome"]["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(statuses, vec!["completed".to_owned()]);
}

#[tokio::test]
async fn cancel_after_terminal_is_a_no_op_and_emits_no_extra_cancelled() {
    // After a turn's terminal, a cancel produces no extra Cancelled event.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(agent.id, "hi", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    dispatcher.cancel(agent.id, CancelSource::User);

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
    let emitter = Arc::new(RecordingEmitter::new());
    let journal = Arc::new(RecordingJournal::default());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        Arc::clone(&journal) as Arc<dyn ConversationJournal>,
    );

    dispatcher
        .send_message(
            agent.id,
            "hello world",
            Uuid::now_v7(),
            factory,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    let turn_id = extract_turn_id(
        &emitter
            .snapshot()
            .iter()
            .find(|(_, v)| event_type(v) == "turn_start")
            .expect("a turn_start")
            .1,
    );
    let sends = journal.sends.lock().unwrap();
    assert_eq!(sends.len(), 1, "the send is journaled at turn-start");
    assert_eq!(sends[0].0, turn_id);
    assert_eq!(sends[0].1, "hello world");
    assert!(
        journal.outcomes.lock().unwrap().is_empty(),
        "a completed turn writes no outcome record — its content lives in the harness file"
    );
}

#[tokio::test]
async fn cancelled_turn_journals_send_and_a_cancelled_outcome() {
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let journal = Arc::new(RecordingJournal::default());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::AwaitCancellation,
        agent.clone(),
        Arc::clone(&emitter),
        Arc::clone(&journal) as Arc<dyn ConversationJournal>,
    );

    dispatcher
        .send_message(
            agent.id,
            "long task",
            Uuid::now_v7(),
            factory,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "turn_start (in flight)",
        emitter.wait_for_type("turn_start", 1),
    )
    .await;
    dispatcher.cancel(agent.id, CancelSource::Workflow);
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    let turn_id = extract_turn_id(
        &emitter
            .snapshot()
            .iter()
            .find(|(_, v)| event_type(v) == "turn_start")
            .expect("a turn_start")
            .1,
    );
    assert_eq!(journal.sends.lock().unwrap().len(), 1);
    let outcomes = journal.outcomes.lock().unwrap();
    assert_eq!(
        outcomes.len(),
        1,
        "a cancelled turn writes one outcome record"
    );
    assert_eq!(outcomes[0].0, turn_id);
    assert_eq!(
        outcomes[0].1,
        TurnOutcome::Cancelled {
            source: CancelSource::Workflow
        },
        "the journaled outcome carries the stamped source"
    );
}

#[tokio::test]
async fn failed_turn_journals_send_and_a_failed_outcome() {
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let journal = Arc::new(RecordingJournal::default());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::Fails,
        agent.clone(),
        Arc::clone(&emitter),
        Arc::clone(&journal) as Arc<dyn ConversationJournal>,
    );

    dispatcher
        .send_message(
            agent.id,
            "will fail",
            Uuid::now_v7(),
            factory,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    let turn_id = extract_turn_id(
        &emitter
            .snapshot()
            .iter()
            .find(|(_, v)| event_type(v) == "turn_start")
            .expect("a turn_start")
            .1,
    );
    assert_eq!(journal.sends.lock().unwrap().len(), 1);
    let outcomes = journal.outcomes.lock().unwrap();
    assert_eq!(outcomes.len(), 1, "a failed turn writes one outcome record");
    assert_eq!(outcomes[0].0, turn_id);
    assert!(
        matches!(outcomes[0].1, TurnOutcome::Failed { .. }),
        "the journaled outcome is the failure"
    );
}

#[tokio::test]
async fn cancellation_latches_and_drops_a_late_real_terminal() {
    // The adapter emits a real `Completed` terminal *after* cancel fires (a
    // buffered result that lost the race). The dispatcher must drop it; the
    // synthesized `Cancelled` wins. Partial content before/after the cancel is
    // still emitted. Exactly one terminal.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::TerminalAfterCancel,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(
            agent.id,
            "racing turn",
            Uuid::now_v7(),
            factory,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "turn_start (in flight)",
        emitter.wait_for_type("turn_start", 1),
    )
    .await;
    dispatcher.cancel(agent.id, CancelSource::User);
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

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

    // Partial output is preserved past the cancel (system-design §7).
    let chunks: Vec<String> = snapshot
        .iter()
        .filter(|(_, v)| event_type(v) == "content_chunk")
        .map(|(_, v)| v["text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        chunks,
        vec!["before-cancel".to_owned(), "after-cancel".to_owned()]
    );

    // Agent-scoped enrichment arriving after the cancel is still forwarded.
    assert!(
        snapshot
            .iter()
            .any(|(_, v)| event_type(v) == "rate_limit_event"),
        "post-cancel agent-scoped enrichment must still be forwarded"
    );
}

#[tokio::test]
async fn send_record_failure_aborts_the_turn_without_starting_it() {
    // Fail-closed: if the user's send can't be journaled, no TurnStart, no
    // adapter dispatch, no outcome marker. Under the actor, the failure surfaces
    // as a MessageFailed event (the async analogue of the old synchronous Err).
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    // FailingJournal errors on record_send, so the turn must not start.
    let factory = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        Arc::new(FailingJournal),
    );

    let message_id = accepted(
        dispatcher
            .send_message(
                agent.id,
                "unpersistable",
                Uuid::now_v7(),
                factory,
                OnBusy::Enqueue,
            )
            .await,
    );

    within(
        &emitter,
        "message_failed",
        emitter.wait_for_type("message_failed", 1),
    )
    .await;
    // Actor still drains to idle after the un-started turn.
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    let events = emitter.snapshot();
    assert_eq!(
        count_type(&events, "turn_start"),
        0,
        "no TurnStart when the send can't be journaled"
    );
    let failed = events
        .iter()
        .find(|(_, v)| event_type(v) == "message_failed")
        .expect("a message_failed event");
    assert_eq!(extract_message_id(&failed.1), message_id);
}

#[tokio::test]
async fn dispatch_failure_after_send_journals_a_failed_outcome() {
    // The send is journaled before dispatch; if dispatch then fails, a Failed
    // marker is recorded (against the minted turn_id) and a message_failed event
    // appears — no TurnStart.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let journal = Arc::new(RecordingJournal::default());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::DispatchFails,
        agent.clone(),
        Arc::clone(&emitter),
        Arc::clone(&journal) as Arc<dyn ConversationJournal>,
    );

    let message_id = accepted(
        dispatcher
            .send_message(
                agent.id,
                "send then fail to start",
                Uuid::now_v7(),
                factory,
                OnBusy::Enqueue,
            )
            .await,
    );

    within(
        &emitter,
        "message_failed",
        emitter.wait_for_type("message_failed", 1),
    )
    .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    let events = emitter.snapshot();
    assert_eq!(
        count_type(&events, "turn_start"),
        0,
        "no TurnStart on a pre-stream dispatch failure"
    );
    let failed = events
        .iter()
        .find(|(_, v)| event_type(v) == "message_failed")
        .expect("a message_failed event");
    assert_eq!(extract_message_id(&failed.1), message_id);

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
}

// ---------------------------------------------------------------------------
// M4.4: queueing, removal, teardown, and wire-correlation behaviors.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fifo_multiple_sends_to_busy_agent_dispatch_in_order() {
    // ≥2 sends to a busy agent dispatch in FIFO order: the TurnStart message_ids
    // appear in send order, and the content chunks reflect each prompt in order.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    let mut ids: Vec<MessageId> = Vec::new();
    for prompt in ["one", "two", "three"] {
        ids.push(accepted(
            dispatcher
                .send_message(
                    agent.id,
                    prompt,
                    Uuid::now_v7(),
                    Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
                    OnBusy::Enqueue,
                )
                .await,
        ));
    }

    within(
        &emitter,
        "three turns drained",
        emitter.wait_for(|e| count_type(e, "agent_idle") >= 1 && count_type(e, "turn_start") >= 3),
    )
    .await;

    let events = emitter.snapshot();
    let start_ids: Vec<MessageId> = events
        .iter()
        .filter(|(_, v)| event_type(v) == "turn_start")
        .map(|(_, v)| extract_message_id(v))
        .collect();
    assert_eq!(start_ids, ids, "TurnStart message_ids match send order");

    // The Streaming scenario echoes the prompt as its middle chunk; assert the
    // prompts surface in send order.
    let echoed: Vec<String> = events
        .iter()
        .filter(|(_, v)| event_type(v) == "content_chunk")
        .map(|(_, v)| v["text"].as_str().unwrap().to_owned())
        .filter(|t| ["one", "two", "three"].contains(&t.as_str()))
        .collect();
    assert_eq!(echoed, vec!["one", "two", "three"]);
    assert_eq!(
        count_type(&events, "agent_idle"),
        1,
        "exactly one AgentIdle after the whole chain"
    );
}

/// Helper: after a first turn settles per `first_scenario`, a queued second
/// (Streaming) turn must still auto-dispatch. Covers completed/failed/cancelled
/// first turns. For the cancel case the first turn parks until cancelled, so we
/// must fire the cancel; for completed/failed the first turn settles on its own.
async fn auto_dispatch_after_first(first_scenario: MockScenario, cancel_first: bool) {
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    // One per-actor factory: the first turn runs `first_scenario`, the second
    // streams. Both sends are enqueued back-to-back so the second is queued
    // behind the first regardless of how fast the first settles — making the
    // "exactly one idle for the whole chain" assertion deterministic.
    let factory = TestFactory::sequence(
        [first_scenario, MockScenario::Streaming],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(
            agent.id,
            "first",
            Uuid::now_v7(),
            Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    let second = accepted(
        dispatcher
            .send_message(agent.id, "second", Uuid::now_v7(), factory, OnBusy::Enqueue)
            .await,
    );

    if cancel_first {
        // The first turn parks until cancelled; fire it once it's live.
        within(
            &emitter,
            "first turn_start",
            emitter.wait_for_type("turn_start", 1),
        )
        .await;
        dispatcher.cancel(agent.id, CancelSource::User);
    }

    // The queued second turn still runs: two turn_starts, one final idle.
    within(
        &emitter,
        "second turn dispatched",
        emitter.wait_for(|e| count_type(e, "turn_start") >= 2 && count_type(e, "agent_idle") >= 1),
    )
    .await;

    let events = emitter.snapshot();
    let last_start = events
        .iter()
        .filter(|(_, v)| event_type(v) == "turn_start")
        .nth(1)
        .expect("a second turn_start");
    assert_eq!(
        extract_message_id(&last_start.1),
        second,
        "the queued second turn auto-dispatched"
    );
    assert_eq!(
        count_type(&events, "agent_idle"),
        1,
        "exactly one AgentIdle after the chain"
    );
}

#[tokio::test]
async fn auto_dispatch_after_completed_first_turn() {
    auto_dispatch_after_first(MockScenario::Streaming, false).await;
}

#[tokio::test]
async fn auto_dispatch_after_failed_first_turn() {
    auto_dispatch_after_first(MockScenario::Fails, false).await;
}

#[tokio::test]
async fn auto_dispatch_after_cancelled_first_turn() {
    auto_dispatch_after_first(MockScenario::AwaitCancellation, true).await;
}

#[tokio::test]
async fn remove_queued_message_prevents_dispatch_and_returns_payload() {
    // Enqueue behind a busy (AwaitCancellation) turn, remove the queued one →
    // it never dispatches; the removal returns its payload.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let blocker = TestFactory::new(
        MockScenario::AwaitCancellation,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(
            agent.id,
            "blocker",
            Uuid::now_v7(),
            blocker,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "blocker turn_start",
        emitter.wait_for_type("turn_start", 1),
    )
    .await;

    let queued_send_id = Uuid::now_v7();
    let queued = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );
    let queued_id = accepted(
        dispatcher
            .send_message(agent.id, "queued", queued_send_id, queued, OnBusy::Enqueue)
            .await,
    );

    let removed = dispatcher
        .remove_queued_message(agent.id, queued_id)
        .await
        .expect("the queued message is removable");
    assert_eq!(removed.agent_id, agent.id);
    assert_eq!(removed.send_id, queued_send_id);
    assert_eq!(removed.prompt, "queued");

    // Removing it again (now unknown) → NotQueued.
    assert!(matches!(
        dispatcher.remove_queued_message(agent.id, queued_id).await,
        Err(NotQueued)
    ));

    // Let the blocker finish; only the blocker's turn ever started.
    dispatcher.cancel(agent.id, CancelSource::User);
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    assert_eq!(
        count_type(&emitter.snapshot(), "turn_start"),
        1,
        "the removed message never dispatched"
    );
}

#[tokio::test]
async fn remove_queued_message_for_unknown_agent_is_not_queued() {
    let dispatcher = Arc::new(Dispatcher::new());
    let agent = agent_record();
    assert!(matches!(
        dispatcher
            .remove_queued_message(agent.id, Uuid::now_v7())
            .await,
        Err(NotQueued)
    ));
}

#[tokio::test]
async fn cancelling_in_flight_turn_leaves_backlog_intact() {
    // Cancelling the in-flight turn does not drop a queued message — it still
    // dispatches after the cancel.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    // First (running) turn parks until cancelled; the queued second streams.
    let factory = TestFactory::sequence(
        [MockScenario::AwaitCancellation, MockScenario::Streaming],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(
            agent.id,
            "running",
            Uuid::now_v7(),
            Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "running turn_start",
        emitter.wait_for_type("turn_start", 1),
    )
    .await;

    let queued_id = accepted(
        dispatcher
            .send_message(agent.id, "queued", Uuid::now_v7(), factory, OnBusy::Enqueue)
            .await,
    );

    dispatcher.cancel(agent.id, CancelSource::User);

    // The queued message dispatches after the cancel — two turn_starts, idle.
    within(
        &emitter,
        "queued dispatched after cancel",
        emitter.wait_for(|e| count_type(e, "turn_start") >= 2 && count_type(e, "agent_idle") >= 1),
    )
    .await;

    let events = emitter.snapshot();
    let second_start = events
        .iter()
        .filter(|(_, v)| event_type(v) == "turn_start")
        .nth(1)
        .expect("a second turn_start");
    assert_eq!(extract_message_id(&second_start.1), queued_id);
    // The first turn's terminal is cancelled; the second completes.
    let statuses: Vec<String> = events
        .iter()
        .filter(|(_, v)| event_type(v) == "turn_end")
        .map(|(_, v)| v["outcome"]["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        statuses,
        vec!["cancelled".to_owned(), "completed".to_owned()]
    );
}

#[tokio::test]
async fn cancel_fires_token_mid_turn_without_waiting_for_natural_finish() {
    // AwaitCancellation parks forever until its token fires; cancel must fire
    // the token so the turn ends promptly (within the timeout) rather than
    // hanging. Reaching agent_idle at all proves the token was honored.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::AwaitCancellation,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(
            agent.id,
            "parks forever",
            Uuid::now_v7(),
            factory,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "turn_start",
        emitter.wait_for_type("turn_start", 1),
    )
    .await;

    dispatcher.cancel(agent.id, CancelSource::User);
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    let statuses: Vec<String> = emitter
        .snapshot()
        .into_iter()
        .filter(|(_, v)| event_type(v) == "turn_end")
        .map(|(_, v)| v["outcome"]["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(statuses, vec!["cancelled".to_owned()]);
}

#[tokio::test]
async fn shutdown_agent_abandons_queued_message() {
    // With a queued message behind a running (AwaitCancellation) turn,
    // shutdown_agent returns and the queued message is NEVER dispatched.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let running = TestFactory::new(
        MockScenario::AwaitCancellation,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(
            agent.id,
            "running",
            Uuid::now_v7(),
            running,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "running turn_start",
        emitter.wait_for_type("turn_start", 1),
    )
    .await;

    let queued = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );
    dispatcher
        .send_message(agent.id, "queued", Uuid::now_v7(), queued, OnBusy::Enqueue)
        .await;

    // shutdown_agent returns once the actor has fully stopped.
    tokio::time::timeout(
        WAIT,
        dispatcher.shutdown_agent(agent.id, CancelSource::Shutdown),
    )
    .await
    .expect("shutdown_agent returns within the timeout");

    // The queued message never dispatched: only the running turn ever started.
    assert_eq!(
        count_type(&emitter.snapshot(), "turn_start"),
        1,
        "the backlog was abandoned on shutdown — no second TurnStart"
    );
}

#[tokio::test]
async fn send_racing_shutdown_is_not_resurrected() {
    // The dispatcher's teardown is resurrection-atomic: a send racing
    // `shutdown_agent` must NOT spawn a fresh actor that drives the same harness
    // session concurrently with the draining turn. Whether the racing send lands
    // just before the `Closing` mark (enqueued onto the old actor, then abandoned
    // on shutdown) or just after (rejected outright), the deterministic invariant
    // is the same: **no second `TurnStart` ever appears** — only the original
    // running turn started, and it ends cancelled.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let running = TestFactory::new(
        MockScenario::AwaitCancellation,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(
            agent.id,
            "running",
            Uuid::now_v7(),
            running,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "running turn_start",
        emitter.wait_for_type("turn_start", 1),
    )
    .await;

    // Race a fresh send against shutdown. A `Streaming` racer would, if it ever
    // started, produce a second turn_start — so its absence proves no resurrection.
    let racer = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );
    let shutdown = dispatcher.shutdown_agent(agent.id, CancelSource::Shutdown);
    let send = dispatcher.send_message(agent.id, "racer", Uuid::now_v7(), racer, OnBusy::Enqueue);
    let ((), _) = tokio::join!(
        async {
            tokio::time::timeout(WAIT, shutdown)
                .await
                .expect("shutdown converges");
        },
        send,
    );

    // Settle on the original turn's cancelled terminal.
    within(
        &emitter,
        "cancelled terminal",
        emitter.wait_for(|e| {
            e.iter()
                .any(|(_, v)| event_type(v) == "turn_end" && v["outcome"]["status"] == "cancelled")
        }),
    )
    .await;

    let snapshot = emitter.snapshot();
    assert_eq!(
        count_type(&snapshot, "turn_start"),
        1,
        "no fresh turn started during teardown — the racing send was rejected/abandoned, not resurrected"
    );
    assert_eq!(
        snapshot
            .iter()
            .filter(|(_, v)| event_type(v) == "turn_end" && v["outcome"]["status"] == "cancelled")
            .count(),
        1,
        "exactly one cancelled terminal — the original running turn"
    );
}

#[tokio::test]
async fn exactly_one_agent_idle_for_a_two_deep_queue() {
    // With a 2-deep queue, exactly ONE agent_idle (after the last turn); between
    // turns only TurnEnd → TurnStart, no interleaved agent_idle.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    for prompt in ["one", "two"] {
        dispatcher
            .send_message(
                agent.id,
                prompt,
                Uuid::now_v7(),
                Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
                OnBusy::Enqueue,
            )
            .await;
    }

    within(
        &emitter,
        "both turns drained",
        emitter.wait_for(|e| count_type(e, "turn_end") >= 2 && count_type(e, "agent_idle") >= 1),
    )
    .await;

    let events = emitter.snapshot();
    assert_eq!(
        count_type(&events, "agent_idle"),
        1,
        "exactly one AgentIdle for the whole 2-deep chain"
    );
    // agent_idle is the very last event; no idle appears between the two turns.
    let types: Vec<&str> = events.iter().map(|(_, v)| event_type(v)).collect();
    assert_eq!(types.last().copied(), Some("agent_idle"));
    let first_turn_end = types.iter().position(|t| *t == "turn_end").unwrap();
    assert!(
        !types[..first_turn_end].contains(&"agent_idle"),
        "no agent_idle before the first turn ends"
    );
    // After the first turn_end, the next non-content event is a turn_start (not
    // an interleaved idle).
    assert!(
        types[first_turn_end + 1..].contains(&"turn_start"),
        "the second turn starts after the first ends"
    );
}

#[tokio::test]
async fn send_message_id_equals_correlated_turn_start_message_id() {
    // Wire contract: the message_id returned by send_message equals the
    // message_id on the correlated TurnStart.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    let message_id = accepted(
        dispatcher
            .send_message(agent.id, "hello", Uuid::now_v7(), factory, OnBusy::Enqueue)
            .await,
    );
    within(
        &emitter,
        "turn_start",
        emitter.wait_for_type("turn_start", 1),
    )
    .await;

    let start = emitter
        .snapshot()
        .into_iter()
        .find(|(_, v)| event_type(v) == "turn_start")
        .expect("a turn_start");
    assert_eq!(extract_message_id(&start.1), message_id);
}

#[tokio::test]
async fn mid_turn_enqueue_chains_without_interleaved_agent_idle() {
    // Enqueue the second message *after* the first turn is already running (not
    // up front), then end the first turn — the second must chain straight on
    // with NO `AgentIdle` flickering out between them. This exercises the
    // mid-turn-enqueue + post-turn command-drain path that the up-front-queue
    // test (`exactly_one_agent_idle_for_a_two_deep_queue`) does not: the "no idle
    // between chained turns" contract must hold structurally, not by timing luck.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    // One per-actor factory handing back AwaitCancellation (turn 1, parks) then
    // Streaming (turn 2, completes).
    let factory = TestFactory::sequence(
        [MockScenario::AwaitCancellation, MockScenario::Streaming],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    dispatcher
        .send_message(
            agent.id,
            "first",
            Uuid::now_v7(),
            Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "first turn_start",
        emitter.wait_for_type("turn_start", 1),
    )
    .await;

    // Second send arrives mid-first-turn → enqueued behind the running turn.
    dispatcher
        .send_message(
            agent.id,
            "second",
            Uuid::now_v7(),
            Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    // End the first turn; the second must chain on.
    dispatcher.cancel(agent.id, CancelSource::User);

    within(
        &emitter,
        "both turns started + idle",
        emitter.wait_for(|e| count_type(e, "turn_start") >= 2 && count_type(e, "agent_idle") >= 1),
    )
    .await;

    let events = emitter.snapshot();
    let types: Vec<&str> = events.iter().map(|(_, v)| event_type(v)).collect();
    assert_eq!(
        count_type(&events, "agent_idle"),
        1,
        "exactly one AgentIdle for the whole chain — none flickered between the two turns"
    );
    assert_eq!(
        types.last().copied(),
        Some("agent_idle"),
        "AgentIdle is the final event"
    );
    let idle_pos = types.iter().position(|t| *t == "agent_idle").unwrap();
    assert_eq!(
        types[..idle_pos]
            .iter()
            .filter(|t| **t == "turn_start")
            .count(),
        2,
        "both turns started before the single AgentIdle (chained, not idled-then-redispatched)"
    );
}

#[tokio::test]
async fn cancel_after_completed_turn_writes_no_cancelled_outcome() {
    // Partition integrity (the reason the `select!` tie-break is "an
    // already-observed terminal beats a later cancel", not the reverse): a
    // completed turn's content lives in the harness session file, so the journal
    // must hold NO outcome marker for it. A cancel arriving after the turn
    // already completed is a no-op and must not synthesize a Cancelled terminal
    // or write a Cancelled marker — doing so would double-source the same turn on
    // reload (harness file + journal marker).
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let journal = Arc::new(RecordingJournal::default());
    let agent = agent_record();
    let factory = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        Arc::clone(&journal) as Arc<dyn ConversationJournal>,
    );

    dispatcher
        .send_message(agent.id, "hello", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "agent_idle (turn fully completed)",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    // The turn completed and the agent is idle — this cancel is post-terminal.
    dispatcher.cancel(agent.id, CancelSource::User);
    // Give the no-op cancel a chance to (wrongly) produce a terminal if the guard
    // were broken: wait until the actor has processed it (a second, benign idle).
    within(
        &emitter,
        "post-cancel settle",
        emitter.wait_for(|e| count_type(e, "agent_idle") >= 2),
    )
    .await;

    let events = emitter.snapshot();
    assert!(
        !events
            .iter()
            .any(|(_, v)| event_type(v) == "turn_end" && v["outcome"]["status"] == "cancelled"),
        "no synthesized Cancelled for an already-completed turn"
    );
    assert!(
        journal.outcomes.lock().unwrap().is_empty(),
        "a completed turn writes no outcome marker — even after a post-terminal cancel — so the journal never double-sources a turn whose content is in the harness file"
    );
    assert_eq!(
        journal.sends.lock().unwrap().len(),
        1,
        "the send was journaled at turn-start"
    );
}

// ── cancel_send (send-scoped cancellation across a fan-out's recipients) ──────
//
// A fan-out is N independent turns sharing one `send_id` (one per recipient
// agent). `cancel_send` must stop exactly that send's turns — cancelling an
// in-flight turn iff its `send_id` matches and dropping a still-queued item of
// the send — while never touching a recipient's *later, unrelated* turn (the
// TOCTOU a naive frontend per-agent `cancel_turn` loop would hit).

/// Sources of every synthesized `Cancelled` terminal recorded on an emitter.
fn cancelled_sources(emitter: &RecordingEmitter) -> Vec<String> {
    emitter
        .snapshot()
        .iter()
        .filter(|(_, v)| event_type(v) == "turn_end" && v["outcome"]["status"] == "cancelled")
        .map(|(_, v)| v["outcome"]["source"].as_str().unwrap_or("?").to_owned())
        .collect()
}

#[tokio::test]
async fn cancel_send_cancels_every_in_flight_turn_of_the_send() {
    // Two recipients, both in-flight on the same send. cancel_send stops both.
    let dispatcher = Arc::new(Dispatcher::new());
    let agent_a = agent_record();
    let agent_b = agent_record();
    let emitter_a = Arc::new(RecordingEmitter::new());
    let emitter_b = Arc::new(RecordingEmitter::new());
    let factory_a = TestFactory::new(
        MockScenario::AwaitCancellation,
        agent_a.clone(),
        Arc::clone(&emitter_a),
        noop_journal(),
    );
    let factory_b = TestFactory::new(
        MockScenario::AwaitCancellation,
        agent_b.clone(),
        Arc::clone(&emitter_b),
        noop_journal(),
    );

    let send_id = Uuid::now_v7();
    dispatcher
        .send_message(
            agent_a.id,
            "to A and B",
            send_id,
            factory_a as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    dispatcher
        .send_message(
            agent_b.id,
            "to A and B",
            send_id,
            factory_b as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter_a,
        "A turn_start",
        emitter_a.wait_for_type("turn_start", 1),
    )
    .await;
    within(
        &emitter_b,
        "B turn_start",
        emitter_b.wait_for_type("turn_start", 1),
    )
    .await;

    dispatcher.cancel_send(send_id, &[agent_a.id, agent_b.id], CancelSource::User);

    within(
        &emitter_a,
        "A agent_idle",
        emitter_a.wait_for_type("agent_idle", 1),
    )
    .await;
    within(
        &emitter_b,
        "B agent_idle",
        emitter_b.wait_for_type("agent_idle", 1),
    )
    .await;
    assert_eq!(
        cancelled_sources(&emitter_a),
        vec!["user"],
        "A's turn is cancelled by the user-sourced cancel_send"
    );
    assert_eq!(
        cancelled_sources(&emitter_b),
        vec!["user"],
        "B's turn is cancelled by the same cancel_send"
    );
}

#[tokio::test]
async fn cancel_send_is_scoped_and_spares_a_later_unrelated_turn() {
    // The scoping guard. Recipient A finishes the send's turn and starts a
    // LATER, unrelated turn (different send_id); recipient B is still on the
    // send. cancel_send(send_id) must cancel only B — A's later turn untouched.
    let dispatcher = Arc::new(Dispatcher::new());
    let agent_a = agent_record();
    let agent_b = agent_record();
    let emitter_a = Arc::new(RecordingEmitter::new());
    let emitter_b = Arc::new(RecordingEmitter::new());
    // A: first turn streams to completion, second turn parks (in-flight).
    let factory_a = TestFactory::sequence(
        [MockScenario::Streaming, MockScenario::AwaitCancellation],
        agent_a.clone(),
        Arc::clone(&emitter_a),
        noop_journal(),
    );
    let factory_b = TestFactory::new(
        MockScenario::AwaitCancellation,
        agent_b.clone(),
        Arc::clone(&emitter_b),
        noop_journal(),
    );

    let send_id = Uuid::now_v7();
    let later_send_id = Uuid::now_v7();

    // A's turn on the send completes.
    dispatcher
        .send_message(
            agent_a.id,
            "the send",
            send_id,
            factory_a.clone() as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter_a,
        "A first idle",
        emitter_a.wait_for_type("agent_idle", 1),
    )
    .await;
    // A starts a later, unrelated turn (different send), now in-flight.
    dispatcher
        .send_message(
            agent_a.id,
            "unrelated follow-up",
            later_send_id,
            factory_a as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter_a,
        "A second turn_start",
        emitter_a.wait_for_type("turn_start", 2),
    )
    .await;
    // B is still on the send, in-flight.
    dispatcher
        .send_message(
            agent_b.id,
            "the send",
            send_id,
            factory_b as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter_b,
        "B turn_start",
        emitter_b.wait_for_type("turn_start", 1),
    )
    .await;

    dispatcher.cancel_send(send_id, &[agent_a.id, agent_b.id], CancelSource::User);

    within(
        &emitter_b,
        "B agent_idle",
        emitter_b.wait_for_type("agent_idle", 1),
    )
    .await;
    assert_eq!(
        cancelled_sources(&emitter_b),
        vec!["user"],
        "B (still on the send) is cancelled"
    );

    // Prove A's later turn was spared *deterministically* via the cancel
    // source. `cancel_send` uses `User`; the direct cancel below uses
    // `Shutdown`. A's later turn can only be cancelled once, so its single
    // cancelled terminal's source tells us who cancelled it: if the scoping
    // guard were broken, `cancel_send` (User) would have won and the source
    // would read "user". A correct guard means only the direct `Shutdown`
    // cancel reaches it. This avoids the timing gap of checking
    // `is_empty()` before A's `CancelSend` was even processed.
    dispatcher.cancel(agent_a.id, CancelSource::Shutdown);
    within(
        &emitter_a,
        "A second idle",
        emitter_a.wait_for_type("agent_idle", 2),
    )
    .await;
    assert_eq!(
        cancelled_sources(&emitter_a),
        vec!["shutdown"],
        "A's later turn was cancelled only by the direct Shutdown cancel — \
         cancel_send (User) for the earlier send did not touch it"
    );
}

#[tokio::test]
async fn cancel_send_cancels_in_flight_and_removes_a_queued_recipient_of_the_send() {
    // Recipient X is in-flight on the send; recipient Y is busy on an unrelated
    // turn with the send's message QUEUED behind it. cancel_send cancels X and
    // removes Y's queued item — which therefore never dispatches — without
    // touching Y's unrelated in-flight turn.
    let dispatcher = Arc::new(Dispatcher::new());
    let agent_x = agent_record();
    let agent_y = agent_record();
    let emitter_x = Arc::new(RecordingEmitter::new());
    let emitter_y = Arc::new(RecordingEmitter::new());
    let factory_x = TestFactory::new(
        MockScenario::AwaitCancellation,
        agent_x.clone(),
        Arc::clone(&emitter_x),
        noop_journal(),
    );
    // Y: first (unrelated) turn parks; if the queued send item ever dispatched
    // it would stream — proving via its ABSENCE that removal worked.
    let factory_y = TestFactory::sequence(
        [MockScenario::AwaitCancellation, MockScenario::Streaming],
        agent_y.clone(),
        Arc::clone(&emitter_y),
        noop_journal(),
    );

    let send_id = Uuid::now_v7();
    let unrelated_send_id = Uuid::now_v7();

    // Y is busy on an unrelated turn.
    dispatcher
        .send_message(
            agent_y.id,
            "Y unrelated",
            unrelated_send_id,
            factory_y as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter_y,
        "Y turn_start",
        emitter_y.wait_for_type("turn_start", 1),
    )
    .await;
    // The fan-out: X in-flight, Y enqueued behind its unrelated turn.
    dispatcher
        .send_message(
            agent_y.id,
            "the send",
            send_id,
            // Factory ignored — Y's actor already exists and owns its builder.
            TestFactory::new(
                MockScenario::Streaming,
                agent_y.clone(),
                Arc::clone(&emitter_y),
                noop_journal(),
            ) as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    dispatcher
        .send_message(
            agent_x.id,
            "the send",
            send_id,
            factory_x as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter_x,
        "X turn_start",
        emitter_x.wait_for_type("turn_start", 1),
    )
    .await;

    dispatcher.cancel_send(send_id, &[agent_x.id, agent_y.id], CancelSource::User);

    within(
        &emitter_x,
        "X agent_idle",
        emitter_x.wait_for_type("agent_idle", 1),
    )
    .await;
    assert_eq!(
        cancelled_sources(&emitter_x),
        vec!["user"],
        "X's in-flight turn on the send is cancelled"
    );

    // Free Y's unrelated turn; with the queued send item removed, Y goes idle
    // WITHOUT ever dispatching it.
    dispatcher.cancel(agent_y.id, CancelSource::User);
    within(
        &emitter_y,
        "Y agent_idle",
        emitter_y.wait_for_type("agent_idle", 1),
    )
    .await;
    assert_eq!(
        count_type(&emitter_y.snapshot(), "turn_start"),
        1,
        "Y's queued send item was removed and never dispatched (only its unrelated turn started)"
    );
}

#[tokio::test]
async fn cancel_send_for_a_completed_send_is_a_noop() {
    // Every turn of the send already finished. cancel_send is a no-op — no new
    // cancelled terminal, no extra events.
    let dispatcher = Arc::new(Dispatcher::new());
    let agent = agent_record();
    let emitter = Arc::new(RecordingEmitter::new());
    let factory = TestFactory::new(
        MockScenario::Streaming,
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    let send_id = Uuid::now_v7();
    dispatcher
        .send_message(agent.id, "quick", send_id, factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    dispatcher.cancel_send(send_id, &[agent.id], CancelSource::User);

    // Deterministic drain instead of a wall-clock sleep: dispatch a fresh turn.
    // The actor processes commands in order, so by the time this second turn
    // starts it has already processed (and no-op'd) the CancelSend — any
    // erroneous re-cancel would have fired before this turn_start.
    dispatcher
        .send_message(
            agent.id,
            "next",
            Uuid::now_v7(),
            TestFactory::new(
                MockScenario::Streaming,
                agent.clone(),
                Arc::clone(&emitter),
                noop_journal(),
            ),
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "second turn_start",
        emitter.wait_for_type("turn_start", 2),
    )
    .await;

    assert!(
        cancelled_sources(&emitter).is_empty(),
        "cancel_send on a fully-completed send produces no cancelled terminal"
    );
}

#[tokio::test]
async fn cancel_agent_cancels_in_flight_clears_backlog_and_stays_alive() {
    // "Stop agent": one in-flight turn + one queued behind it. cancel_agent
    // cancels the running turn AND drops the queued item (so it never
    // dispatches), but the actor survives — a later send dispatches normally.
    let dispatcher = Arc::new(Dispatcher::new());
    let agent = agent_record();
    let emitter = Arc::new(RecordingEmitter::new());
    // Turn 1 parks until cancelled; turn 2 (the queued one) would Stream if it
    // ever dispatched — its ABSENCE proves the backlog was cleared. Turn 3 (the
    // later send) streams to prove the actor is still alive.
    let factory = TestFactory::sequence(
        [MockScenario::AwaitCancellation, MockScenario::Streaming],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
    );

    // Turn 1 in-flight.
    dispatcher
        .send_message(
            agent.id,
            "first",
            Uuid::now_v7(),
            factory as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "turn 1 start",
        emitter.wait_for_type("turn_start", 1),
    )
    .await;
    // Turn 2 queued behind it (distinct send).
    dispatcher
        .send_message(
            agent.id,
            "queued",
            Uuid::now_v7(),
            TestFactory::new(
                MockScenario::Streaming,
                agent.clone(),
                Arc::clone(&emitter),
                noop_journal(),
            ) as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;

    dispatcher.cancel_agent(agent.id, CancelSource::User);

    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;
    assert_eq!(
        cancelled_sources(&emitter),
        vec!["user"],
        "the in-flight turn is cancelled by cancel_agent"
    );
    assert_eq!(
        count_type(&emitter.snapshot(), "turn_start"),
        1,
        "the queued turn was dropped and never dispatched"
    );
    assert_eq!(
        count_type(&emitter.snapshot(), "message_cancelled"),
        1,
        "the dropped queued send emits MessageCancelled so the UI renders it (not the running turn, which gets a Cancelled terminal)"
    );

    // The actor is still alive: a fresh send dispatches normally.
    dispatcher
        .send_message(
            agent.id,
            "after stop",
            Uuid::now_v7(),
            TestFactory::new(
                MockScenario::Streaming,
                agent.clone(),
                Arc::clone(&emitter),
                noop_journal(),
            ) as Arc<dyn DispatchContextFactory>,
            OnBusy::Enqueue,
        )
        .await;
    within(
        &emitter,
        "post-stop turn start",
        emitter.wait_for_type("turn_start", 2),
    )
    .await;
}

#[tokio::test]
async fn captured_locator_is_persisted_and_turn_completes() {
    // A SessionLocatorCaptured event (Codex/Antigravity learning its locator at
    // runtime) is persisted via the injected sink and is NOT forwarded to the
    // frontend; the turn completes normally.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let locator = SessionLocator::Uuid(Uuid::now_v7());
    let sink = Arc::new(RecordingLocatorSink::default());
    let factory = TestFactory::sequence_with_locator_sink(
        [MockScenario::CapturesLocator(locator.clone())],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
        Arc::new(NoopMetadataCache),
        Arc::clone(&sink) as Arc<dyn SessionLocatorSink>,
    );

    dispatcher
        .send_message(agent.id, "hello", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    let persisted = sink.persisted.lock().unwrap();
    assert_eq!(
        persisted.len(),
        1,
        "the sink is invoked once per capture event"
    );
    assert_eq!(persisted[0], (agent.id, locator));

    let snapshot = emitter.snapshot();
    // The turn completed...
    assert!(
        snapshot
            .iter()
            .any(|(_, v)| event_type(v) == "turn_end" && v["outcome"]["status"] == "completed"),
        "turn completes after a successful capture"
    );
    // ...and the internal capture event never reached the wire.
    assert!(
        !snapshot
            .iter()
            .any(|(_, v)| event_type(v) == "session_locator_captured"),
        "the capture event must not be forwarded to the frontend"
    );
}

#[tokio::test]
async fn codex_variant_capture_flows_through_the_same_sink() {
    // The capture path is locator-variant-agnostic: a `Codex` locator persists
    // through the same dispatcher event + sink as the `Uuid` (Antigravity) one,
    // with no `match harness` anywhere in the dispatcher.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let locator = SessionLocator::Codex {
        thread_id: "thread-abc".to_owned(),
        partition_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
    };
    let sink = Arc::new(RecordingLocatorSink::default());
    let factory = TestFactory::sequence_with_locator_sink(
        [MockScenario::CapturesLocator(locator.clone())],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
        Arc::new(NoopMetadataCache),
        Arc::clone(&sink) as Arc<dyn SessionLocatorSink>,
    );

    dispatcher
        .send_message(agent.id, "hello", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    let persisted = sink.persisted.lock().unwrap();
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0], (agent.id, locator));
}

#[tokio::test]
async fn capture_persist_failure_fails_the_turn() {
    // The load-bearing distinction from MetadataCache: a persist failure on
    // capture fails the turn (a lost locator silently starts a fresh session
    // next turn), rather than being swallowed.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let factory = TestFactory::sequence_with_locator_sink(
        [MockScenario::CapturesLocator(SessionLocator::Uuid(
            Uuid::now_v7(),
        ))],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
        Arc::new(NoopMetadataCache),
        Arc::new(FailingLocatorSink),
    );

    dispatcher
        .send_message(agent.id, "hello", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    let snapshot = emitter.snapshot();
    let terminal = snapshot
        .iter()
        .find(|(_, v)| event_type(v) == "turn_end")
        .expect("a turn_end terminal");
    assert_eq!(
        terminal.1["outcome"]["status"], "failed",
        "a capture-persist failure fails the turn"
    );
    assert_eq!(terminal.1["outcome"]["kind"], "adapter_failure");
    assert!(
        !snapshot
            .iter()
            .any(|(_, v)| event_type(v) == "turn_end" && v["outcome"]["status"] == "completed"),
        "the turn must not also complete"
    );
}

#[tokio::test]
async fn capture_persist_failure_suppresses_subsequent_stream_events() {
    // After a force-failed turn, the adapter may keep emitting (Antigravity's
    // post-exit drain emits content + terminal after the capture). None of it
    // may reach the wire — the turn is authoritatively over, and a "failed"
    // terminal followed by more content would corrupt the lifecycle contract.
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let factory = TestFactory::sequence_with_locator_sink(
        // capture FIRST, then content + a completed terminal.
        [MockScenario::CapturesLocatorThenContent(
            SessionLocator::Uuid(Uuid::now_v7()),
        )],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
        Arc::new(NoopMetadataCache),
        Arc::new(FailingLocatorSink),
    );

    dispatcher
        .send_message(agent.id, "hello", Uuid::now_v7(), factory, OnBusy::Enqueue)
        .await;
    within(
        &emitter,
        "agent_idle",
        emitter.wait_for_type("agent_idle", 1),
    )
    .await;

    let snapshot = emitter.snapshot();
    // Exactly one terminal, and it's the synthesized failure.
    let terminals: Vec<_> = snapshot
        .iter()
        .filter(|(_, v)| event_type(v) == "turn_end")
        .collect();
    assert_eq!(terminals.len(), 1, "exactly one terminal");
    assert_eq!(terminals[0].1["outcome"]["status"], "failed");
    // No post-capture content leaked past the failed terminal.
    assert!(
        !snapshot
            .iter()
            .any(|(_, v)| event_type(v) == "content_chunk"),
        "content emitted after the capture must be suppressed on a force-failed turn"
    );
}

#[tokio::test]
async fn repeated_capture_events_each_persist_and_are_not_deduped() {
    // The fork-and-heal shape at the dispatcher layer: a capture event on a
    // second turn re-invokes the sink — the dispatcher never suppresses a
    // capture as a duplicate (else a healed locator would be dropped).
    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let agent = agent_record();
    let sink = Arc::new(RecordingLocatorSink::default());
    let factory = TestFactory::sequence_with_locator_sink(
        [
            MockScenario::CapturesLocator(SessionLocator::Uuid(Uuid::now_v7())),
            MockScenario::CapturesLocator(SessionLocator::Uuid(Uuid::now_v7())),
        ],
        agent.clone(),
        Arc::clone(&emitter),
        noop_journal(),
        Arc::new(NoopMetadataCache),
        Arc::clone(&sink) as Arc<dyn SessionLocatorSink>,
    );

    // Send each turn only after the prior drained to idle, so they run as two
    // distinct turns (back-to-back enqueues would chain into one drain and emit
    // a single agent_idle).
    for n in 1..=2 {
        dispatcher
            .send_message(
                agent.id,
                "hello",
                Uuid::now_v7(),
                Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
                OnBusy::Enqueue,
            )
            .await;
        within(
            &emitter,
            "agent_idle",
            emitter.wait_for_type("agent_idle", n),
        )
        .await;
    }

    assert_eq!(
        sink.persisted.lock().unwrap().len(),
        2,
        "each turn's capture event persists independently — never deduped"
    );
}
