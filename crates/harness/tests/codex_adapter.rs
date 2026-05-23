//! Integration tests for `CodexAdapter`: drive the adapter end-to-end with
//! the `fake_codex` fixture binary, asserting on the emitted `AdapterEvent`
//! sequences. These run as part of `make test`; live tests against the real
//! `codex` CLI live in `crates/harness/tests/live.rs` (`#[ignore]`-gated).

use std::path::Path;

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_harness::{
    AdapterEvent, CodexAdapter, DispatchOptions, FailureKind, HarnessAdapter, ToolKind, TurnOutcome,
};
use uuid::Uuid;

#[cfg(unix)]
use nix::unistd::{Pid, getpgid};
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio_util::sync::CancellationToken;

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
        // Codex agents always have session_id = None.
        session_id: None,
        created_at: chrono::Utc::now(),
    }
}

/// Dispatch the agent at the `fake_codex` binary with the named fixture as
/// the prompt (which `fake_codex` interprets as the fixture path). Drains
/// the stream to a `Vec<AdapterEvent>` for assertion. Always injects a
/// fresh empty `home_dir` so post-terminal enrichment runs against an
/// empty ~/.codex/sessions (no developer-environment leakage) — and
/// degrades gracefully to default-Enrichment (no derived events).
async fn dispatch_fixture(
    agent: &AgentRecord,
    cwd: &Path,
    fixture_path: &str,
) -> Vec<AdapterEvent> {
    let home = tempfile::TempDir::new().unwrap();
    let turn_id = Uuid::now_v7();
    let adapter = CodexAdapter::with_binary_and_home(FAKE_CODEX, home.path());
    let stream = adapter
        .dispatch(
            agent,
            cwd,
            fixture_path,
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed");
    stream.collect().await
    // `home` drops after the stream is fully collected; the producer task
    // is done by then so no use-after-drop risk.
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
    // Expected sequence (from the captured fixture):
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
    // Sanity: shape includes the sidecar schema fields.
    assert!(lines[0].contains("session_id"));
    assert!(lines[0].contains("session_partition_date"));
    assert!(lines[0].contains("started_at"));
}

#[tokio::test]
async fn first_dispatch_captures_local_date_not_utc() {
    // Pins the date-partition contract: Codex partitions session files by
    // **local** date, so `try_persist_sidecar` captures
    // `chrono::Local::now().date_naive()` (not `Utc::now()`). For hosts
    // whose local date differs from UTC at sidecar write time (any user
    // west of UTC after ~16:00 local, or east of UTC before ~08:00), a
    // regression to `Utc::now()` surfaces here.
    //
    // **Detection is timezone-conditional.** On hosts where local date
    // equals UTC date (CI at UTC; users at UTC) `Local::now() == Utc::now()`
    // collapses the two paths, and a reverted-to-`Utc::now()` regression
    // would pass this test. Clock-injection is the proper fix; deferred.
    // For now this test is a developer-machine drift catcher (which is
    // where `make test-live` runs anyway) plus a JSON-field-name pin.
    //
    // The acceptance window is `[local_before, local_after]` — a dispatch
    // that legitimately straddles local midnight may write either date.
    // Accepting arbitrary "yesterday" would be too loose: a `Utc::now()`
    // regression on a UTC+N host during early-morning hours can produce
    // yesterday-local while local-today is the right answer, and the
    // before/after window correctly rejects that case.
    let tmp = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();

    let local_before = chrono::Local::now().date_naive();
    let _ = dispatch_fixture(&agent, tmp.path(), &fixture("text-only")).await;
    let local_after = chrono::Local::now().date_naive();

    let sidecar = tmp
        .path()
        .join(".switchboard")
        .join("projects")
        .join(agent.project_id.to_string())
        .join("sessions")
        .join(format!("{}.jsonl", agent.id));
    let content = std::fs::read_to_string(&sidecar).unwrap();
    let record: serde_json::Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();
    let written_date: chrono::NaiveDate =
        serde_json::from_value(record["session_partition_date"].clone()).unwrap();

    assert!(
        written_date == local_before || written_date == local_after,
        "session_partition_date must be the LOCAL date at dispatch time \
         (one of [{local_before}, {local_after}]), not the UTC date. wrote {written_date}",
    );
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
    //   (b) session_partition_date is copied verbatim from the prior record
    //       on resume — never re-derived from Utc::today().
    //
    // The earlier first-fixture-twice shape couldn't catch a regression
    // that read the prior record but failed to copy session_partition_date,
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
    // session_partition_date preserved verbatim across records — even
    // though session_id changed, the date must NOT be recomputed.
    assert_eq!(
        r1["session_partition_date"], r2["session_partition_date"],
        "resume must copy session_partition_date verbatim regardless of thread_id change"
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
    let home = tempfile::TempDir::new().unwrap();
    let turn_id = Uuid::now_v7();
    let stream = CodexAdapter::with_binary_and_home(FAKE_CODEX, home.path())
        .dispatch(
            &agent,
            cwd.path(),
            fixture_path.to_str().unwrap(),
            turn_id,
            DispatchOptions::default(),
        )
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
async fn malformed_json_mid_stream_force_kills_and_emits_adapter_failure() {
    // The ParseOutcome::Error branch in run_producer's main loop sets
    // force_kill_child and breaks. This test exercises that path with a
    // two-process tree (via the spawn_child_holding_stderr directive),
    // proving both that (a) malformed mid-stream JSON terminates the
    // stream promptly and (b) the kill reaches the descendant — without
    // killpg, the forked child would keep stderr open and the producer
    // would hang past the 5s budget.
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture_path = tmp.path().join("malformed-mid-stream.jsonl");
    std::fs::write(
        &fixture_path,
        r#"// spawn_child_holding_stderr
{"type":"thread.started","thread_id":"00000000-0000-7000-8000-0000000000bb"}
this is not valid json mid-stream
"#,
    )
    .unwrap();

    let agent = codex_agent();
    let events = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        dispatch_fixture(&agent, tmp.path(), fixture_path.to_str().unwrap()),
    )
    .await
    .expect("stream must close promptly after malformed JSON triggers force-kill");

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(
        terminals.len(),
        1,
        "exactly one terminal event from the malformed-JSON path"
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
                message.contains("malformed JSON"),
                "expected 'malformed JSON' in error message, got: {message}"
            );
        }
        other => panic!("expected TurnEnd(AdapterFailure), got: {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn force_kill_signals_whole_process_group_not_just_parent() {
    // Codex's CLI is a two-process tree: a Node parent that spawns a Rust
    // child holding the actual model work. The Rust child inherits stdout
    // / stderr pipes from the parent. If the producer's force_kill_child
    // path used plain `child.kill()` (which signals only the parent PID),
    // the Rust child would keep stderr open and the producer's
    // stderr_task.await would hang on EOF that never arrives.
    //
    // This fixture uses fake_codex's `// spawn_child_holding_stderr`
    // directive to fork a child that inherits stderr and sleeps forever.
    // Then emits a corrupt thread.started to trigger force_kill_child.
    // With killpg (the correct fix), both processes die and the stream
    // closes in milliseconds. With plain kill, this hangs past the 5s
    // timeout.
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture_path = tmp.path().join("two-process-corrupt.jsonl");
    std::fs::write(
        &fixture_path,
        r#"// spawn_child_holding_stderr
{"type":"thread.started"}
"#,
    )
    .unwrap();

    let agent = codex_agent();
    let events = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        dispatch_fixture(&agent, tmp.path(), fixture_path.to_str().unwrap()),
    )
    .await
    .expect("stream must close promptly — killpg must signal the forked child too");

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(
        terminals.len(),
        1,
        "exactly one terminal event from the corrupt-thread.started path"
    );
    assert!(
        matches!(
            terminals[0],
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Failed {
                    kind: FailureKind::AdapterFailure,
                    ..
                },
                ..
            }
        ),
        "expected TurnEnd(AdapterFailure), got: {:?}",
        terminals[0]
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

    let home = tempfile::TempDir::new().unwrap();
    let result = CodexAdapter::with_binary_and_home(FAKE_CODEX, home.path())
        .dispatch(
            &agent,
            tmp.path(),
            &fixture("text-only"),
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await;
    assert!(
        matches!(
            result,
            Err(switchboard_harness::DispatchError::PreStreamRead(_))
        ),
        "expected PreStreamRead error on corrupt sidecar"
    );
}

// --- Post-terminal enrichment ---
//
// These tests stage a temp `home_dir` (via `CodexAdapter::with_binary_and_home`)
// and pre-write a Codex session file at the path the adapter will look up
// via the sidecar's `session_id` + `session_partition_date`. The fixture's
// hardcoded thread_id (`00000000-0000-7000-8000-000000000001`) is what
// `fake_codex` echoes back via the `thread.started` stream event, so the
// sidecar's session_id is predictable for staging.

const FIXTURE_THREAD_ID: &str = "00000000-0000-7000-8000-000000000001";

/// The session-file content used by the enrichment tests below. Pinned
/// inline rather than reading the `rate-limits.session.jsonl` fixture
/// directly because the tests assert specific numeric values; coupling the
/// test to the fixture would make a future fixture refresh silently break
/// the assertions.
const ENRICHMENT_SESSION_CONTENT: &str = r#"{"timestamp":"2026-01-01T00:00:00.000Z","type":"session_meta","payload":{"cli_version":"0.130.0","base_instructions":{"text":"long system prompt"}}}
{"timestamp":"2026-01-01T00:00:00.500Z","type":"turn_context","payload":{"model":"gpt-5.5","cwd":"/example/cwd"}}
{"timestamp":"2026-01-01T00:00:01.000Z","type":"event_msg","payload":{"type":"task_started","model_context_window":258400}}
{"timestamp":"2026-01-01T00:00:01.500Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"primary":{"used_percent":42.0,"window_minutes":300}}}}
"#;

fn session_file_path(home: &Path, date: chrono::NaiveDate, session_id: &str) -> std::path::PathBuf {
    // Mirrors the layout `session_file::session_directory` computes in
    // production: `<home>/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-*-<uuid>.jsonl`.
    home.join(".codex")
        .join("sessions")
        .join(date.format("%Y").to_string())
        .join(date.format("%m").to_string())
        .join(date.format("%d").to_string())
        .join(format!("rollout-1747000000000-{session_id}.jsonl"))
}

fn stage_session_file(
    home: &Path,
    date: chrono::NaiveDate,
    session_id: &str,
    content: &str,
) -> std::path::PathBuf {
    let path = session_file_path(home, date, session_id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    path
}

fn write_sidecar(
    cwd: &Path,
    agent: &AgentRecord,
    session_id: &str,
    session_partition_date: chrono::NaiveDate,
) {
    let sidecar = cwd
        .join(".switchboard")
        .join("projects")
        .join(agent.project_id.to_string())
        .join("sessions")
        .join(format!("{}.jsonl", agent.id));
    std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    let record = serde_json::json!({
        "session_id": session_id,
        "session_partition_date": session_partition_date.format("%Y-%m-%d").to_string(),
        "started_at": "2026-05-15T00:00:00Z",
    });
    std::fs::write(&sidecar, format!("{record}\n")).unwrap();
}

async fn dispatch_with_home(
    agent: &AgentRecord,
    cwd: &Path,
    home: &Path,
    fixture_path: &str,
) -> Vec<AdapterEvent> {
    dispatch_with_home_and_options(agent, cwd, home, fixture_path, DispatchOptions::default()).await
}

async fn dispatch_with_home_and_options(
    agent: &AgentRecord,
    cwd: &Path,
    home: &Path,
    fixture_path: &str,
    options: DispatchOptions,
) -> Vec<AdapterEvent> {
    let turn_id = Uuid::now_v7();
    let adapter = CodexAdapter::with_binary_and_home(FAKE_CODEX, home);
    let stream = adapter
        .dispatch(agent, cwd, fixture_path, turn_id, options)
        .await
        .expect("dispatch should succeed");
    stream.collect().await
}

#[tokio::test]
async fn first_turn_emits_enriched_turn_end_rate_limit_and_session_meta() {
    // First-turn dispatch with a real session file staged at today's date
    // partition + MCP config + a skill. Asserts the full enriched event
    // sequence: TurnEnd(enriched) → RateLimitEvent → SessionMeta.
    let cwd = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();

    // Stage session file at today's local date (first-turn case — the
    // adapter's `try_persist_sidecar` captures `chrono::Local::now().date_naive()`
    // for `session_partition_date` because Codex partitions session files
    // by local date, not UTC).
    let today = chrono::Local::now().date_naive();
    stage_session_file(
        home.path(),
        today,
        FIXTURE_THREAD_ID,
        ENRICHMENT_SESSION_CONTENT,
    );

    // Stage an MCP config at user scope.
    let config_dir = home.path().join(".codex");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
[mcp_servers.user_alpha]
command = "x"
"#,
    )
    .unwrap();

    // Stage a skill at user scope.
    let skill_dir = home
        .path()
        .join(".agents")
        .join("skills")
        .join("user_skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "# skill").unwrap();

    let events = dispatch_with_home(&agent, cwd.path(), home.path(), &fixture("text-only")).await;

    // Locate the TurnEnd, RateLimitEvent, SessionMeta — and verify ordering.
    let terminal_idx = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("TurnEnd present");
    let rate_limit_idx = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::RateLimitEvent { .. }))
        .expect("RateLimitEvent emitted post-terminal");
    let session_meta_idx = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::SessionMeta { .. }))
        .expect("SessionMeta emitted on first turn");
    assert!(
        terminal_idx < rate_limit_idx && rate_limit_idx < session_meta_idx,
        "order must be TurnEnd → RateLimitEvent → SessionMeta; got indices {terminal_idx}, {rate_limit_idx}, {session_meta_idx}"
    );

    // Enriched context_window from task_started.model_context_window.
    match &events[terminal_idx] {
        AdapterEvent::TurnEnd { usage: Some(u), .. } => {
            assert_eq!(
                u.context_window,
                Some(258_400),
                "context_window enriched from session file"
            );
        }
        other => panic!("expected TurnEnd with Some(usage), got {other:?}"),
    }

    // RateLimitEvent.info carries the rate_limits object verbatim.
    match &events[rate_limit_idx] {
        AdapterEvent::RateLimitEvent { info, .. } => {
            assert_eq!(
                info.pointer("/primary/used_percent"),
                Some(&serde_json::Value::from(42.0))
            );
        }
        _ => unreachable!(),
    }

    // SessionMeta carries model, harness_version, MCP server, skill, and
    // base_instructions.text is stripped from raw.
    match &events[session_meta_idx] {
        AdapterEvent::SessionMeta {
            model,
            harness_version,
            mcp_servers,
            skills,
            tools,
            raw,
            ..
        } => {
            assert_eq!(model, "gpt-5.5");
            assert_eq!(harness_version, "0.130.0");
            assert!(tools.is_empty(), "tools is vec![] for Codex");
            assert!(
                mcp_servers.iter().any(|s| s.name == "user_alpha"),
                "merged MCP servers must include user_alpha"
            );
            assert_eq!(skills, &vec!["user_skill".to_owned()]);
            // base_instructions.text must be stripped to keep IPC payloads small.
            let stripped = raw.pointer("/payload/base_instructions/text");
            assert_eq!(
                stripped,
                Some(&serde_json::Value::String(
                    "<stripped — see codex-cli-observed.md>".to_owned()
                )),
                "base_instructions.text must be stripped in raw"
            );
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn resume_turn_omits_session_meta_but_still_emits_rate_limit_and_enriches() {
    // Pre-write a sidecar to mark this as a resume from the adapter's
    // point of view. SessionMeta is first-turn-only (prior.is_none() → true
    // only when sidecar is absent at dispatch start).
    let cwd = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();
    let today = chrono::Utc::now().date_naive();
    write_sidecar(cwd.path(), &agent, FIXTURE_THREAD_ID, today);
    stage_session_file(
        home.path(),
        today,
        FIXTURE_THREAD_ID,
        ENRICHMENT_SESSION_CONTENT,
    );

    let events = dispatch_with_home(&agent, cwd.path(), home.path(), &fixture("text-only")).await;

    let terminal_idx = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("TurnEnd present");
    let rate_limit_idx = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::RateLimitEvent { .. }))
        .expect("RateLimitEvent emitted every turn");
    assert!(
        terminal_idx < rate_limit_idx,
        "RateLimitEvent must follow TurnEnd on resume turns too; got indices {terminal_idx}, {rate_limit_idx}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AdapterEvent::SessionMeta { .. })),
        "SessionMeta MUST NOT fire on resume turns — got {events:#?}"
    );
    // TurnEnd is still enriched.
    let enriched_window = events.iter().find_map(|e| match e {
        AdapterEvent::TurnEnd { usage: Some(u), .. } => Some(u.context_window),
        _ => None,
    });
    assert_eq!(
        enriched_window,
        Some(Some(258_400)),
        "context_window enriched on resume turns too"
    );
}

#[tokio::test]
async fn attach_flow_first_dispatch_forces_session_meta_despite_sidecar_present() {
    // Pre-write sidecar (mimicking the attach-existing-session flow). The
    // adapter's prior.is_none() heuristic would normally classify this as
    // a resume and skip SessionMeta — leaving the sidebar's MCP/skills/model
    // listing empty for attached Codex agents until some other path fires.
    //
    // With DispatchOptions::is_first_dispatch_after_attach = true, the
    // adapter must treat the dispatch as a first turn and emit SessionMeta.
    let cwd = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();
    let today = chrono::Utc::now().date_naive();
    // Sidecar exists at dispatch start — without the override this would
    // suppress SessionMeta.
    write_sidecar(cwd.path(), &agent, FIXTURE_THREAD_ID, today);
    stage_session_file(
        home.path(),
        today,
        FIXTURE_THREAD_ID,
        ENRICHMENT_SESSION_CONTENT,
    );

    let options = DispatchOptions {
        is_first_dispatch_after_attach: true,
        ..Default::default()
    };
    let events = dispatch_with_home_and_options(
        &agent,
        cwd.path(),
        home.path(),
        &fixture("text-only"),
        options,
    )
    .await;

    let session_meta = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::SessionMeta { .. }));
    assert!(
        session_meta.is_some(),
        "is_first_dispatch_after_attach must force SessionMeta on a resume dispatch; got events: {events:#?}"
    );
}

