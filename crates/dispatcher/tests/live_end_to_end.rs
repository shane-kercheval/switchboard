//! Live end-to-end integration tests against real `claude`, `codex`, and
//! `gemini`.
//!
//! Exercises the **full backend vertical slice** that a user actually
//! triggers: `Directory::init` → `create_project` → `register_agent` →
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
//!   `live_gemini_full_stack_emits_turn_start_then_content_then_turn_end`)
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

use switchboard_core::{AgentRecord, Directory, HarnessKind, SendId};
use switchboard_dispatcher::{
    ConversationJournal, DispatchContext, DispatchContextFactory, Dispatcher, EventEmitter,
    NoopJournal, NoopMetadataCache, NoopSessionLocatorSink, OnBusy, RecordingEmitter, SendOutcome,
};
use switchboard_harness::{
    AntigravityAdapter, CancelSource, ClaudeCodeAdapter, CodexAdapter, DispatchOptions,
    GeminiAdapter, HarnessAdapter,
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

/// The wire-`type` sequence on a channel, in arrival order.
fn kind_sequence(emitter: &Arc<RecordingEmitter>, channel: &str) -> Vec<String> {
    emitter
        .snapshot()
        .into_iter()
        .filter(|(name, _)| name == channel)
        .map(|(_, payload)| payload["type"].as_str().unwrap_or("").to_owned())
        .collect()
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

#[tokio::test]
#[ignore = "requires claude installed and authenticated — run with: make test-live"]
async fn live_claude_full_stack_two_consecutive_turns_succeed() {
    // Reproduces the full backend vertical slice. Two turns: the second is the
    // load-bearing assertion — the first creates the session file, and the
    // second must find it and switch from `--session-id` to `--resume`. A
    // path-encoding bug surfaces as the second turn failing with "Session ID …
    // is already in use". The two turns chain through the actor's FIFO backlog.
    let tmp = TempDir::new().expect("tempdir");
    let directory = Directory::at(tmp.path()).expect("Directory::at");
    directory.init().expect("init .switchboard/");
    let project = directory
        .create_project("integration-test")
        .expect("create_project");
    let agent = project
        .register_agent("assistant", HarnessKind::ClaudeCode)
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
    let directory = Directory::at(tmp.path()).expect("Directory::at");
    directory.init().expect("init");
    let project = directory.create_project("order-test").expect("project");
    let agent = project
        .register_agent("assistant", HarnessKind::ClaudeCode)
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
    let directory = Directory::at(&working_dir).expect("Directory::at");
    directory.init().expect("init");
    let project = directory.create_project("dot-path-test").expect("project");
    let agent = project
        .register_agent("assistant", HarnessKind::ClaudeCode)
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

    let directory = Directory::at(tmp.path()).expect("Directory::at");
    directory.init().expect("init");
    let project = directory.create_project("cwd-test").expect("project");
    let agent = project
        .register_agent("assistant", HarnessKind::ClaudeCode)
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
#[ignore = "requires gemini installed and authenticated — run with: make test-live"]
async fn live_gemini_full_stack_emits_turn_start_then_content_then_turn_end() {
    let tmp = TempDir::new().expect("tempdir");
    let directory = Directory::at(tmp.path()).expect("Directory::at");
    directory.init().expect("init");
    let project = directory
        .create_project("gemini-order-test")
        .expect("project");
    let agent = project
        .register_agent("assistant", HarnessKind::Gemini)
        .expect("agent");
    assert!(
        agent.session_locator.is_some(),
        "Gemini agents must have a pre-generated session_id (Claude-shape)"
    );

    let dispatcher = Arc::new(Dispatcher::new());
    let adapter: Arc<dyn HarnessAdapter> = Arc::new(GeminiAdapter::new());
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
#[ignore = "requires codex installed and authenticated — run with: make test-live"]
async fn live_codex_full_stack_emits_turn_start_then_content_then_turn_end() {
    // Codex agents register with `session_id = None` (the per-agent sidecar is
    // the system-of-record); the dispatcher must not depend on
    // `agent.session_locator.is_some()` to function.
    let tmp = TempDir::new().expect("tempdir");
    let directory = Directory::at(tmp.path()).expect("Directory::at");
    directory.init().expect("init");
    let project = directory
        .create_project("codex-order-test")
        .expect("project");
    let agent = project
        .register_agent("assistant", HarnessKind::Codex)
        .expect("agent");
    assert!(
        agent.session_locator.is_none(),
        "Codex agents must register with session_id = None"
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
    // path through the dispatcher: turn 1 captures the server-assigned UUID into
    // the per-agent sidecar; turn 2 must read that sidecar and resume via
    // `--conversation <uuid>`.
    let tmp = TempDir::new().expect("tempdir");
    let directory = Directory::at(tmp.path()).expect("Directory::at");
    directory.init().expect("init");
    let project = directory
        .create_project("antigravity-e2e")
        .expect("project");
    let agent = project
        .register_agent("assistant", HarnessKind::Antigravity)
        .expect("agent");
    assert!(
        agent.session_locator.is_none(),
        "Antigravity agents carry session_id: None (server-assigned, sidecar-carried)"
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
// Live cancellation tests (M4.3). Each dispatches a turn through the real CLI,
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
    let directory = Directory::at(tmp.path()).expect("Directory::at");
    directory.init().expect("init .switchboard/");
    let project = directory
        .create_project("cancel-test")
        .expect("create_project");
    let agent = project
        .register_agent("assistant", harness)
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
#[ignore = "requires gemini installed and authenticated — run with: make test-live"]
async fn live_gemini_cancel_terminates_and_synthesizes_cancelled() {
    live_cancel_case(HarnessKind::Gemini, Arc::new(GeminiAdapter::new())).await;
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
