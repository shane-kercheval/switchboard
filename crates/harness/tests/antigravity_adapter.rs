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
use switchboard_core::{AgentRecord, HarnessKind, SessionLocator};
use switchboard_harness::antigravity::paths;
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
        model: None,
        effort: None,
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "test-agy".to_owned(),
        harness: HarnessKind::Antigravity,
        session_locator: None,
        created_at: chrono::Utc::now(),
    }
}

/// An agent resuming an existing conversation — its locator is already on the
/// record (as the dispatcher's live-read factory would supply it).
fn agy_agent_resuming(conversation_id: Uuid) -> AgentRecord {
    AgentRecord {
        model: None,
        effort: None,
        session_locator: Some(SessionLocator::Uuid(conversation_id)),
        ..agy_agent()
    }
}

/// The conversation UUID carried by the dispatch's capture event, or `None` if
/// it emitted none (a plain resume). Asserts at most one capture per dispatch —
/// the adapter must emit it once on first capture / fork-and-heal, never twice.
fn captured_uuid(events: &[AdapterEvent]) -> Option<Uuid> {
    let mut captures = events.iter().filter_map(|e| match e {
        AdapterEvent::SessionLocatorCaptured {
            locator: SessionLocator::Uuid(u),
            ..
        } => Some(*u),
        _ => None,
    });
    let first = captures.next();
    assert!(
        captures.next().is_none(),
        "a dispatch must emit at most one SessionLocatorCaptured event"
    );
    first
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

/// Stage a user-scope MCP config (one server "tiddly") and one plugin skill
/// (`chrome-devtools-plugin/troubleshooting`) under `home`'s `~/.gemini/config`,
/// so the registry loaders have something to surface.
fn stage_registries(home: &Path) {
    std::fs::create_dir_all(paths::config_root(home)).unwrap();
    std::fs::write(
        paths::mcp_config_path(home),
        r#"{"mcpServers":{"tiddly":{"command":"tiddly-bin"}}}"#,
    )
    .unwrap();
    let skill = paths::plugins_root(home)
        .join("chrome-devtools-plugin")
        .join("skills")
        .join("troubleshooting");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "# troubleshooting").unwrap();
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
/// for tool lifecycle and the answer text, and emits the capture event. Timed
/// record appends force some records to be tailed mid-loop and the rest in the
/// post-exit drain — so the single-emission assertions also prove the cursor
/// advances and never re-emits across drains.
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
    assert!(
        text.contains("mango"),
        "answer text emitted from the transcript"
    );

    // Capture event emitted with the server-assigned UUID — the dispatcher
    // persists it to the registry (no sidecar written by the adapter).
    assert_eq!(captured_uuid(&events), Some(uuid));
}

/// `fake_agy` writes everything and exits with zero delays — before any 100ms
/// poll tick can run. The post-exit final capture must still bind and emit
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
    assert_eq!(
        captured_uuid(&events),
        Some(uuid),
        "post-exit capture still emits the UUID on the fast path"
    );
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

/// Resilience: if `agy`'s CLI log ever stops naming the conversation (the
/// Google-internal log line moves on a version bump), capture must degrade to
/// the brain-dir prompt-correlation fallback rather than fail every turn.
/// `suppress_conversation_log` omits the id lines while still creating the
/// brain dir, so only the fallback can bind the UUID.
#[tokio::test]
async fn capture_falls_back_to_brain_dir_when_log_omits_the_id() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::with_binary_and_home(FAKE_AGY, home.path());
    let agent = agy_agent();
    let uuid = Uuid::new_v4();

    write_script(
        cwd.path(),
        &json!({
            "conversation_uuid": uuid.to_string(),
            "suppress_conversation_log": true,
            "records": [
                {"json": user_record("fallback please", "2026-05-19T19:00:00Z"), "delay_ms": 0},
                {"json": terminal_record("2026-05-19T19:00:01Z", "ok"), "delay_ms": 0},
            ],
            "stdout": [drip("ok", 0)],
            "exit_code": 0,
        }),
    );

    let events = dispatch(&adapter, &agent, cwd.path(), "fallback please").await;
    assert!(
        matches!(outcome(&events), TurnOutcome::Completed),
        "fallback prompt-correlation should still bind the conversation"
    );
    assert_eq!(
        captured_uuid(&events),
        Some(uuid),
        "capture event emitted via the fallback path"
    );
}