#[tokio::test]
async fn cross_midnight_uses_sidecar_date_not_today() {
    // Sidecar's session_partition_date says yesterday; host clock says
    // today. Lookup must use the sidecar's date.
    //
    // We can't move the host clock backwards in this test, so we stage the
    // file at "yesterday" relative to Utc::now(). The adapter, on this
    // resume dispatch, must read the sidecar (yesterday) and find the file
    // — not call Utc::today() and look in today's empty directory.
    let cwd = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();
    let yesterday = chrono::Utc::now().date_naive() - chrono::Duration::days(1);
    write_sidecar(cwd.path(), &agent, FIXTURE_THREAD_ID, yesterday);
    stage_session_file(
        home.path(),
        yesterday,
        FIXTURE_THREAD_ID,
        ENRICHMENT_SESSION_CONTENT,
    );

    let events = dispatch_with_home(&agent, cwd.path(), home.path(), &fixture("text-only")).await;

    let enriched_window = events.iter().find_map(|e| match e {
        AdapterEvent::TurnEnd { usage: Some(u), .. } => Some(u.context_window),
        _ => None,
    });
    assert_eq!(
        enriched_window,
        Some(Some(258_400)),
        "lookup must use sidecar's session_partition_date (yesterday), not today"
    );
    // Also confirms the RateLimitEvent path traverses the cross-day file.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::RateLimitEvent { .. })),
        "RateLimitEvent found in yesterday's session file"
    );
}

