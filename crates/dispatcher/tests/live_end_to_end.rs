//! Live end-to-end integration tests against real `claude`, `codex`, and
//! `agy`.
//!
//! Exercises the **full backend vertical slice** that a user actually
//! triggers: `Store::create_project` → `register_agent` →
//! `Dispatcher::send_message` → real subprocess → events streamed back
//! through the `EventEmitter`. Uses realistic on-disk paths so any
//! path-encoding rule or cwd-semantic decision is exercised against the
//! actual layout.
//!
//! Why this layer matters: pure unit tests and adapter-only live tests can
//! pass while the integration path still has a bug. Concrete regressions
//! this layer guards against:
//!
//! - The session-id encoding bug (`/` → `-` only, missing `. → -`) —
//!   detected by `live_claude_full_stack_two_consecutive_turns_succeed`, which
//!   exercises session resume across two Claude turns.
//! - The cwd bug (claude was spawned in `.switchboard/projects/<uuid>/`
//!   instead of the user's bound working directory, so it couldn't see the
//!   user's repo files) — detected by `live_claude_full_stack_sees_files_in_cwd`,
//!   which writes a file into the working dir and asserts claude can read it.
//! - A future dispatcher branch that's secretly harness-specific — the
//!   per-harness event-ordering checks
//!   (`live_claude_full_stack_emits_turn_start_then_content_then_turn_end` for
//!   Claude,
//!   `live_codex_full_stack_emits_turn_start_then_content_then_turn_end`,
//!   assert the same `turn_start → content_chunk → turn_end → agent_idle`
//!   contract holds through every harness's real subprocess. Any accidental
//!   coupling of the dispatcher to one harness's behavior surfaces in the
//!   other tests.
//!
//! Turn/chain completion is awaited deterministically off the event stream
//! (`RecordingEmitter::wait_for_type("agent_idle", n)`) under a timeout —
//! there is no per-send join handle in the actor model.
//!
//! Run with: `make test-live`. Gated behind `#[ignore]` because each test
//! costs real credits and requires the corresponding CLI installed and
//! authenticated.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use switchboard_core::{
    AgentRecord, Attachment, AttachmentKind, HarnessKind, Project, SendId, Store,
};
use switchboard_dispatcher::{
    ConversationJournal, DispatchContext, DispatchContextFactory, Dispatcher, EventEmitter,
    NoopJournal, NoopMetadataCache, NoopSessionLocatorSink, OnBusy, RecordingEmitter, SendOutcome,
};
use switchboard_harness::{
    AntigravityAdapter, CancelSource, ClaudeCodeAdapter, CodexAdapter, DispatchOptions,
    HarnessAdapter,
};
use tempfile::TempDir;
use uuid::Uuid;

/// Generous deadline for a real-CLI turn (spawn + model latency). A logic bug
/// surfaces as a bounded timeout with the recorded events, not a hang.
#[allow(
    clippy::duration_suboptimal_units,
    reason = "from_mins is unstable on the pinned toolchain"
)]
const LIVE_WAIT: Duration = Duration::from_secs(60);

/// The capturing factory the live tests build: it freezes the real adapter,
/// agent record, emitter, and (no-op) journal, handing the actor a fresh
/// `DispatchContext` per turn — the app-side factory's contract, against a
/// real harness adapter.
struct LiveFactory {
    adapter: Arc<dyn HarnessAdapter>,
    cwd: PathBuf,
    agent: AgentRecord,
    emitter: Arc<dyn EventEmitter>,
}

impl LiveFactory {
    fn new(
        adapter: Arc<dyn HarnessAdapter>,
        cwd: PathBuf,
        agent: AgentRecord,
        emitter: Arc<RecordingEmitter>,
    ) -> Arc<Self> {
        Arc::new(Self {
            adapter,
            cwd,
            agent,
            emitter: emitter as Arc<dyn EventEmitter>,
        })
    }
}

impl DispatchContextFactory for LiveFactory {
    fn selection_snapshot(&self) -> Option<switchboard_dispatcher::SelectionSnapshot> {
        None
    }

    fn build(&self, _send_id: SendId) -> DispatchContext {
        DispatchContext {
            adapter: Arc::clone(&self.adapter),
            cwd: self.cwd.clone(),
            agent: self.agent.clone(),
            emitter: Arc::clone(&self.emitter),
            options: DispatchOptions::default(),
            journal: noop_journal(),
            metadata: Arc::new(NoopMetadataCache),
            locator_sink: Arc::new(NoopSessionLocatorSink),
        }
    }

    fn idle_emitter(&self) -> Arc<dyn EventEmitter> {
        Arc::clone(&self.emitter)
    }
}

/// Live tests assert on real-harness stream behavior, not journaling.
fn noop_journal() -> Arc<dyn ConversationJournal> {
    Arc::new(NoopJournal)
}