/// The load-bearing case. A resume whose `--conversation <stale>` expired
/// server-side: `agy` warns and forks a fresh conversation. The adapter must
/// (a) emit a capture event healing the locator to the new UUID, (b) surface the
/// forked turn's tool events (re-pointed transcript), and (c) make the *next*
/// dispatch (whose record now holds the healed UUID) resume that UUID.
#[tokio::test]
async fn fork_and_heal_recaptures_new_conversation_and_next_dispatch_resumes_it() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::with_binary_and_home(FAKE_AGY, home.path());
    let stale_uuid = Uuid::new_v4();
    let new_uuid = Uuid::new_v4();

    // Dispatch 1 resumes the stale UUID (carried on the record); agy can't find
    // it and forks `new_uuid`.
    let agent1 = agy_agent_resuming(stale_uuid);
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
    let events1 = dispatch(&adapter, &agent1, cwd.path(), "first prompt").await;

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
    // (a) Capture event heals the locator to the forked UUID — the dispatcher
    // persists it, so the next turn's record carries `new_uuid`.
    assert_eq!(
        captured_uuid(&events1),
        Some(new_uuid),
        "fork-and-heal emits a capture event with the forked UUID"
    );

    // Dispatch 2: a normal resume against the healed UUID (as the dispatcher's
    // live-read factory would now supply it). It must pass --conversation
    // <new_uuid> and emit no further capture.
    let agent2 = agy_agent_resuming(new_uuid);
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
    let events2 = dispatch(&adapter, &agent2, cwd.path(), "second prompt").await;
    assert!(matches!(outcome(&events2), TurnOutcome::Completed));
    assert_eq!(
        captured_uuid(&events2),
        None,
        "a plain resume (locator unchanged) emits no capture event"
    );

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
/// conversation UUID and asserts the reconstructed turns.
#[tokio::test]
async fn hydration_reconstructs_real_tool_use_transcript() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let agent = agy_agent();
    let uuid = Uuid::new_v4();

    let dest = paths::transcript_path(home.path(), uuid);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::copy(format!("{FIXTURES}/tool-use.transcript.jsonl"), &dest).unwrap();

    let loaded =
        load_antigravity_transcript(home.path(), cwd.path(), Some(uuid), agent.id).unwrap();

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

/// Regression: on a resume turn `agy` replays the whole conversation's prior
/// answers to stdout. The turn's bubble must contain only the **new** answer
/// (sourced from the transcript's per-turn record), not the replayed history.
#[tokio::test]
async fn resume_turn_does_not_accumulate_prior_answers() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::with_binary_and_home(FAKE_AGY, home.path());
    let uuid = Uuid::new_v4();

    // Turn 1 (fresh): answer "ALPHA". Captures `uuid`.
    write_script(
        cwd.path(),
        &json!({
            "conversation_uuid": uuid.to_string(),
            "records": [
                {"json": user_record("first", "2026-05-19T19:00:00Z"), "delay_ms": 0},
                {"json": terminal_record("2026-05-19T19:00:01Z", "ALPHA"), "delay_ms": 0},
            ],
            "stdout": [drip("ALPHA", 0)],
            "exit_code": 0,
        }),
    );
    let events1 = dispatch(&adapter, &agy_agent(), cwd.path(), "first").await;
    assert_eq!(captured_uuid(&events1), Some(uuid));

    // Turn 2 (resume): the dispatcher would have persisted `uuid` onto the
    // record, so the resuming agent carries it. The new answer is "BETA", but
    // stdout replays "ALPHA" then "BETA" — the producer must emit only "BETA"
    // (from the transcript, past the resume cursor).
    write_script(
        cwd.path(),
        &json!({
            "conversation_uuid": uuid.to_string(),
            "records": [
                {"json": user_record("second", "2026-05-19T19:05:00Z"), "delay_ms": 0},
                {"json": terminal_record("2026-05-19T19:05:01Z", "BETA"), "delay_ms": 0},
            ],
            "stdout": [drip("ALPHA", 0), drip("BETA", 0)],
            "exit_code": 0,
        }),
    );
    let events = dispatch(&adapter, &agy_agent_resuming(uuid), cwd.path(), "second").await;

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("BETA"),
        "new answer must appear; got {text:?}"
    );
    assert!(
        !text.contains("ALPHA"),
        "prior turn's answer must NOT accumulate into this turn; got {text:?}"
    );
}

