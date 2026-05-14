//! Live end-to-end integration tests against real `claude`.
//!
//! Exercises the **full backend vertical slice** that an M1 user actually
//! triggers: `Directory::init` → `create_project` → `register_agent` →
//! `Dispatcher::send_message` → real `claude` subprocess → events streamed
//! back through the `EventEmitter`. Uses realistic on-disk paths
//! (`<tmp>/.switchboard/projects/<uuid>/`) so any path-encoding rule applied
//! by the harness adapter is exercised against the *actual* layout — not the
//! flatter `/tmp` paths the harness-crate live tests use.
//!
//! Why this layer matters: pure unit tests and adapter-only live tests can
//! pass while the integration path still has a bug. The M1.5 session-id
//! encoding bug (path containing `.switchboard` was not detected on the
//! second turn because the encoding rule was `/ → -` only, missing `. → -`)
//! is the load-bearing example. A test at this layer fails on the second
//! send, immediately, with the same `"Session ID is already in use"` claude
//! returns — months before any UI manual testing surfaces it.
//!
//! Run with: `make test-live`. Gated behind `#[ignore]` because each test
//! costs real claude credits (~$0.08–$0.16 per run) and requires
//! authenticated `claude` on PATH.

use std::sync::Arc;

use switchboard_core::{Directory, HarnessKind};
use switchboard_dispatcher::{Dispatcher, EventEmitter, RecordingEmitter};
use switchboard_harness::{ClaudeCodeAdapter, HarnessAdapter};
use tempfile::TempDir;

/// Extracts the `outcome.status` strings from every `turn_end` event the
/// emitter saw on the given channel, in arrival order. Tests assert against
/// this rather than rummaging through the event list each time.
fn turn_end_statuses(emitter: &Arc<RecordingEmitter>, channel: &str) -> Vec<String> {
    emitter
        .snapshot()
        .into_iter()
        .filter(|(name, payload)| name == channel && payload["type"] == "turn_end")
        .filter_map(|(_, payload)| payload["outcome"]["status"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
#[ignore = "requires claude installed and authenticated — run with: make test-live"]
async fn live_full_stack_two_consecutive_turns_succeed() {
    // Reproduces the full M1 vertical slice. Creates a working directory,
    // initializes the .switchboard/ layout, registers a project + agent
    // exactly like the app does, then dispatches two turns. The second turn
    // is the load-bearing assertion — the first turn creates the session
    // file, and the second turn must find it and switch from `--session-id`
    // to `--resume`. The M1.5 encoding bug would surface here as the second
    // turn failing with "Session ID … is already in use".
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
        agent.session_id.is_some(),
        "ClaudeCode agents must have a pre-generated session_id"
    );

    let dispatcher = Arc::new(Dispatcher::new());
    let adapter: Arc<dyn HarnessAdapter> = Arc::new(ClaudeCodeAdapter::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let channel = format!("agent:{}", agent.id);

    // Turn 1: adapter passes --session-id (session file doesn't exist yet).
    let handle1 = dispatcher
        .send_message(
            &agent,
            &project.root,
            "Reply with exactly the word: ack",
            adapter.as_ref(),
            Arc::clone(&emitter) as Arc<dyn EventEmitter>,
        )
        .await
        .expect("first send_message");
    // Awaiting the drain handle guarantees every event for this turn is
    // already in the emitter by the time the await resolves.
    handle1.join.await.expect("drain joined");

    // Turn 2: the regression catch. Adapter must detect the session file
    // created by turn 1 and pass --resume (not --session-id). Pre-fix, this
    // failed with "Session ID is already in use" because the path-encoding
    // mismatch made the session file appear missing.
    let handle2 = dispatcher
        .send_message(
            &agent,
            &project.root,
            "And again, exactly: ack",
            adapter.as_ref(),
            Arc::clone(&emitter) as Arc<dyn EventEmitter>,
        )
        .await
        .expect("second send_message");
    handle2.join.await.expect("second drain joined");

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
async fn live_full_stack_emits_turn_start_then_content_then_turn_end() {
    // Confirms the event-order contract end-to-end through the dispatcher.
    // The reducer relies on `turn_start` arriving before any `content_chunk`
    // and exactly one `turn_end` per turn; if the dispatcher's ordering
    // (acquire guard → generate TurnId → dispatch → emit TurnStart → spawn
    // drain) ever regressed, the frontend reducer would silently start
    // dropping events. This test fails fast in that case without needing a
    // running app.
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

    let handle = dispatcher
        .send_message(
            &agent,
            &project.root,
            "Reply with exactly: hi",
            adapter.as_ref(),
            Arc::clone(&emitter) as Arc<dyn EventEmitter>,
        )
        .await
        .expect("send_message");
    handle.join.await.expect("drain joined");

    let kinds: Vec<String> = emitter
        .snapshot()
        .into_iter()
        .filter(|(name, _)| name == &channel)
        .map(|(_, payload)| payload["type"].as_str().unwrap_or("").to_owned())
        .collect();

    assert_eq!(
        kinds.first().map(String::as_str),
        Some("turn_start"),
        "first event on the channel must be turn_start; got: {kinds:?}"
    );
    assert_eq!(
        kinds.last().map(String::as_str),
        Some("turn_end"),
        "last event must be turn_end; got: {kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| *k == "turn_end").count(),
        1,
        "must be exactly one terminal event per turn; got: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|k| k == "content_chunk"),
        "at least one content_chunk expected for a real-claude completion; got: {kinds:?}"
    );
}

#[tokio::test]
#[ignore = "requires claude installed and authenticated — run with: make test-live"]
async fn live_full_stack_paths_with_dot_components_resolve_correctly() {
    // Direct regression test for the path-encoding bug. Asserts that the
    // session file claude actually wrote (in ~/.claude/projects/...) is
    // detectable by the adapter when the project_root contains a
    // dot-prefixed directory (`.switchboard/`). If the encoding rule
    // diverges from claude's behaviour, this test fails on the second send
    // attempt with "Session ID is already in use".
    let tmp = TempDir::new().expect("tempdir");
    let directory = Directory::at(tmp.path()).expect("Directory::at");
    directory.init().expect("init");
    let project = directory.create_project("dot-path-test").expect("project");

    // Sanity: project.root must contain `.switchboard` — if Switchboard ever
    // changes its on-disk layout, this assertion catches the test going stale.
    assert!(
        project
            .root
            .components()
            .any(|c| c.as_os_str().to_string_lossy() == ".switchboard"),
        "expected project.root to contain `.switchboard`, got: {:?}",
        project.root
    );

    let agent = project
        .register_agent("assistant", HarnessKind::ClaudeCode)
        .expect("agent");
    let dispatcher = Arc::new(Dispatcher::new());
    let adapter: Arc<dyn HarnessAdapter> = Arc::new(ClaudeCodeAdapter::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let channel = format!("agent:{}", agent.id);

    // Send two consecutive turns. The session-id encoding bug surfaces as a
    // failure on the second.
    for (i, prompt) in ["Say 1", "Say 2"].iter().enumerate() {
        let handle = dispatcher
            .send_message(
                &agent,
                &project.root,
                prompt,
                adapter.as_ref(),
                Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            )
            .await
            .unwrap_or_else(|e| panic!("send_message #{} failed: {e:?}", i + 1));
        handle
            .join
            .await
            .unwrap_or_else(|e| panic!("drain join #{} failed: {e:?}", i + 1));
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