/// Block until `count` `agent_idle` events have been recorded, under the live
/// timeout — the actor-model replacement for `handle.join.await`.
async fn wait_for_idles(emitter: &Arc<RecordingEmitter>, count: usize) {
    tokio::time::timeout(LIVE_WAIT, emitter.wait_for_type("agent_idle", count))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for {count} agent_idle event(s); events: {:?}",
                emitter.snapshot()
            )
        });
}

/// Unwrap an `Accepted` send (the Enqueue path never returns `Busy`).
fn expect_accepted(outcome: SendOutcome, label: &str) {
    assert!(
        matches!(outcome, SendOutcome::Accepted(_)),
        "{label} must be accepted; got {outcome:?}"
    );
}

/// Extracts the `outcome.status` strings from every `turn_end` event the
/// emitter saw on the given channel, in arrival order.
fn turn_end_statuses(emitter: &Arc<RecordingEmitter>, channel: &str) -> Vec<String> {
    emitter
        .snapshot()
        .into_iter()
        .filter(|(name, payload)| name == channel && payload["type"] == "turn_end")
        .filter_map(|(_, payload)| payload["outcome"]["status"].as_str().map(str::to_owned))
        .collect()
}

/// Concatenates every `content_chunk.text` for a channel in arrival order.
fn agent_text(emitter: &Arc<RecordingEmitter>, channel: &str) -> String {
    emitter
        .snapshot()
        .into_iter()
        .filter(|(name, payload)| name == channel && payload["type"] == "content_chunk")
        .filter_map(|(_, payload)| payload["text"].as_str().map(str::to_owned))
        .collect()
}

/// The wire-`type` sequence of an already-filtered payload list. A payload with
/// no `type` cannot come off the tagged wire enum; if one ever did it is dropped
/// rather than rendered as an empty string.
fn kind_sequence_of(events: &[serde_json::Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|p| p["type"].as_str().map(str::to_owned))
        .collect()
}

/// The wire-`type` sequence on a channel, in arrival order.
fn kind_sequence(emitter: &Arc<RecordingEmitter>, channel: &str) -> Vec<String> {
    let payloads: Vec<serde_json::Value> = emitter
        .snapshot()
        .into_iter()
        .filter(|(name, _)| name == channel)
        .map(|(_, payload)| payload)
        .collect();
    kind_sequence_of(&payloads)
}

/// The shared event-ordering contract every harness must satisfy:
/// `turn_start` first, `agent_idle` last, exactly one `turn_end`, exactly one
/// `agent_idle`, `turn_end` before `agent_idle`, and at least one
/// `content_chunk`.
fn assert_ordering_contract(kinds: &[String]) {
    assert_eq!(
        kinds.first().map(String::as_str),
        Some("turn_start"),
        "first event on the channel must be turn_start; got: {kinds:?}"
    );
    assert_eq!(
        kinds.last().map(String::as_str),
        Some("agent_idle"),
        "last event must be agent_idle; got: {kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| *k == "turn_end").count(),
        1,
        "must be exactly one terminal event per turn; got: {kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| *k == "agent_idle").count(),
        1,
        "exactly one agent_idle per dispatch; got: {kinds:?}"
    );
    let turn_end_idx = kinds.iter().position(|k| k == "turn_end").unwrap();
    let agent_idle_idx = kinds.iter().position(|k| k == "agent_idle").unwrap();
    assert!(
        turn_end_idx < agent_idle_idx,
        "turn_end must precede agent_idle; got: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|k| k == "content_chunk"),
        "at least one content_chunk expected for a real completion; got: {kinds:?}"
    );
}

/// A project in a fresh temp store, rooted at `working_directory`.
///
/// The store root is its own `TempDir` and is returned so the caller keeps it
/// alive — nothing Switchboard-owned is written into the working directory any
/// more, so the project's files would vanish the moment the root dropped.
fn temp_project(working_directory: &std::path::Path, name: &str) -> (TempDir, Project) {
    let store_root = TempDir::new().expect("store root");
    let store = Store::open(store_root.path()).expect("open store");
    let project = store
        .create_project(working_directory, name)
        .expect("create_project");
    (store_root, project)
}

