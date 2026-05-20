//! Integration tests for `AntigravityAdapter`: drive the dual-source producer
//! end-to-end with the `fake_agy` fixture binary, which creates the
//! conversation directory under `$HOME` and writes `transcript.jsonl` while the
//! child runs — exercising the orchestration (UUID capture → tail → terminator
//! → post-exit drain → outcome) that was previously only reachable via
//! `make test-live`. These run as part of `make test`; live tests against the
//! real `agy` CLI live in `crates/harness/tests/live.rs`.

use std::path::Path;

use futures::StreamExt;
use serde_json::{Value, json};
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_harness::antigravity::paths::transcript_path;
use switchboard_harness::antigravity::sidecar::{
    SessionLinkRecord, append_record, read_latest, sidecar_path,
};
use switchboard_harness::antigravity::{FAKE_AGY_INVOCATIONS_FILE, FAKE_AGY_SCRIPT_FILE};
use switchboard_harness::{
    AdapterEvent, AntigravityAdapter, DispatchOptions, FailureKind, HarnessAdapter, Turn, TurnItem,
    TurnOutcome, TurnStatus, load_antigravity_transcript,
};
use uuid::Uuid;

const FAKE_AGY: &str = env!("CARGO_BIN_EXE_fake_agy");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/antigravity");

fn agy_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "test-agy".to_owned(),
        harness: HarnessKind::Antigravity,
        session_id: None,
        created_at: chrono::Utc::now(),
    }
}

fn user_record(prompt: &str, ts: &str) -> String {
    json!({
        "step_index": 0, "source": "USER_EXPLICIT", "type": "USER_INPUT",
        "status": "DONE", "created_at": ts,
        "content": format!("<USER_REQUEST>\n{prompt}\n</USER_REQUEST>"),
    })
    .to_string()
}

fn tool_call_record(ts: &str) -> String {
    json!({
        "step_index": 2, "source": "MODEL", "type": "PLANNER_RESPONSE",
        "status": "DONE", "created_at": ts,
        "tool_calls": [{"name": "run_command", "args": {"CommandLine": "\"ls\""}}],
    })
    .to_string()
}

fn tool_result_record(ts: &str, content: &str) -> String {
    json!({
        "step_index": 3, "source": "MODEL", "type": "RUN_COMMAND",
        "status": "DONE", "created_at": ts, "content": content,
    })
    .to_string()
}

fn terminal_record(ts: &str, content: &str) -> String {
    json!({
        "step_index": 4, "source": "MODEL", "type": "PLANNER_RESPONSE",
        "status": "DONE", "created_at": ts, "content": content,
    })
    .to_string()
}

fn drip(line: &str, delay_ms: u64) -> Value {
    json!({"json": line, "text": line, "delay_ms": delay_ms})
}

fn write_script(cwd: &Path, script: &Value) {
    std::fs::write(cwd.join(FAKE_AGY_SCRIPT_FILE), script.to_string()).unwrap();
}

async fn dispatch(
    adapter: &AntigravityAdapter,
    agent: &AgentRecord,
    cwd: &Path,
    prompt: &str,
) -> Vec<AdapterEvent> {
    let stream = adapter
        .dispatch(
            agent,
            cwd,
            prompt,
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed");
    stream.collect().await
}

fn outcome(events: &[AdapterEvent]) -> &TurnOutcome {
    events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::TurnEnd { outcome, .. } => Some(outcome),
            _ => None,
        })
        .expect("a TurnEnd event")
}

fn count<P: Fn(&AdapterEvent) -> bool>(events: &[AdapterEvent], pred: P) -> usize {
    events.iter().filter(|e| pred(e)).count()
}

