//! Integration tests for `CodexAdapter`: drive the adapter end-to-end with
//! the `fake_codex` fixture binary, asserting on the emitted `AdapterEvent`
//! sequences. These run as part of `make test`; live tests against the real
//! `codex` CLI live in `crates/harness/tests/live.rs` (`#[ignore]`-gated).

use std::path::Path;

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_harness::{
    AdapterEvent, CodexAdapter, FailureKind, HarnessAdapter, ToolKind, TurnOutcome,
};
use uuid::Uuid;

const FAKE_CODEX: &str = env!("CARGO_BIN_EXE_fake_codex");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex");

fn fixture(name: &str) -> String {
    format!("{FIXTURES}/{name}.jsonl")
}

fn codex_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "test-codex".to_owned(),
        harness: HarnessKind::Codex,
        // Codex agents always have session_id = None (M2.3 invariant).
        session_id: None,
        created_at: chrono::Utc::now(),
    }
}

fn adapter() -> CodexAdapter {
    CodexAdapter::with_binary_path(FAKE_CODEX)
}

/// Dispatch the agent at the `fake_codex` binary with the named fixture as
/// the prompt (which `fake_codex` interprets as the fixture path). Drains the
/// stream to a `Vec<AdapterEvent>` for assertion.
async fn dispatch_fixture(
    agent: &AgentRecord,
    cwd: &Path,
    fixture_path: &str,
) -> Vec<AdapterEvent> {
    let turn_id = Uuid::now_v7();
    let stream = adapter()
        .dispatch(agent, cwd, fixture_path, turn_id)
        .await
        .expect("dispatch should succeed");
    stream.collect().await
}

#[tokio::test]
async fn text_only_fixture_yields_chunk_then_completed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();
    let events = dispatch_fixture(&agent, tmp.path(), &fixture("text-only")).await;

    let chunks: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(chunks, vec!["ack"]);

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(terminals.len(), 1);
    assert!(matches!(
        terminals[0],
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Completed,
            ..
        }
    ));
}

#[tokio::test]
async fn tool_use_fixture_yields_command_execution_sequence() {
    let tmp = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();
    let events = dispatch_fixture(&agent, tmp.path(), &fixture("tool-use")).await;
    // Expected sequence (from M2.1 fixture):
    //   ToolStarted(command_execution) → ToolCompleted → ContentChunk → TurnEnd(Completed)
    let tool_started = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::ToolStarted { .. }))
        .expect("ToolStarted expected");
    match tool_started {
        AdapterEvent::ToolStarted {
            kind,
            name,
            tool_use_id,
            ..
        } => {
            assert_eq!(*kind, ToolKind::Builtin);
            assert_eq!(name, "command_execution");
            assert_eq!(tool_use_id, "item_0");
        }
        _ => unreachable!(),
    }

    let tool_completed = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::ToolCompleted { .. }))
        .expect("ToolCompleted expected");
    match tool_completed {
        AdapterEvent::ToolCompleted {
            is_error,
            output,
            tool_use_id,
            ..
        } => {
            assert!(!is_error);
            assert_eq!(tool_use_id, "item_0");
            assert!(output.contains("alpha.txt"));
        }
        _ => unreachable!(),
    }

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::ContentChunk { .. }))
    );
    assert!(matches!(
        events.last().unwrap(),
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Completed,
            ..
        }
    ));
}

#[tokio::test]
async fn mcp_tool_call_fixture_yields_mcp_tool_started_with_server_dot_tool_name() {
    let tmp = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();
    let events = dispatch_fixture(&agent, tmp.path(), &fixture("mcp-tool-call")).await;

    let tool_started = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::ToolStarted { .. }))
        .expect("ToolStarted expected");
    match tool_started {
        AdapterEvent::ToolStarted { kind, name, .. } => {
            assert_eq!(*kind, ToolKind::Mcp);
            assert_eq!(name, "example_mcp_server.list_tags");
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn auth_failure_fixture_yields_auth_failure_terminal() {
    let tmp = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();
    let events = dispatch_fixture(&agent, tmp.path(), &fixture("auth-failure")).await;

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(terminals.len(), 1, "exactly one terminal event");
    match terminals[0] {
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Failed { kind, message },
            ..
        } => {
            assert_eq!(*kind, FailureKind::AuthFailure);
            assert!(message.contains("401 Unauthorized"));
        }
        other => panic!("expected TurnEnd(AuthFailure), got {other:?}"),
    }
}

#[tokio::test]
async fn errored_fixture_unwraps_json_encoded_message() {
    let tmp = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();
    let events = dispatch_fixture(&agent, tmp.path(), &fixture("errored")).await;

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(terminals.len(), 1);
    match terminals[0] {
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Failed { kind, message },
            ..
        } => {
            assert_eq!(*kind, FailureKind::HarnessError);
            assert!(
                message.contains("invalid-model-name"),
                "expected inner unwrapped message, got: {message}"
            );
        }
        other => panic!("expected TurnEnd(HarnessError) with unwrapped message, got {other:?}"),
    }
}