#[tokio::test]
#[ignore = "requires claude installed and authenticated — run with: make test-live"]
async fn live_claude_full_stack_two_consecutive_turns_succeed() {
    // Reproduces the full backend vertical slice. Two turns: the second is the
    // load-bearing assertion — the first creates the session file, and the
    // second must find it and switch from `--session-id` to `--resume`. A
    // path-encoding bug surfaces as the second turn failing with "Session ID …
    // is already in use". The two turns chain through the actor's FIFO backlog.
    let tmp = TempDir::new().expect("tempdir");
    let (_store_root, project) = temp_project(tmp.path(), "integration-test");
    let agent = project
        .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
        .expect("register_agent");
    assert!(
        agent.session_locator.is_some(),
        "ClaudeCode agents must have a pre-generated session_id"
    );

    let dispatcher = Arc::new(Dispatcher::new());
    let adapter: Arc<dyn HarnessAdapter> = Arc::new(ClaudeCodeAdapter::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let channel = format!("agent:{}", agent.id);
    let factory = LiveFactory::new(
        adapter,
        project.directory.clone(),
        agent.clone(),
        Arc::clone(&emitter),
    );

    // Turn 1: adapter passes --session-id (session file doesn't exist yet).
    expect_accepted(
        dispatcher
            .send_message(
                agent.id,
                "Reply with exactly the word: ack",
                vec![],
                Uuid::now_v7(),
                Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
                OnBusy::Enqueue,
            )
            .await,
        "first send",
    );
    // Wait for turn 1 to fully settle before sending turn 2, so the adapter's
    // session-file lookup on turn 2 sees the file turn 1 wrote.
    wait_for_idles(&emitter, 1).await;

    // Turn 2: the regression catch. Adapter must detect the session file
    // created by turn 1 and pass --resume (not --session-id).
    expect_accepted(
        dispatcher
            .send_message(
                agent.id,
                "And again, exactly: ack",
                vec![],
                Uuid::now_v7(),
                factory,
                OnBusy::Enqueue,
            )
            .await,
        "second send",
    );
    wait_for_idles(&emitter, 2).await;

    let statuses = turn_end_statuses(&emitter, &channel);
    assert_eq!(
        statuses,
        vec!["completed".to_owned(), "completed".to_owned()],
        "both turns must complete; got: {statuses:?}\nevents: {:?}",
        emitter.snapshot()
    );
}

#[tokio::test]
#[ignore = "requires claude installed and authenticated — run with: make test-live"]
async fn live_claude_full_stack_emits_turn_start_then_content_then_turn_end() {
    let tmp = TempDir::new().expect("tempdir");
    let (_store_root, project) = temp_project(tmp.path(), "order-test");
    let agent = project
        .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
        .expect("agent");

    let dispatcher = Arc::new(Dispatcher::new());
    let adapter: Arc<dyn HarnessAdapter> = Arc::new(ClaudeCodeAdapter::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let channel = format!("agent:{}", agent.id);
    let factory = LiveFactory::new(
        adapter,
        project.directory.clone(),
        agent.clone(),
        Arc::clone(&emitter),
    );

    expect_accepted(
        dispatcher
            .send_message(
                agent.id,
                "Reply with exactly: hi",
                vec![],
                Uuid::now_v7(),
                factory,
                OnBusy::Enqueue,
            )
            .await,
        "send",
    );
    wait_for_idles(&emitter, 1).await;

    assert_ordering_contract(&kind_sequence(&emitter, &channel));
}

#[tokio::test]
#[ignore = "requires claude installed and authenticated — run with: make test-live"]
async fn live_claude_full_stack_paths_with_dot_components_resolve_correctly() {
    // Direct regression test for the path-encoding bug. The user's bound
    // working directory can contain dots; the encoding rule must apply the same
    // way at any path position, otherwise the second-turn `--resume` lookup
    // fails.
    let tmp = TempDir::new().expect("tempdir");
    let working_dir = tmp.path().join(".config").join("my.app");
    std::fs::create_dir_all(&working_dir).expect("create working dir");
    let (_store_root, project) = temp_project(&working_dir, "dot-path-test");
    let agent = project
        .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
        .expect("agent");

    let dispatcher = Arc::new(Dispatcher::new());
    let adapter: Arc<dyn HarnessAdapter> = Arc::new(ClaudeCodeAdapter::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let channel = format!("agent:{}", agent.id);
    let factory = LiveFactory::new(
        adapter,
        project.directory.clone(),
        agent.clone(),
        Arc::clone(&emitter),
    );

    // Two consecutive turns, each awaited to idle before the next so the resume
    // lookup on turn 2 sees turn 1's session file.
    for (i, prompt) in ["Say 1", "Say 2"].iter().enumerate() {
        expect_accepted(
            dispatcher
                .send_message(
                    agent.id,
                    prompt,
                    vec![],
                    Uuid::now_v7(),
                    Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
                    OnBusy::Enqueue,
                )
                .await,
            "send",
        );
        wait_for_idles(&emitter, i + 1).await;
    }

    let statuses = turn_end_statuses(&emitter, &channel);
    assert_eq!(statuses.len(), 2, "expected two turn_end events");
    assert_eq!(
        statuses[1],
        "completed",
        "second turn must complete (proves session resume works through dot-path encoding); \
         got statuses: {statuses:?} from events: {:?}",
        emitter.snapshot()
    );
}

#[tokio::test]
#[ignore = "requires claude installed and authenticated — run with: make test-live"]
async fn live_claude_full_stack_sees_files_in_cwd() {
    // Regression test for the cwd-routing rule: claude must be spawned in the
    // user's bound working directory, NOT in `<dir>/.switchboard/projects/<uuid>/`.
    let tmp = TempDir::new().expect("tempdir");
    let token = "SWITCHBOARD_LIVE_TEST_TOKEN_F8A23E";
    std::fs::write(tmp.path().join("MARKER.txt"), token).expect("write marker");

    let (_store_root, project) = temp_project(tmp.path(), "cwd-test");
    let agent = project
        .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
        .expect("agent");

    let dispatcher = Arc::new(Dispatcher::new());
    let adapter: Arc<dyn HarnessAdapter> = Arc::new(ClaudeCodeAdapter::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let channel = format!("agent:{}", agent.id);
    let factory = LiveFactory::new(
        adapter,
        project.directory.clone(),
        agent.clone(),
        Arc::clone(&emitter),
    );

    expect_accepted(
        dispatcher
            .send_message(
                agent.id,
                "Read the file MARKER.txt in the current directory and tell me what string it contains. Reply with just the string, nothing else.",
                vec![],
                Uuid::now_v7(),
                factory,
                OnBusy::Enqueue,
            )
            .await,
        "send",
    );
    wait_for_idles(&emitter, 1).await;

    let text = agent_text(&emitter, &channel);
    assert!(
        text.contains(token),
        "claude's response must contain the marker token (proves it read the file from the cwd); \
         got text: {text:?}"
    );
}

#[tokio::test]
#[ignore = "requires codex installed and authenticated — run with: make test-live"]
async fn live_codex_full_stack_emits_turn_start_then_content_then_turn_end() {
    // Codex agents register with `session_locator = None` (the locator is
    // captured at runtime and persisted to the registry record); the dispatcher
    // must not depend on `agent.session_locator.is_some()` to function.
    let tmp = TempDir::new().expect("tempdir");
    let (_store_root, project) = temp_project(tmp.path(), "codex-order-test");
    let agent = project
        .register_agent("assistant", HarnessKind::Codex, None, None)
        .expect("agent");
    assert!(
        agent.session_locator.is_none(),
        "Codex agents must register with session_locator = None"
    );

    let dispatcher = Arc::new(Dispatcher::new());
    let adapter: Arc<dyn HarnessAdapter> = Arc::new(CodexAdapter::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let channel = format!("agent:{}", agent.id);
    let factory = LiveFactory::new(
        adapter,
        project.directory.clone(),
        agent.clone(),
        Arc::clone(&emitter),
    );

    expect_accepted(
        dispatcher
            .send_message(
                agent.id,
                "Reply with exactly: hi",
                vec![],
                Uuid::now_v7(),
                factory,
                OnBusy::Enqueue,
            )
            .await,
        "send",
    );
    wait_for_idles(&emitter, 1).await;

    assert_ordering_contract(&kind_sequence(&emitter, &channel));
    let statuses = turn_end_statuses(&emitter, &channel);
    assert_eq!(statuses, vec!["completed".to_owned()]);
}

#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_full_stack_two_turns_resume_through_dispatcher() {
    // The full backend slice for Antigravity, exercising the capture→resume
    // path through the dispatcher: turn 1 captures the server-assigned UUID and
    // persists it to the registry record; turn 2 must resume via the
    // registry-stored locator (`--conversation <uuid>`).
    let tmp = TempDir::new().expect("tempdir");
    let (_store_root, project) = temp_project(tmp.path(), "antigravity-e2e");
    let agent = project
        .register_agent("assistant", HarnessKind::Antigravity, None, None)
        .expect("agent");
    assert!(
        agent.session_locator.is_none(),
        "Antigravity agents carry session_locator: None (server-assigned, captured at runtime)"
    );

    let dispatcher = Arc::new(Dispatcher::new());
    let adapter: Arc<dyn HarnessAdapter> = Arc::new(AntigravityAdapter::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let channel = format!("agent:{}", agent.id);
    let factory = LiveFactory::new(
        adapter,
        project.directory.clone(),
        agent.clone(),
        Arc::clone(&emitter),
    );

    expect_accepted(
        dispatcher
            .send_message(
                agent.id,
                "Reply with exactly the word: ack",
                vec![],
                Uuid::now_v7(),
                Arc::clone(&factory) as Arc<dyn DispatchContextFactory>,
                OnBusy::Enqueue,
            )
            .await,
        "first send",
    );
    wait_for_idles(&emitter, 1).await;

    // Event-ordering contract on turn 1.
    let kinds = kind_sequence(&emitter, &channel);
    assert_eq!(
        kinds.first().map(String::as_str),
        Some("turn_start"),
        "first event must be turn_start; got: {kinds:?}"
    );
    assert_eq!(
        kinds.last().map(String::as_str),
        Some("agent_idle"),
        "last event must be agent_idle; got: {kinds:?}"
    );

    // Turn 2: the resume regression catch.
    expect_accepted(
        dispatcher
            .send_message(
                agent.id,
                "And again, exactly: ack",
                vec![],
                Uuid::now_v7(),
                factory,
                OnBusy::Enqueue,
            )
            .await,
        "second send",
    );
    wait_for_idles(&emitter, 2).await;

    let statuses = turn_end_statuses(&emitter, &channel);
    assert_eq!(
        statuses,
        vec!["completed".to_owned(), "completed".to_owned()],
        "both turns must complete (turn 2 resumes via the captured UUID); got: {statuses:?}"
    );
}

// ---------------------------------------------------------------------------
// Live cancellation tests. Each dispatches a turn through the real CLI,
// fires the cancellation token while it is in flight, and asserts the
// dispatcher synthesizes a `Cancelled` terminal and the agent returns to idle
// (reaches agent_idle) and is re-promptable.
// ---------------------------------------------------------------------------

/// The `outcome.source` of the single `turn_end` on the channel, if cancelled.
fn cancelled_source(emitter: &Arc<RecordingEmitter>, channel: &str) -> Option<String> {
    emitter
        .snapshot()
        .into_iter()
        .filter(|(name, payload)| name == channel && payload["type"] == "turn_end")
        .find_map(|(_, payload)| {
            if payload["outcome"]["status"] == "cancelled" {
                payload["outcome"]["source"].as_str().map(str::to_owned)
            } else {
                None
            }
        })
}

async fn live_cancel_case(harness: HarnessKind, adapter: Arc<dyn HarnessAdapter>) {
    let tmp = TempDir::new().expect("tempdir");
    let (_store_root, project) = temp_project(tmp.path(), "cancel-test");
    let agent = project
        .register_agent("assistant", harness, None, None)
        .expect("register_agent");

    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let channel = format!("agent:{}", agent.id);
    let factory = LiveFactory::new(
        adapter,
        project.directory.clone(),
        agent.clone(),
        Arc::clone(&emitter),
    );

    expect_accepted(
        dispatcher
            .send_message(
                agent.id,
                "Count slowly to one hundred, one number per line.",
                vec![],
                Uuid::now_v7(),
                factory,
                OnBusy::Enqueue,
            )
            .await,
        "send",
    );

    // Wait until the turn is actually live, then fire the token. The adapter
    // must kill the subprocess group and end the stream, and the dispatcher
    // must synthesize `Cancelled { User }`.
    tokio::time::timeout(LIVE_WAIT, emitter.wait_for_type("turn_start", 1))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "turn never started within the timeout; events: {:?}",
                emitter.snapshot()
            )
        });
    dispatcher.cancel(agent.id, CancelSource::User);

    // The agent drains to idle within the timeout; a CLI that ignored the
    // cancel path surfaces as a clear failure with the events.
    tokio::time::timeout(
        Duration::from_secs(30),
        emitter.wait_for_type("agent_idle", 1),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "turn did not drain within 30s of cancel — cancellation may have hung; events: {:?}",
            emitter.snapshot()
        )
    });

    assert_eq!(
        cancelled_source(&emitter, &channel).as_deref(),
        Some("user"),
        "expected a cancelled terminal stamped `user`; events: {:?}",
        emitter.snapshot()
    );
}