/// Output appeared but the transcript holds no terminal answer (a `tool_call`
/// + result but no closing `PLANNER_RESPONSE`, e.g. a transcript-path break or
/// truncation). Since stdout is no longer rendered, the turn must fail loud
/// rather than complete with a blank bubble.
#[tokio::test]
async fn output_without_transcript_terminal_answer_fails_loud() {
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
                {"json": user_record("do work", "2026-05-19T19:00:00Z"), "delay_ms": 0},
                {"json": tool_call_record("2026-05-19T19:00:01Z"), "delay_ms": 0},
                {"json": tool_result_record("2026-05-19T19:00:02Z", "ran"), "delay_ms": 0},
                // No terminal PLANNER_RESPONSE answer record.
            ],
            "stdout": [drip("some stdout output", 0)],
            "exit_code": 0,
        }),
    );

    let events = dispatch(&adapter, &agent, cwd.path(), "do work").await;
    // The tool still surfaced...
    assert_eq!(
        count(&events, |e| matches!(e, AdapterEvent::ToolCompleted { .. })),
        1
    );
    // ...but with no readable answer the turn fails loud, not blank-Completed.
    match outcome(&events) {
        TurnOutcome::Failed {
            kind: FailureKind::AdapterFailure,
            message,
        } => assert!(
            message.contains("could not read the answer"),
            "got: {message}"
        ),
        other => panic!("expected AdapterFailure, got {other:?}"),
    }
}

/// `agy` falls back to interactive OAuth when the keyring token is stale,
/// printing `Authentication required…` to stdout and blocking ~30s. The
/// producer fast-fails on that stdout line and emits `AuthFailure`. Driven by
/// `fake_agy` (stdout drip) rather than a stderr fixture — it exercises the
/// real stdout detection path non-destructively (no Keychain mutation).
#[tokio::test]
async fn auth_failure_line_on_stdout_emits_auth_failure() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::with_binary_and_home(FAKE_AGY, home.path());
    let agent = agy_agent();

    write_script(
        cwd.path(),
        &json!({
            "conversation_uuid": Uuid::new_v4().to_string(),
            "create_brain_dir": false,
            // `hang` (park after dripping, never self-exit) models real `agy`
            // blocking ~30s on interactive OAuth, so the adapter's stdout
            // auth-detection deterministically force-kills it. A clean self-exit
            // (`exit_code: 0`) instead manufactures a race: on a loaded CI box the
            // process-exit can beat detection, yielding AdapterFailure ("no
            // answer") rather than AuthFailure. The detection path under test is
            // unchanged — this just stops the fake from exiting out from under it.
            "stdout": [drip("Authentication required. Please visit the URL to log in:", 0)],
            "hang": true,
        }),
    );

    let events = dispatch(&adapter, &agent, cwd.path(), "hi").await;
    match outcome(&events) {
        TurnOutcome::Failed {
            kind: FailureKind::AuthFailure,
            ..
        } => {}
        other => panic!("expected AuthFailure, got {other:?}"),
    }
}

/// Wiring seam (dispatch): the loaders run at dispatch time and their output
/// must reach the emitted `SessionMeta`. Proves `ProducerCtx` → `SessionMeta`
/// carries the loaded vecs, which the structural-only live test cannot.
#[tokio::test]
async fn dispatch_session_meta_carries_loaded_registries() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::with_binary_and_home(FAKE_AGY, home.path());
    let agent = agy_agent();
    let uuid = Uuid::new_v4();

    stage_registries(home.path());
    write_script(
        cwd.path(),
        &json!({
            "conversation_uuid": uuid.to_string(),
            "records": [
                {"json": user_record("hi", "2026-05-19T19:00:00Z"), "delay_ms": 0},
                {"json": terminal_record("2026-05-19T19:00:01Z", "ack"), "delay_ms": 0},
            ],
            "stdout": [drip("ack", 0)],
            "exit_code": 0,
        }),
    );

    let events = dispatch(&adapter, &agent, cwd.path(), "hi").await;
    let meta = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::SessionMeta { .. }))
        .expect("SessionMeta emitted post-terminal");
    match meta {
        AdapterEvent::SessionMeta {
            mcp_servers,
            skills,
            ..
        } => {
            assert!(
                mcp_servers.iter().any(|s| s.name == "tiddly"),
                "configured MCP server reached SessionMeta; got {mcp_servers:?}"
            );
            assert!(
                skills.contains(&"chrome-devtools-plugin/troubleshooting".to_owned()),
                "qualified skill reached SessionMeta; got {skills:?}"
            );
        }
        other => panic!("expected SessionMeta, got {other:?}"),
    }
}