#[tokio::test]
async fn first_dispatch_writes_sidecar_with_captured_thread_id() {
    let tmp = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();
    let _ = dispatch_fixture(&agent, tmp.path(), &fixture("text-only")).await;

    // Sidecar path: <cwd>/.switchboard/projects/<project-id>/sessions/<agent-id>.jsonl
    let sidecar = tmp
        .path()
        .join(".switchboard")
        .join("projects")
        .join(agent.project_id.to_string())
        .join("sessions")
        .join(format!("{}.jsonl", agent.id));
    assert!(
        sidecar.is_file(),
        "sidecar must be created on first dispatch"
    );

    let content = std::fs::read_to_string(&sidecar).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1, "exactly one record after first dispatch");
    // Fixture's thread_id is 00000000-0000-7000-8000-000000000001.
    assert!(
        lines[0].contains("00000000-0000-7000-8000-000000000001"),
        "captured thread_id should land in the record, got: {}",
        lines[0]
    );
    // Sanity: shape includes the M2.4 contract fields.
    assert!(lines[0].contains("session_id"));
    assert!(lines[0].contains("original_start_date_utc"));
    assert!(lines[0].contains("started_at"));
}

#[tokio::test]
async fn resume_dispatch_appends_second_record_preserving_original_date() {
    // Pin two contract properties together:
    //
    //   (a) session_id on a record is whatever the dispatch's thread.started
    //       carried (in real Codex, the resumed thread_id echoes back the
    //       same id; in tests, it's whatever the fixture says — and we use
    //       two distinguishable fixtures here to prove the producer records
    //       *each dispatch's* captured id, not just the first).
    //   (b) original_start_date_utc is copied verbatim from the prior record
    //       on resume — never re-derived from Utc::today().
    //
    // The earlier first-fixture-twice shape couldn't catch a regression
    // that read the prior record but failed to copy original_start_date_utc,
    // because both records carried identical content from the same fixture.
    // Using a second fixture with a distinct thread_id exercises the
    // copy-vs-recompute distinction directly.
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture_alt_path = tmp.path().join("text-only-alt.jsonl");
    std::fs::write(
        &fixture_alt_path,
        r#"{"type":"thread.started","thread_id":"019aaaaa-bbbb-7777-8888-000000000042"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"ack-alt"}}
{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}
"#,
    )
    .unwrap();

    let agent = codex_agent();
    // First dispatch: canonical fixture (thread_id ends in 001).
    let _ = dispatch_fixture(&agent, tmp.path(), &fixture("text-only")).await;
    // Second dispatch: alt fixture with a DIFFERENT thread_id.
    let _ = dispatch_fixture(&agent, tmp.path(), fixture_alt_path.to_str().unwrap()).await;

    let sidecar = tmp
        .path()
        .join(".switchboard")
        .join("projects")
        .join(agent.project_id.to_string())
        .join("sessions")
        .join(format!("{}.jsonl", agent.id));
    let content = std::fs::read_to_string(&sidecar).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "second dispatch appends a record");
    let r1: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    let r2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    // session_id reflects each dispatch's captured thread.started (the
    // two fixtures emit different ids, so the recorded ids must differ).
    assert_eq!(r1["session_id"], "00000000-0000-7000-8000-000000000001");
    assert_eq!(r2["session_id"], "019aaaaa-bbbb-7777-8888-000000000042");
    // original_start_date_utc preserved verbatim across records — even
    // though session_id changed, the date must NOT be recomputed.
    assert_eq!(
        r1["original_start_date_utc"], r2["original_start_date_utc"],
        "resume must copy original_start_date_utc verbatim regardless of thread_id change"
    );
    // started_at differs per record (each dispatch gets a fresh wall-clock
    // stamp). Loose check — exact equality only on the date components is
    // not enforced; we just confirm both records carry the field.
    assert!(r1["started_at"].is_string());
    assert!(r2["started_at"].is_string());
}

#[tokio::test]
async fn truncated_stream_synthesizes_adapter_failure_with_buffered_error() {
    // Build an inline fixture that emits an `error` then drops without
    // turn.failed — adapter must synthesize AdapterFailure including the
    // buffered error message (unwrapped) and the EOF suffix.
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture_path = tmp.path().join("truncated.jsonl");
    std::fs::write(
        &fixture_path,
        r#"{"type":"thread.started","thread_id":"00000000-0000-7000-8000-000000000099"}
{"type":"turn.started"}
{"type":"error","message":"transient downstream failure"}
"#,
    )
    .unwrap();

    let agent = codex_agent();
    let cwd = tempfile::TempDir::new().unwrap();
    let turn_id = Uuid::now_v7();
    let stream = adapter()
        .dispatch(&agent, cwd.path(), fixture_path.to_str().unwrap(), turn_id)
        .await
        .expect("dispatch should succeed");
    let events: Vec<AdapterEvent> = stream.collect().await;

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(terminals.len(), 1, "exactly one terminal event");
    match terminals[0] {
        AdapterEvent::TurnEnd {
            outcome:
                TurnOutcome::Failed {
                    kind: FailureKind::AdapterFailure,
                    message,
                },
            ..
        } => {
            assert!(
                message.contains("transient downstream failure"),
                "buffered error must surface: {message}"
            );
            assert!(
                message.contains("stream ended without turn.failed"),
                "EOF marker must appear: {message}"
            );
        }
        other => panic!("expected TurnEnd(AdapterFailure), got {other:?}"),
    }
}