#[tokio::test]
#[ignore = "requires claude installed and authenticated — run with: make test-live"]
async fn live_claude_cancel_terminates_and_synthesizes_cancelled() {
    live_cancel_case(HarnessKind::ClaudeCode, Arc::new(ClaudeCodeAdapter::new())).await;
}

#[tokio::test]
#[ignore = "requires codex installed and authenticated — run with: make test-live"]
async fn live_codex_cancel_terminates_and_synthesizes_cancelled() {
    // Codex is the load-bearing case: it exits 0 on SIGTERM and emits no
    // terminal event, so only the dispatcher's token-driven synthesis produces
    // `Cancelled` here.
    live_cancel_case(HarnessKind::Codex, Arc::new(CodexAdapter::new())).await;
}

#[tokio::test]
#[ignore = "requires codex installed and authenticated — run with: make test-live"]
async fn live_codex_cancel_after_content_recovers_turn_identity() {
    // Pins the cancel-path identity recovery against the REAL Codex CLI: by
    // the time the first content chunk streams, the rollout file holds this
    // turn's `turn_context` — so a cancel must kill the group, read the file,
    // and surface `turn_context.turn_id` as a `turn_identity` event (which the
    // dispatcher journals as the cancelled turn's `TurnLink`). Cancelling only
    // after content deliberately avoids the pre-`turn_context` window, where
    // no identity legitimately exists (the plain cancel test covers that).
    let tmp = TempDir::new().expect("tempdir");
    let (_store_root, project) = temp_project(tmp.path(), "cancel-identity-test");
    let agent = project
        .register_agent("assistant", HarnessKind::Codex, None, None)
        .expect("register_agent");

    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let channel = format!("agent:{}", agent.id);
    let factory = LiveFactory::new(
        Arc::new(CodexAdapter::new()),
        project.directory.clone(),
        agent.clone(),
        Arc::clone(&emitter),
    );

    expect_accepted(
        dispatcher
            .send_message(
                agent.id,
                "Count slowly to one hundred, one number per line.",
                vec![],
                Uuid::now_v7(),
                factory,
                OnBusy::Enqueue,
            )
            .await,
        "send",
    );

    tokio::time::timeout(LIVE_WAIT, emitter.wait_for_type("content_chunk", 1))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "no content within the timeout; events: {:?}",
                emitter.snapshot()
            )
        });
    dispatcher.cancel(agent.id, CancelSource::User);
    tokio::time::timeout(
        Duration::from_secs(30),
        emitter.wait_for_type("agent_idle", 1),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "turn did not drain within 30s of cancel; events: {:?}",
            emitter.snapshot()
        )
    });

    assert_eq!(
        cancelled_source(&emitter, &channel).as_deref(),
        Some("user"),
        "expected a cancelled terminal stamped `user`; events: {:?}",
        emitter.snapshot()
    );
    let identity = emitter
        .snapshot()
        .iter()
        .find(|(name, payload)| name == &channel && payload["type"] == "turn_identity")
        .map(|(_, payload)| {
            payload["hydration_key"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        });
    assert!(
        identity.as_deref().is_some_and(|k| !k.is_empty()),
        "cancel after content must surface the turn's identity from the session file; events: {:?}",
        emitter.snapshot()
    );
}