#[tokio::test]
async fn missing_session_file_emits_unenriched_turn_end_and_no_derived_events() {
    // No staged session file at all → load_with_retry returns
    // Enrichment::default() → TurnEnd has usage with context_window: None,
    // no RateLimitEvent, no SessionMeta (cli_version + model both absent →
    // build_session_meta_fields returns None).
    let cwd = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();

    let events = dispatch_with_home(&agent, cwd.path(), home.path(), &fixture("text-only")).await;

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(terminals.len(), 1, "exactly one TurnEnd");
    match terminals[0] {
        AdapterEvent::TurnEnd {
            usage: Some(u),
            outcome: TurnOutcome::Completed,
            ..
        } => {
            // Stream-derived usage is present; context_window is None
            // because enrichment found nothing.
            assert!(
                u.context_window.is_none(),
                "no session file → context_window stays None"
            );
        }
        other => panic!("expected TurnEnd(Completed) with Some(usage), got {other:?}"),
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AdapterEvent::RateLimitEvent { .. })),
        "no rate_limits found → no RateLimitEvent"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AdapterEvent::SessionMeta { .. })),
        "no model/cli_version found → no SessionMeta"
    );
}

#[tokio::test]
async fn project_scope_mcp_config_overlays_user_scope() {
    // Project-scope <cwd>/.codex/config.toml entries appear alongside
    // user-scope entries in the SessionMeta.mcp_servers list.
    let cwd = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let agent = codex_agent();
    // First-turn case: adapter writes sidecar with `Local::now()`. Stage
    // the session file at the same local date.
    let today = chrono::Local::now().date_naive();
    stage_session_file(
        home.path(),
        today,
        FIXTURE_THREAD_ID,
        ENRICHMENT_SESSION_CONTENT,
    );

    let user_config_dir = home.path().join(".codex");
    std::fs::create_dir_all(&user_config_dir).unwrap();
    std::fs::write(
        user_config_dir.join("config.toml"),
        r#"
[mcp_servers.from_user]
command = "u"
"#,
    )
    .unwrap();

    let project_config_dir = cwd.path().join(".codex");
    std::fs::create_dir_all(&project_config_dir).unwrap();
    std::fs::write(
        project_config_dir.join("config.toml"),
        r#"
[mcp_servers.from_project]
command = "p"
"#,
    )
    .unwrap();

    let events = dispatch_with_home(&agent, cwd.path(), home.path(), &fixture("text-only")).await;

    let session_meta = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::SessionMeta { mcp_servers, .. } => Some(mcp_servers.clone()),
            _ => None,
        })
        .expect("SessionMeta emitted on first turn");
    let names: Vec<&str> = session_meta.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"from_user") && names.contains(&"from_project"),
        "merged registry must include both scopes; got {names:?}"
    );
    assert!(
        session_meta.iter().all(|s| s.status == "configured"),
        "all entries report status='configured'"
    );
}

