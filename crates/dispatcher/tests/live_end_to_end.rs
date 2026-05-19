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
//!   detected by `live_full_stack_two_consecutive_turns_succeed`, which
//!   exercises session resume across two Claude turns.
//! - The cwd bug (claude was spawned in `.switchboard/projects/<uuid>/`
//!   instead of the user's bound working directory, so it couldn't see the
//!   user's repo files) — detected by `live_full_stack_claude_sees_files_in_cwd`,
//!   which writes a file into the working dir and asserts claude can read it.
//! - A future dispatcher branch that's secretly harness-specific — the
//!   per-harness event-ordering checks
//!   (`live_full_stack_emits_turn_start_then_content_then_turn_end` for
//!   Claude,
//!   `live_full_stack_codex_emits_turn_start_then_content_then_turn_end`,
//!   `live_full_stack_gemini_emits_turn_start_then_content_then_turn_end`)
//!   assert the same `turn_start → content_chunk → turn_end → agent_idle`
//!   contract holds through every harness's real subprocess. Any accidental
//!   coupling of the dispatcher to one harness's behavior surfaces in the
//!   other tests.
//!
//! Run with: `make test-live`. Gated behind `#[ignore]` because each test
//! costs real credits and requires the corresponding CLI installed and
//! authenticated.

use std::sync::Arc;

use switchboard_core::{Directory, HarnessKind};
use switchboard_dispatcher::{Dispatcher, EventEmitter, RecordingEmitter};
use switchboard_harness::{
    ClaudeCodeAdapter, CodexAdapter, DispatchOptions, GeminiAdapter, HarnessAdapter,
};
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