#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_cancel_terminates_and_synthesizes_cancelled() {
    live_cancel_case(
        HarnessKind::Antigravity,
        Arc::new(AntigravityAdapter::new()),
    )
    .await;
}

/// Where the shared attachment body stages its file.
///
/// An enum rather than a bool because the two cases assert *opposite* things
/// about the same subject and a transposed argument would silently invert the
/// test's meaning.
#[derive(Debug, Clone, Copy)]
enum AttachmentStaging {
    /// The control: `<cwd>/attachments/`, inside the agent's working directory.
    /// Not a production location (Switchboard writes nothing into a working
    /// directory) — it is the baseline every harness must pass regardless of
    /// its sandbox, so a failure on the production case below is attributable
    /// to the location rather than to attachments as such.
    InsideCwdControl,
    /// Production: the project's `attachments/` in the user-global store.
    /// `temp_project` opens that store in its own tempdir, so this path is
    /// wholly outside the agent's cwd — exactly as it is for real users.
    ProjectStore,
}

/// What one attachment dispatch produced, for callers that assert more than
/// "the token came back" (the fence test's negative control).
struct AttachmentRun {
    token: &'static str,
    staged: std::path::PathBuf,
    text: String,
    /// Every wire payload emitted on the agent's channel, in arrival order.
    events: Vec<serde_json::Value>,
}