/// Wiring seam (hydration): the same loaders feed `merge_meta_with_loaders`, so
/// hydrated meta must carry non-empty registries alongside the parsed turns.
#[tokio::test]
async fn hydration_meta_carries_loaded_registries() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let agent = agy_agent();
    let uuid = Uuid::new_v4();

    stage_registries(home.path());
    let dest = paths::transcript_path(home.path(), uuid);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(
        &dest,
        format!(
            "{}\n{}\n",
            user_record("hi", "2026-05-19T19:00:00Z"),
            terminal_record("2026-05-19T19:00:01Z", "ack"),
        ),
    )
    .unwrap();

    let loaded =
        load_antigravity_transcript(home.path(), cwd.path(), Some(uuid), agent.id).unwrap();
    let meta = loaded.meta.expect("meta present");
    assert!(meta.mcp_servers.iter().any(|s| s.name == "tiddly"));
    assert!(
        meta.skills
            .contains(&"chrome-devtools-plugin/troubleshooting".to_owned())
    );
    // Registries layer onto the parsed turns, not replace them.
    assert_eq!(loaded.turns.len(), 2);
}

/// Wiring seam (never-dispatched): `None` conversation id still surfaces the
/// registries (the no-sidecar path that `commands.rs` routes through).
#[tokio::test]
async fn hydration_none_conversation_still_surfaces_registries() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let agent = agy_agent();

    stage_registries(home.path());
    let loaded = load_antigravity_transcript(home.path(), cwd.path(), None, agent.id).unwrap();
    assert!(loaded.turns.is_empty(), "never dispatched → no turns");
    let meta = loaded
        .meta
        .expect("registry meta even with no conversation");
    assert!(meta.mcp_servers.iter().any(|s| s.name == "tiddly"));
    assert!(
        meta.skills
            .contains(&"chrome-devtools-plugin/troubleshooting".to_owned())
    );
}

#[tokio::test]
async fn cancel_terminates_and_emits_no_terminal() {
    // Antigravity already uses `select!` to tail the transcript on a tick;
    // cancellation is one more arm. The script `hang`s after writing its
    // records, so the turn is still in flight when we fire the token. On
    // cancel the adapter kills the process and ends the stream with NO
    // terminal event (the dispatcher synthesizes `Cancelled`). The
    // timeout-wrapped collect proves the kill actually tore the process down
    // (a failed kill would leave the producer's drain awaiting forever).
    use tokio_util::sync::CancellationToken;

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
                {"json": user_record("long running", "2026-05-19T19:00:00Z"), "delay_ms": 0},
            ],
            "stdout": [],
            "hang": true,
        }),
    );

    let token = CancellationToken::new();
    let options = DispatchOptions {
        cancel_token: token.clone(),
        ..Default::default()
    };
    let stream = adapter
        .dispatch(&agent, cwd.path(), "long running", Uuid::now_v7(), options)
        .await
        .expect("dispatch should succeed");

    // Let the adapter spawn `agy`, capture the UUID, and settle into its poll
    // loop, then cancel mid-turn. `hang: true` keeps the turn in flight.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    token.cancel();

    let events: Vec<AdapterEvent> =
        tokio::time::timeout(std::time::Duration::from_secs(15), stream.collect())
            .await
            .expect("stream must end promptly after cancel, not hang");

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AdapterEvent::TurnEnd { .. })),
        "adapter must emit no terminal event on cancel; got: {events:?}"
    );
}

/// End-to-end quota path: with the per-dispatch `--log-file` carrying a
/// `RESOURCE_EXHAUSTED` line, the no-answer branch surfaces an authored
/// `HarnessError` quota message instead of the generic "agy exited without
/// producing an answer" fallback. Closes G1 (`docs/research/harness-behavior.md`).
#[tokio::test]
async fn quota_log_line_surfaces_as_harness_error_quota_message() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::with_binary_and_home(FAKE_AGY, home.path());
    let agent = agy_agent();

    write_script(
        cwd.path(),
        &json!({
            "conversation_uuid": Uuid::new_v4().to_string(),
            "create_brain_dir": false,
            "stdout": [],
            "exit_code": 0,
            "log_file_content": "E0526 13:59:05.636054  1754 log.go:398] agent executor error: rpc error: code = ResourceExhausted desc = Individual quota reached. Contact your administrator to enable overages. Resets in 143h34m25s.: Individual quota reached. Contact your administrator to enable overages. Resets in 143h34m25s.\n",
        }),
    );

    let events = dispatch(&adapter, &agent, cwd.path(), "hi").await;
    match outcome(&events) {
        TurnOutcome::Failed {
            kind: FailureKind::HarnessError,
            message,
        } => {
            assert!(
                message.contains("quota exhausted"),
                "authored quota prefix: {message}"
            );
            assert!(
                message.contains("Resets in 143h34m25s"),
                "reset-time tail carried through: {message}"
            );
            assert_eq!(
                message.matches("Individual quota reached").count(),
                1,
                "agy doubles the descriptive sentence; dedup must surface one copy: {message}"
            );
            assert!(
                !message.contains("without producing an answer"),
                "must not fall through to the generic no-answer message: {message}"
            );
        }
        other => panic!("expected HarnessError quota message, got {other:?}"),
    }
}