/// Poll until `path` holds a parseable pgid; bounded so a spawn failure panics.
#[cfg(unix)]
async fn wait_for_pgid(path: &Path) -> Pid {
    for _ in 0..200 {
        if let Ok(s) = std::fs::read_to_string(path)
            && let Ok(n) = s.trim().parse::<i32>()
        {
            return Pid::from_raw(n);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("fake_codex never wrote its pgid at {}", path.display());
}

/// Poll until the process group's leader is gone (`getpgid` → `ESRCH`).
#[cfg(unix)]
async fn assert_group_reaped(leader: Pid) {
    for _ in 0..200 {
        if getpgid(Some(leader)).is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("process group {leader} still alive after cancel");
}

#[cfg(unix)]
#[tokio::test]
async fn cancel_kills_whole_group_and_emits_no_terminal() {
    // Codex is the load-bearing cancel case: it exits 0 on SIGTERM, emits no
    // terminal event, and runs a two-process tree. The fixture spawns a child
    // that holds the stderr pipe open, then `// hang`s. On cancel the adapter
    // must `killpg` the WHOLE group (not just the parent) — otherwise the
    // spawned child keeps stderr open and the stderr-drain `.await` hangs,
    // tripping this test's timeout. And the adapter must emit no terminal
    // (the dispatcher synthesizes `Cancelled`).
    let cwd = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let pgid_path = cwd.path().join("child.pgid");
    let fixture_path = cwd.path().join("cancel-fixture.jsonl");
    let body = format!(
        "// pgid_to:{}\n// spawn_child_holding_stderr\n// hang\n",
        pgid_path.display()
    );
    std::fs::write(&fixture_path, body).unwrap();

    let adapter = CodexAdapter::with_binary_and_home(FAKE_CODEX, home.path());
    let token = CancellationToken::new();
    let options = DispatchOptions {
        cancel_token: token.clone(),
        ..Default::default()
    };
    let stream = adapter
        .dispatch(
            &codex_agent(),
            cwd.path(),
            fixture_path.to_str().unwrap(),
            Uuid::now_v7(),
            options,
        )
        .await
        .expect("dispatch should succeed");

    let leader = wait_for_pgid(&pgid_path).await;
    token.cancel();

    let events: Vec<AdapterEvent> = tokio::time::timeout(Duration::from_secs(15), stream.collect())
        .await
        .expect("stream must end promptly after cancel (no stderr-pipe hang), not time out");

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AdapterEvent::TurnEnd { .. })),
        "adapter must emit no terminal event on cancel; got: {events:?}"
    );
    assert_group_reaped(leader).await;
}