/// The production-shaped prompt: what a user's send looks like after the
/// attachment footer is appended.
const ATTACHMENT_PROMPT: &str =
    "Read the attached file and reply with just the string it contains, nothing else.";

/// Shared body for the per-harness attachment readability tests. Stages a file
/// (see [`AttachmentStaging`]) and sends it via the dispatcher's attachment path,
/// asserting the real CLI reads it. This proves the load-bearing assumption of
/// the whole feature: the staging location resolves under each harness's
/// sandbox. If a harness fails here, record the gap in
/// `docs/harness-behavior.md` and the README's harness-limitations.
async fn live_attachment_case(
    harness: HarnessKind,
    adapter: Arc<dyn HarnessAdapter>,
    staging: AttachmentStaging,
) {
    let tmp = TempDir::new().expect("tempdir");
    let run =
        run_attachment_case_in(tmp.path(), harness, adapter, staging, ATTACHMENT_PROMPT).await;
    assert!(
        run.text.contains(run.token),
        "{harness:?} ({staging:?}): response must contain the token from the staged \
         attachment (proves a file at {} is readable under this harness's sandbox — \
         that temp store is deleted after the turn, so the path identifies the case, \
         not a file to inspect); got: {:?}",
        run.staged.display(),
        run.text
    );
}