/// A first turn that captures the server-assigned UUID, tails the transcript
/// for tool lifecycle, streams the answer over stdout, and persists the
/// sidecar. Timed record appends force some records to be tailed mid-loop and
/// the rest in the post-exit drain — so the single-emission assertions also
/// prove the cursor advances and never re-emits across drains.
#[tokio::test]
async fn first_turn_captures_uuid_tails_tools_and_completes() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::with_binary_and_home(FAKE_AGY, home.path());
    let agent = agy_agent();
    let uuid = Uuid::new_v4();

    write_script(
        cwd.path(),
        &json!({
            "conversation_uuid": uuid.to_string(),
            "records": [
                {"json": user_record("remember mango", "2026-05-19T19:00:00Z"), "delay_ms": 40},
                {"json": tool_call_record("2026-05-19T19:00:01Z"), "delay_ms": 40},
                {"json": tool_result_record("2026-05-19T19:00:02Z", "Output:\nMARKER.txt\n"), "delay_ms": 40},
                {"json": terminal_record("2026-05-19T19:00:03Z", "mango"), "delay_ms": 40},
            ],
            "stdout": [drip("mango", 40)],
            "exit_code": 0,
        }),
    );

    let events = dispatch(&adapter, &agent, cwd.path(), "remember mango").await;

    assert!(matches!(outcome(&events), TurnOutcome::Completed));
    assert_eq!(
        count(&events, |e| matches!(e, AdapterEvent::ToolStarted { .. })),
        1,
        "exactly one ToolStarted (no re-emission across drains)"
    );
    assert_eq!(
        count(&events, |e| matches!(e, AdapterEvent::ToolCompleted { .. })),
        1,
        "exactly one ToolCompleted"
    );
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(text.contains("mango"), "stdout answer streamed as content");

    // Sidecar healed/persisted with the captured UUID.
    let latest = read_latest(&sidecar_path(cwd.path(), agent.project_id, agent.id))
        .unwrap()
        .expect("sidecar persisted");
    assert_eq!(latest.conversation_id, uuid);
}

/// `fake_agy` writes everything and exits with zero delays — before any 100ms
/// poll tick can run. The post-exit final capture must still bind and persist
/// the UUID, and the turn must complete.
#[tokio::test]
async fn fast_first_turn_still_captures_uuid() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::with_binary_and_home(FAKE_AGY, home.path());
    let agent = agy_agent();
    let uuid = Uuid::new_v4();

    write_script(
        cwd.path(),
        &json!({
            "conversation_uuid": uuid.to_string(),
            "records": [
                {"json": user_record("quick", "2026-05-19T19:00:00Z"), "delay_ms": 0},
                {"json": terminal_record("2026-05-19T19:00:01Z", "done"), "delay_ms": 0},
            ],
            "stdout": [drip("done", 0)],
            "exit_code": 0,
        }),
    );

    let events = dispatch(&adapter, &agent, cwd.path(), "quick").await;
    assert!(matches!(outcome(&events), TurnOutcome::Completed));
    let latest = read_latest(&sidecar_path(cwd.path(), agent.project_id, agent.id))
        .unwrap()
        .expect("sidecar persisted even on the fast path");
    assert_eq!(latest.conversation_id, uuid);
}

/// A reply streamed but no matching `brain/<uuid>/` directory exists (a broken
/// transcript path). The agent can't be made resumable, so the turn must fail
/// loud rather than silently complete on the stdout answer.
#[tokio::test]
async fn unresumable_when_no_conversation_dir_appears() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::with_binary_and_home(FAKE_AGY, home.path());
    let agent = agy_agent();

    write_script(
        cwd.path(),
        &json!({
            "conversation_uuid": Uuid::new_v4().to_string(),
            "create_brain_dir": false,
            "stdout": [drip("here is an answer with no conversation dir", 0)],
            "exit_code": 0,
        }),
    );

    let events = dispatch(&adapter, &agent, cwd.path(), "orphan").await;
    match outcome(&events) {
        TurnOutcome::Failed {
            kind: FailureKind::AdapterFailure,
            message,
        } => assert!(message.contains("cannot be resumed"), "got: {message}"),
        other => panic!("expected unresumable AdapterFailure, got {other:?}"),
    }
}