/// Concatenates every `content_chunk.text` for a channel in arrival order.
fn agent_text(emitter: &Arc<RecordingEmitter>, channel: &str) -> String {
    emitter
        .snapshot()
        .into_iter()
        .filter(|(name, payload)| name == channel && payload["type"] == "content_chunk")
        .filter_map(|(_, payload)| payload["text"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
#[ignore = "requires claude installed and authenticated — run with: make test-live"]
async fn live_full_stack_two_consecutive_turns_succeed() {
    // Reproduces the full backend vertical slice. Creates a working
    // directory, initializes the .switchboard/ layout, registers a project +
    // agent exactly like the app does, then dispatches two turns. The
    // second turn is the load-bearing assertion — the first turn creates
    // the session file, and the second turn must find it and switch from
    // `--session-id` to `--resume`. A path-encoding bug in the session-file
    // lookup would surface here as the second turn failing with "Session
    // ID … is already in use".
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
            &project.directory,
            "Reply with exactly the word: ack",
            adapter.as_ref(),
            Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            DispatchOptions::default(),
        )
        .await
        .expect("first send_message");
    handle1.join.await.expect("drain joined");

    // Turn 2: the regression catch. Adapter must detect the session file
    // created by turn 1 and pass --resume (not --session-id). Pre-fix, this
    // failed with "Session ID is already in use" because the path-encoding
    // mismatch made the session file appear missing.
    let handle2 = dispatcher
        .send_message(
            &agent,
            &project.directory,
            "And again, exactly: ack",
            adapter.as_ref(),
            Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            DispatchOptions::default(),
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
            &project.directory,
            "Reply with exactly: hi",
            adapter.as_ref(),
            Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            DispatchOptions::default(),
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
    // AGENTS.md stream contract: `AgentIdle` is the last event on the
    // per-agent channel for a dispatch, AFTER `TurnEnd` and any
    // post-terminal agent-scoped events. This was originally a
    // `turn_end`-is-last assertion (pre-AgentIdle); updated to track the
    // current contract.
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
    // turn_end must precede agent_idle.
    let turn_end_idx = kinds.iter().position(|k| k == "turn_end").unwrap();
    let agent_idle_idx = kinds.iter().position(|k| k == "agent_idle").unwrap();
    assert!(
        turn_end_idx < agent_idle_idx,
        "turn_end must precede agent_idle; got: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|k| k == "content_chunk"),
        "at least one content_chunk expected for a real-claude completion; got: {kinds:?}"
    );
}

#[tokio::test]
#[ignore = "requires claude installed and authenticated — run with: make test-live"]
async fn live_full_stack_paths_with_dot_components_resolve_correctly() {
    // Direct regression test for the path-encoding bug. The user's bound
    // working directory can contain dots (hidden directories, dotted
    // usernames, etc.). The path-encoding rule must apply the same way at
    // any path position, otherwise the second-turn `--resume` lookup fails.
    let tmp = TempDir::new().expect("tempdir");
    // Build a working directory with a dot-prefixed component to exercise
    // the encoding rule. The actual user-bound dir is typed by the user
    // and can take any shape; we simulate one of the trickier cases here.
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

    // Send two consecutive turns. The session-id encoding bug surfaces as a
    // failure on the second.
    for (i, prompt) in ["Say 1", "Say 2"].iter().enumerate() {
        let handle = dispatcher
            .send_message(
                &agent,
                &project.directory,
                prompt,
                adapter.as_ref(),
                Arc::clone(&emitter) as Arc<dyn EventEmitter>,
                DispatchOptions::default(),
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

#[tokio::test]
#[ignore = "requires claude installed and authenticated — run with: make test-live"]
async fn live_full_stack_claude_sees_files_in_cwd() {
    // Regression test for the cwd-routing rule: claude must be spawned in
    // the user's bound working directory, NOT in
    // `<dir>/.switchboard/projects/<uuid>/` (the Switchboard-internal
    // metadata directory). The observable symptom of a regression here is
    // that claude can't see the user's repo files via its Read/Glob/Bash
    // tools — the core use case (orchestrate claude on the user's code)
    // would be broken.
    //
    // This test: write a known-content file into the working directory,
    // dispatch a turn asking claude to read it, assert the streamed
    // response references the content. Pre-fix, claude's Read tool would
    // fail or return nothing (the file isn't in `.switchboard/projects/<uuid>/`).
    let tmp = TempDir::new().expect("tempdir");
    // A token unlikely to appear in any other context, so its presence in
    // the response is strong evidence claude read the file.
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

    let handle = dispatcher
        .send_message(
            &agent,
            &project.directory,
            "Read the file MARKER.txt in the current directory and tell me what string it contains. Reply with just the string, nothing else.",
            adapter.as_ref(),
            Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            DispatchOptions::default(),
        )
        .await
        .expect("send_message");
    handle.join.await.expect("drain joined");

    let text = agent_text(&emitter, &channel);
    assert!(
        text.contains(token),
        "claude's response must contain the marker token (proves it read the file from the cwd); \
         got text: {text:?}"
    );
}

#[tokio::test]
#[ignore = "requires gemini installed and authenticated — run with: make test-live"]
async fn live_full_stack_gemini_emits_turn_start_then_content_then_turn_end() {
    // The canonical empirical assertion of M3's headline claim: the
    // dispatcher abstraction is genuinely harness-neutral. Same
    // event-ordering contract (turn_start → content_chunk → turn_end →
    // agent_idle) must hold through the dispatcher for Gemini exactly
    // as it does for Claude — proved through the real `gemini`
    // subprocess code path, not just through adapter-layer fixtures.
    //
    // `Project::register_agent` mints UUID v4 for Gemini because session
    // filenames use the first 8 hex chars of the session UUID and v7s
    // minted in the same millisecond share that prefix. No special
    // test-side handling needed; the production path honors this.
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
        agent.session_id.is_some(),
        "Gemini agents must have a pre-generated session_id (Claude-shape)"
    );

    let dispatcher = Arc::new(Dispatcher::new());
    let adapter: Arc<dyn HarnessAdapter> = Arc::new(GeminiAdapter::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let channel = format!("agent:{}", agent.id);

    let handle = dispatcher
        .send_message(
            &agent,
            &project.directory,
            "Reply with exactly: hi",
            adapter.as_ref(),
            Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            DispatchOptions::default(),
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
        "at least one content_chunk expected for a real-gemini completion; got: {kinds:?}"
    );

    let statuses = turn_end_statuses(&emitter, &channel);
    assert_eq!(statuses, vec!["completed".to_owned()]);
}

#[tokio::test]
#[ignore = "requires codex installed and authenticated — run with: make test-live"]
async fn live_full_stack_codex_emits_turn_start_then_content_then_turn_end() {
    // Symmetric coverage with the Claude and Gemini dispatcher-layer
    // tests. The dispatcher's per-turn machinery (TurnId generation,
    // AgentIdleGuard, EventEmitter forwarding, per-agent state tracking)
    // must remain harness-agnostic; this test proves the same
    // `turn_start → content_chunk → turn_end → agent_idle` ordering
    // contract through a real `codex` subprocess. A regression coupling
    // any dispatcher path to Claude- or Gemini-specific behavior fails
    // here.
    //
    // Codex agents register with `session_id = None` (the per-agent
    // sidecar is the system-of-record); the dispatcher must not depend
    // on `agent.session_id.is_some()` to function.
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
        agent.session_id.is_none(),
        "Codex agents must register with session_id = None"
    );

    let dispatcher = Arc::new(Dispatcher::new());
    let adapter: Arc<dyn HarnessAdapter> = Arc::new(CodexAdapter::new());
    let emitter = Arc::new(RecordingEmitter::new());
    let channel = format!("agent:{}", agent.id);

    let handle = dispatcher
        .send_message(
            &agent,
            &project.directory,
            "Reply with exactly: hi",
            adapter.as_ref(),
            Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            DispatchOptions::default(),
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
        "at least one content_chunk expected for a real-codex completion; got: {kinds:?}"
    );

    let statuses = turn_end_statuses(&emitter, &channel);
    assert_eq!(statuses, vec!["completed".to_owned()]);
}