/// The dispatch half of [`live_attachment_case`], with a caller-supplied working
/// directory (so a caller can seed `<cwd>/.claude/settings.json` first) and no
/// assertion — it returns what the turn produced. Each call registers a fresh
/// project and agent, so two calls on one cwd are two independent sessions
/// under the same `~/.claude/projects/<encoded-cwd>/`, never a resume.
async fn run_attachment_case_in(
    cwd: &std::path::Path,
    harness: HarnessKind,
    adapter: Arc<dyn HarnessAdapter>,
    staging: AttachmentStaging,
    prompt: &str,
) -> AttachmentRun {
    let (_store_root, project) = temp_project(cwd, "attachment-test");
    let agent = project
        .register_agent("assistant", harness, None, None)
        .expect("register_agent");

    // Mirror what `stage_attachment` does: place the file in the staging dir and
    // reference it by absolute path.
    let token = "SWITCHBOARD_LIVE_ATTACHMENT_TOKEN_C4D77B";
    let dir = match staging {
        AttachmentStaging::InsideCwdControl => cwd.join("attachments"),
        AttachmentStaging::ProjectStore => project.attachments_dir(),
    };
    std::fs::create_dir_all(&dir).expect("create attachments dir");
    let staged = dir.join("note.txt");
    std::fs::write(&staged, token).expect("write staged attachment");
    let attachment = Attachment {
        label: "text-1".to_owned(),
        kind: AttachmentKind::Text,
        path: staged.to_string_lossy().into_owned(),
        original_name: "note.txt".to_owned(),
        dispatched_path: None,
    };

    let dispatcher = Arc::new(Dispatcher::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let channel = format!("agent:{}", agent.id);
    let factory = LiveFactory::new(
        adapter,
        project.directory.clone(),
        agent.clone(),
        Arc::clone(&emitter),
    );

    expect_accepted(
        dispatcher
            .send_message(
                agent.id,
                prompt,
                vec![attachment],
                Uuid::now_v7(),
                factory,
                OnBusy::Enqueue,
            )
            .await,
        "send",
    );
    wait_for_idles(&emitter, 1).await;

    let text = agent_text(&emitter, &channel);
    let events = emitter
        .snapshot()
        .into_iter()
        .filter(|(name, _)| *name == channel)
        .map(|(_, payload)| payload)
        .collect();
    // `_store_root` lives to here so the staged file outlives the turn; it is
    // deleted on return, so `staged` is a label for diagnostics, not a path
    // a caller can inspect.
    AttachmentRun {
        token,
        staged,
        text,
        events,
    }
}

#[tokio::test]
#[ignore = "requires claude installed and authenticated — run with: make test-live"]
async fn live_claude_attachment_inside_cwd_is_readable() {
    live_attachment_case(
        HarnessKind::ClaudeCode,
        Arc::new(ClaudeCodeAdapter::new()),
        AttachmentStaging::InsideCwdControl,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires codex installed and authenticated — run with: make test-live"]
async fn live_codex_attachment_inside_cwd_is_readable() {
    live_attachment_case(
        HarnessKind::Codex,
        Arc::new(CodexAdapter::new()),
        AttachmentStaging::InsideCwdControl,
    )
    .await;
}

// --- Project-store staging: attachments live outside every agent's cwd. ---
//
// Each of these asks one question: can this harness read an attachment at its
// real, production location — the user-global store, wholly outside the agent's
// working directory? This was the decision gate for moving the store out of
// working directories, and it is settled: all three pass. Read the Claude pass
// for what it is — the adapter grants the filesystem root via `--add-dir /`, so
// its green says "our flag holds," not "Claude has no fence" (it does, since
// 2.1.257; the fence test below is the one that speaks to it). Codex and
// Antigravity pass on their own sandbox posture. Record any change in
// `docs/harness-behavior.md` §3.7 before acting on it.

#[tokio::test]
#[ignore = "requires claude installed and authenticated — run with: make test-live"]
async fn live_claude_attachment_in_project_store_is_readable() {
    live_attachment_case(
        HarnessKind::ClaudeCode,
        Arc::new(ClaudeCodeAdapter::new()),
        AttachmentStaging::ProjectStore,
    )
    .await;
}

/// Claude 2.1.257 added `permissions.blockReadsOutsideWorkingDirectories`, a
/// user setting that refuses reads outside the working directories **even under
/// `--dangerously-skip-permissions`** (the CLI marks it `bypassImmune`). With it
/// on, the production case above degrades silently: `Read` is denied, the model
/// answers that it couldn't open the file, and the turn still ends `success`.
/// The adapter's root grant (`--add-dir /`) is what keeps attachments readable.
///
/// Two halves, both load-bearing. The **negative control** dispatches with the
/// grant removed and requires the fence to actually fire — the `Read` attempt's
/// own completion errored and naming the setting — so the positive half cannot
/// pass for the ordinary reason if a future Claude renames the key, moves it, or
/// stops reading it from where this test plants it. The **positive half** is the
/// production adapter reading an identically staged file (each half mints its
/// own store, so the two files are equivalent copies at different paths, never
/// one shared file). Both use one cwd on purpose: distinct projects, distinct
/// agents, distinct session ids, so the second is never a `--resume` of the
/// first (which could answer from context).
///
/// Both halves use a prompt that mandates the tool call. Left to its own
/// judgment the model sometimes declines an outside read *before* trying the
/// tool (observed once in three with the fence off), which would leave the
/// control with no denial to observe and fail it with a message that reads as
/// upstream drift. Mandating the attempt takes that discretion out of the loop
/// and makes the two halves a true A/B on the same tool call; the
/// `*_in_project_store_is_readable` tests keep the production-shaped prompt.
///
/// The setting is seeded at project scope (`<cwd>/.claude/settings.json`) so the
/// test never touches the developer's own settings; if Claude ever stopped
/// honoring project scope while still honoring user scope, the control would
/// read the token and fail loudly — the signal to move the seed, not a silent
/// pass.
#[tokio::test]
#[ignore = "requires claude installed and authenticated — run with: make test-live"]
async fn live_claude_attachment_in_project_store_is_readable_with_outside_reads_blocked() {
    const MANDATED_READ_PROMPT: &str = "Use the Read tool on the attached file. Do not decline \
        in advance. If the tool returns an error, reply with the error text verbatim; \
        otherwise reply with just the string the file contains, nothing else.";

    let tmp = TempDir::new().expect("tempdir");
    let settings_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&settings_dir).expect("create .claude");
    std::fs::write(
        settings_dir.join("settings.json"),
        r#"{"permissions":{"blockReadsOutsideWorkingDirectories":true}}"#,
    )
    .expect("write project settings");

    let control = run_attachment_case_in(
        tmp.path(),
        HarnessKind::ClaudeCode,
        Arc::new(ClaudeCodeAdapter::new().with_working_directory_grants(Vec::new())),
        AttachmentStaging::ProjectStore,
        MANDATED_READ_PROMPT,
    )
    .await;
    let kinds = kind_sequence_of(&control.events);
    let read_ids: Vec<&str> = control
        .events
        .iter()
        .filter(|p| p["type"] == "tool_started" && p["name"] == "Read")
        .filter_map(|p| p["tool_use_id"].as_str())
        .collect();
    assert!(
        !read_ids.is_empty(),
        "negative control: the model never attempted the Read (no `tool_started` for \
         Read) — a model/prompt drift, not a fence result; kinds: {kinds:?}; text: {:?}",
        control.text
    );
    let read_completions: Vec<&serde_json::Value> = control
        .events
        .iter()
        .filter(|p| {
            p["type"] == "tool_completed"
                && p["tool_use_id"]
                    .as_str()
                    .is_some_and(|id| read_ids.contains(&id))
        })
        .collect();
    let fence_fired = read_completions.iter().any(|p| {
        p["is_error"] == true
            && p["output"]
                .as_str()
                .is_some_and(|o| o.contains("blockReadsOutsideWorkingDirectories"))
    });
    assert!(
        fence_fired,
        "negative control: the Read was attempted but not denied by the fence (no \
         errored completion naming the setting) — the setting is not being honored \
         where this test plants it, so the positive half below would prove nothing; \
         Read completions: {read_completions:?}; text: {:?}",
        control.text
    );
    assert!(
        !control.text.is_empty() && !control.text.contains(control.token),
        "negative control: the turn must complete with a reply that lacks the token; \
         got: {:?}",
        control.text
    );

    let run = run_attachment_case_in(
        tmp.path(),
        HarnessKind::ClaudeCode,
        Arc::new(ClaudeCodeAdapter::new()),
        AttachmentStaging::ProjectStore,
        MANDATED_READ_PROMPT,
    )
    .await;
    assert!(
        run.text.contains(run.token),
        "with the root grant the identically staged attachment at {} must be readable \
         despite the fence; got: {:?}",
        run.staged.display(),
        run.text
    );
}

#[tokio::test]
#[ignore = "requires codex installed and authenticated — run with: make test-live"]
async fn live_codex_attachment_in_project_store_is_readable() {
    live_attachment_case(
        HarnessKind::Codex,
        Arc::new(CodexAdapter::new()),
        AttachmentStaging::ProjectStore,
    )
    .await;
}

/// Antigravity is the one harness with a real reason to fail this: `--add-dir <cwd>`
/// establishes its *workspace*, which scopes its file tools independently of
/// `--dangerously-skip-permissions`. If this fails, the fix to probe next is a
/// second `--add-dir` naming the staging directory.
#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_attachment_in_project_store_is_readable() {
    live_attachment_case(
        HarnessKind::Antigravity,
        Arc::new(AntigravityAdapter::new()),
        AttachmentStaging::ProjectStore,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_attachment_inside_cwd_is_readable() {
    live_attachment_case(
        HarnessKind::Antigravity,
        Arc::new(AntigravityAdapter::new()),
        AttachmentStaging::InsideCwdControl,
    )
    .await;
}