#[cfg(unix)]
#[tokio::test]
async fn cancel_sweeps_sigterm_immune_descendant() {
    // Regression guard for `terminate_then_kill`'s unconditional final group
    // SIGKILL. The spawned child IGNORES SIGTERM and holds the inherited stderr
    // pipe, while fake_codex (the parent) dies on SIGTERM. If the helper only
    // waited on the parent (conditional SIGKILL), the immune child would keep
    // stderr open and the adapter's stderr drain would block forever — this
    // test's timeout would trip. With the final group SIGKILL, the child is
    // reaped and the stream ends.
    let cwd = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let pgid_path = cwd.path().join("child.pgid");
    let fixture_path = cwd.path().join("immune-fixture.jsonl");
    let body = format!(
        "// pgid_to:{}\n// spawn_sigterm_immune_child_holding_stderr\n// hang\n",
        pgid_path.display()
    );
    std::fs::write(&fixture_path, body).unwrap();

    let adapter = CodexAdapter::with_binary_and_home(FAKE_CODEX, home.path());
    let token = CancellationToken::new();
    let options = DispatchOptions {
        cancel_token: token.clone(),
        ..Default::default()
    };
    let stream = adapter
        .dispatch(
            &codex_agent(),
            cwd.path(),
            fixture_path.to_str().unwrap(),
            Uuid::now_v7(),
            options,
        )
        .await
        .expect("dispatch should succeed");

    let leader = wait_for_pgid(&pgid_path).await;
    token.cancel();

    let events: Vec<AdapterEvent> = tokio::time::timeout(Duration::from_secs(15), stream.collect())
        .await
        .expect(
            "stream must end after cancel — the SIGTERM-immune descendant must be SIGKILLed by \
             the final group sweep, not left holding stderr",
        );

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AdapterEvent::TurnEnd { .. })),
        "adapter must emit no terminal event on cancel; got: {events:?}"
    );
    assert_group_reaped(leader).await;
}