#[tokio::test]
async fn corrupt_thread_started_emits_adapter_failure() {
    // thread.started without a thread_id field — the sidecar can't be
    // written, so the adapter must fail-loud rather than silently produce
    // an unresumable agent.
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture_path = tmp.path().join("corrupt-thread.jsonl");
    std::fs::write(
        &fixture_path,
        r#"{"type":"thread.started"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"ack"}}
{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}
"#,
    )
    .unwrap();

    let agent = codex_agent();
    // Bounded: the stream must close promptly after the producer force-kills
    // the child. A regression that omitted the kill would leave the producer
    // task awaiting child.wait() forever (fake_codex sleeps via stderr
    // drain) and the test would hang past this timeout. 5s is generous;
    // healthy path closes in well under 100ms.
    let events = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        dispatch_fixture(&agent, tmp.path(), fixture_path.to_str().unwrap()),
    )
    .await
    .expect("stream must close promptly after corrupt thread.started");

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(
        terminals.len(),
        1,
        "exactly one terminal event from the corrupt-thread.started path"
    );
    match terminals[0] {
        AdapterEvent::TurnEnd {
            outcome:
                TurnOutcome::Failed {
                    kind: FailureKind::AdapterFailure,
                    message,
                },
            ..
        } => {
            assert!(
                message.contains("thread.started") && message.contains("thread_id"),
                "expected corrupt-thread-id explanation, got: {message}"
            );
        }
        other => panic!("expected TurnEnd(AdapterFailure), got {other:?}"),
    }

    // Sidecar must NOT exist — we refused to write a record without a
    // valid thread_id, so the next dispatch correctly sees no prior session.
    let sidecar = tmp
        .path()
        .join(".switchboard")
        .join("projects")
        .join(agent.project_id.to_string())
        .join("sessions")
        .join(format!("{}.jsonl", agent.id));
    assert!(
        !sidecar.exists(),
        "sidecar must not be written when thread_id is invalid"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn sidecar_write_failure_terminates_stream_with_adapter_failure() {
    // Goal: prove that an in-stream sidecar-write failure (post-dispatch,
    // during the producer task's first thread.started capture) synthesizes
    // TurnEnd(AdapterFailure) and stops the stream. To trigger a
    // write-but-not-read failure, pre-create an empty sidecar file and
    // chmod it 444 — `read_latest` opens, sees no records, returns Ok(None);
    // `append_record` then fails on PermissionDenied when reopening for
    // append. Unix-only because Windows file permissions don't behave the
    // same way.
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();
    let sidecar = tmp
        .path()
        .join(".switchboard")
        .join("projects")
        .join(agent.project_id.to_string())
        .join("sessions")
        .join(format!("{}.jsonl", agent.id));
    std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    std::fs::write(&sidecar, b"").unwrap();
    let mut perms = std::fs::metadata(&sidecar).unwrap().permissions();
    perms.set_mode(0o444); // read-only
    std::fs::set_permissions(&sidecar, perms).unwrap();

    // Bounded: the producer must force-kill the child after emitting the
    // AdapterFailure event so the stream closes promptly. A regression that
    // emitted the event but failed to kill would let the producer task
    // hang awaiting child.wait() while the consumer's stream stayed open;
    // this timeout catches that.
    let events = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        dispatch_fixture(&agent, tmp.path(), &fixture("text-only")),
    )
    .await
    .expect("stream must close promptly after sidecar write failure");
    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(terminals.len(), 1, "exactly one terminal event");
    match terminals[0] {
        AdapterEvent::TurnEnd {
            outcome:
                TurnOutcome::Failed {
                    kind: FailureKind::AdapterFailure,
                    message,
                },
            ..
        } => {
            assert!(
                message.contains("sidecar write failed"),
                "expected sidecar-write-failure message, got: {message}"
            );
        }
        other => panic!("expected TurnEnd(AdapterFailure), got {other:?}"),
    }
}

#[tokio::test]
async fn dispatch_with_corrupt_sidecar_returns_pre_stream_read_error() {
    // The Codex adapter must fail loudly on corrupt sidecar JSONL (per
    // AGENTS.md cross-cutting invariant). This is the dispatch-time path
    // (before any stream is established).
    let tmp = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();
    let path = tmp
        .path()
        .join(".switchboard")
        .join("projects")
        .join(agent.project_id.to_string())
        .join("sessions")
        .join(format!("{}.jsonl", agent.id));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{not valid json\n").unwrap();

    let result = adapter()
        .dispatch(&agent, tmp.path(), &fixture("text-only"), Uuid::now_v7())
        .await;
    assert!(
        matches!(
            result,
            Err(switchboard_harness::DispatchError::PreStreamRead(_))
        ),
        "expected PreStreamRead error on corrupt sidecar"
    );
}