/// The load-bearing case. A resume whose `--conversation <stale>` expired
/// server-side: `agy` warns and forks a fresh conversation. The adapter must
/// (a) heal the sidecar to the new UUID, (b) surface the forked turn's tool
/// events (re-pointed transcript), and (c) make the *next* dispatch resume the
/// healed UUID.
#[tokio::test]
async fn fork_and_heal_recaptures_new_conversation_and_next_dispatch_resumes_it() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::with_binary_and_home(FAKE_AGY, home.path());
    let agent = agy_agent();
    let stale_uuid = Uuid::new_v4();
    let new_uuid = Uuid::new_v4();
    let sidecar = sidecar_path(cwd.path(), agent.project_id, agent.id);

    // Pre-seed the sidecar so dispatch 1 is a resume against the stale UUID.
    append_record(
        &sidecar,
        &SessionLinkRecord {
            conversation_id: stale_uuid,
            captured_at: chrono::Utc::now(),
        },
    )
    .unwrap();

    // Dispatch 1: agy can't find the stale conversation, forks `new_uuid`.
    write_script(
        cwd.path(),
        &json!({
            "conversation_uuid": new_uuid.to_string(),
            "warning_not_found": stale_uuid.to_string(),
            "records": [
                {"json": user_record("first prompt", "2026-05-19T19:00:00Z"), "delay_ms": 0},
                {"json": tool_call_record("2026-05-19T19:00:01Z"), "delay_ms": 0},
                {"json": tool_result_record("2026-05-19T19:00:02Z", "ok"), "delay_ms": 0},
                {"json": terminal_record("2026-05-19T19:00:03Z", "answer one"), "delay_ms": 0},
            ],
            "stdout": [drip("answer one", 0)],
            "exit_code": 0,
        }),
    );
    let events1 = dispatch(&adapter, &agent, cwd.path(), "first prompt").await;

    // (c-prereq) Turn still completes — a real answer streamed.
    assert!(matches!(outcome(&events1), TurnOutcome::Completed));
    // (b) The forked conversation's tools surfaced (re-pointed transcript).
    assert_eq!(
        count(&events1, |e| matches!(e, AdapterEvent::ToolStarted { .. })),
        1,
        "forked turn's tool surfaced"
    );
    assert_eq!(
        count(&events1, |e| matches!(
            e,
            AdapterEvent::ToolCompleted { .. }
        )),
        1
    );
    // (a) Sidecar healed to the new UUID.
    let healed = read_latest(&sidecar).unwrap().expect("healed sidecar");
    assert_eq!(
        healed.conversation_id, new_uuid,
        "sidecar healed to forked UUID"
    );

    // Dispatch 2: a normal resume (no fork). It should pass --conversation <new_uuid>.
    write_script(
        cwd.path(),
        &json!({
            "conversation_uuid": new_uuid.to_string(),
            "records": [
                {"json": user_record("second prompt", "2026-05-19T19:05:00Z"), "delay_ms": 0},
                {"json": terminal_record("2026-05-19T19:05:01Z", "answer two"), "delay_ms": 0},
            ],
            "stdout": [drip("answer two", 0)],
            "exit_code": 0,
        }),
    );
    let events2 = dispatch(&adapter, &agent, cwd.path(), "second prompt").await;
    assert!(matches!(outcome(&events2), TurnOutcome::Completed));

    // (c) The post-heal dispatch resumed the healed UUID.
    let invocations = std::fs::read_to_string(cwd.path().join(FAKE_AGY_INVOCATIONS_FILE)).unwrap();
    let lines: Vec<&str> = invocations.lines().collect();
    assert_eq!(lines.len(), 2, "two dispatches recorded");
    assert!(
        lines[1].contains(&format!("--conversation {new_uuid}")),
        "second dispatch must resume the healed UUID; got: {}",
        lines[1]
    );
}

/// Hydration against a real-shape `transcript.jsonl` captured from `agy`
/// (multi-step tool use). Stages it where the loader resolves the path from the
/// sidecar's conversation UUID and asserts the reconstructed turns.
#[tokio::test]
async fn hydration_reconstructs_real_tool_use_transcript() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let agent = agy_agent();
    let uuid = Uuid::new_v4();

    let dest = transcript_path(home.path(), uuid);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::copy(format!("{FIXTURES}/tool-use.transcript.jsonl"), &dest).unwrap();

    let loaded = load_antigravity_transcript(home.path(), cwd.path(), uuid, agent.id).unwrap();

    assert_eq!(loaded.turns.len(), 2, "one user + one agent turn");
    assert!(loaded.warnings.is_empty());
    match &loaded.turns[0] {
        Turn::User { text, .. } => assert!(text.contains("Read the file MARKER.txt")),
        other => panic!("expected user turn, got {other:?}"),
    }
    match &loaded.turns[1] {
        Turn::Agent { status, items, .. } => {
            assert_eq!(*status, TurnStatus::Complete);
            let tools = items
                .iter()
                .filter(|i| matches!(i, TurnItem::Tool { .. }))
                .count();
            assert_eq!(tools, 2, "run_command + view_file, both completed");
            // Both tools paired to their result records (FIFO).
            for item in items {
                if let TurnItem::Tool {
                    output,
                    completed_at,
                    ..
                } = item
                {
                    assert!(output.is_some());
                    assert!(completed_at.is_some());
                }
            }
            // Terminal answer text emitted (hydration, unlike live, has no
            // stdout to replay).
            assert!(items.iter().any(|i| matches!(
                i,
                TurnItem::Text { text, .. } if text.contains("SWITCHBOARD_AGY_PROBE_42")
            )));
        }
        other => panic!("expected agent turn, got {other:?}"),
    }
    assert_eq!(loaded.meta.unwrap().model, "Gemini 3.5 Flash");
}