/// Two concurrent dispatches each scan their own `--log-file` — no
/// cross-attribution. The closed-the-misattribution-hole reason for
/// per-dispatch log isolation (vs. an mtime-windowed default-dir scan)
/// only matters under concurrency, so prove it directly.
#[tokio::test]
async fn concurrent_quota_dispatches_read_their_own_logs() {
    let home_a = tempfile::TempDir::new().unwrap();
    let home_b = tempfile::TempDir::new().unwrap();
    let cwd_a = tempfile::TempDir::new().unwrap();
    let cwd_b = tempfile::TempDir::new().unwrap();
    let adapter_a = AntigravityAdapter::with_binary_and_home(FAKE_AGY, home_a.path());
    let adapter_b = AntigravityAdapter::with_binary_and_home(FAKE_AGY, home_b.path());
    let agent_a = agy_agent();
    let agent_b = agy_agent();

    // Agent A: quota failure. Agent B: a different RPC failure (unknown code,
    // passes through as authored "Antigravity error: …").
    write_script(
        cwd_a.path(),
        &json!({
            "conversation_uuid": Uuid::new_v4().to_string(),
            "create_brain_dir": false,
            "stdout": [],
            "exit_code": 0,
            "log_file_content": "E0526 13:59:05 log.go:398] rpc error: code = ResourceExhausted desc = quota reached\n",
        }),
    );
    write_script(
        cwd_b.path(),
        &json!({
            "conversation_uuid": Uuid::new_v4().to_string(),
            "create_brain_dir": false,
            "stdout": [],
            "exit_code": 0,
            "log_file_content": "E0526 13:59:05 log.go:398] rpc error: code = Internal desc = backend exploded\n",
        }),
    );

    let (events_a, events_b) = tokio::join!(
        dispatch(&adapter_a, &agent_a, cwd_a.path(), "a"),
        dispatch(&adapter_b, &agent_b, cwd_b.path(), "b"),
    );

    match outcome(&events_a) {
        TurnOutcome::Failed {
            kind: FailureKind::HarnessError,
            message,
        } => assert!(
            message.contains("quota exhausted"),
            "A must see its own quota error, not B's Internal: {message}"
        ),
        other => panic!("A: expected quota HarnessError, got {other:?}"),
    }
    match outcome(&events_b) {
        TurnOutcome::Failed {
            kind: FailureKind::HarnessError,
            message,
        } => {
            assert!(
                message.contains("Antigravity error: ") && message.contains("backend exploded"),
                "B must see its own error's descriptive tail, not A's quota: {message}"
            );
            assert!(
                !message.contains("rpc error: code = "),
                "Go RPC boilerplate is stripped from the unknown-code message: {message}"
            );
            assert!(
                !message.contains("quota exhausted"),
                "B's message must not contain A's quota text: {message}"
            );
        }
        other => panic!("B: expected Internal HarnessError, got {other:?}"),
    }
}

/// Successful turn: the per-dispatch log is left untouched by the producer's
/// scan (only the no-answer branch reads it). Even if the log file is
/// staged with a quota line, a transcript terminal answer takes precedence
/// and the turn completes.
#[tokio::test]
async fn successful_turn_does_not_scan_or_misclassify_from_log() {
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
                {"json": user_record("hi", "2026-05-19T19:00:00Z"), "delay_ms": 0},
                {"json": terminal_record("2026-05-19T19:00:01Z", "ack"), "delay_ms": 0},
            ],
            "stdout": [drip("ack", 0)],
            "exit_code": 0,
            "log_file_content": "E0526 13:59:05 log.go:398] rpc error: code = ResourceExhausted desc = should never be surfaced\n",
        }),
    );

    let events = dispatch(&adapter, &agent, cwd.path(), "hi").await;
    assert!(
        matches!(outcome(&events), TurnOutcome::Completed),
        "successful turn must complete; log scan must not run: {:?}",
        outcome(&events)
    );
}
