use std::path::Path;

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_harness::{
    AdapterEvent, ContentKind, DispatchError, DispatchOptions, FailureKind, GeminiAdapter,
    HarnessAdapter, ToolKind, TurnOutcome,
};
use tempfile::TempDir;
use uuid::Uuid;

#[cfg(unix)]
use nix::unistd::{Pid, getpgid};

const FAKE_GEMINI: &str = env!("CARGO_BIN_EXE_fake_gemini");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gemini");

fn fixture(name: &str) -> String {
    format!("{FIXTURES}/{name}.jsonl")
}

fn gemini_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "gemini-test".to_owned(),
        harness: HarnessKind::Gemini,
        session_id: Some(Uuid::new_v4()),
        created_at: chrono::Utc::now(),
    }
}

/// Build an adapter that points at the fake binary and an empty home dir.
/// Empty home → no `~/.gemini/projects.json` → existence check returns
/// false → `build_args` picks `--session-id`. Tests that want `--resume`
/// can populate the home dir explicitly via `with_binary_and_home`.
fn adapter() -> (GeminiAdapter, TempDir) {
    let home = TempDir::new().unwrap();
    let adapter = GeminiAdapter::with_binary_and_home(FAKE_GEMINI, home.path());
    (adapter, home)
}

async fn collect_events(
    adapter: &GeminiAdapter,
    agent: &AgentRecord,
    fixture_path: &str,
) -> Vec<AdapterEvent> {
    let turn_id = Uuid::now_v7();
    let stream = adapter
        .dispatch(
            agent,
            Path::new("/tmp"),
            fixture_path,
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed");
    stream.collect().await
}

fn count_terminals(events: &[AdapterEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .count()
}

fn find_terminal(events: &[AdapterEvent]) -> &AdapterEvent {
    events
        .iter()
        .find(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("expected one TurnEnd")
}

#[tokio::test]
async fn happy_path_fixture_emits_session_meta_chunk_and_completed() {
    let (adapter, _home) = adapter();
    let agent = gemini_agent();
    let events = collect_events(&adapter, &agent, &fixture("happy-path.stream")).await;

    assert_eq!(count_terminals(&events), 1, "expected exactly one TurnEnd");

    let session_meta = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::SessionMeta { .. }))
        .expect("expected SessionMeta from init event");
    match session_meta {
        AdapterEvent::SessionMeta {
            model,
            mcp_servers,
            skills,
            ..
        } => {
            assert_eq!(model, "gemini-3-flash-preview");
            assert!(mcp_servers.is_empty());
            assert!(skills.is_empty());
        }
        _ => unreachable!(),
    }

    let chunks: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk {
                text,
                kind: ContentKind::Text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(chunks.join(""), "ack");

    match find_terminal(&events) {
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Completed,
            usage,
            ..
        } => {
            let usage = usage.as_ref().expect("usage on completed turn");
            assert_eq!(usage.input_tokens, 10178);
            assert_eq!(usage.output_tokens, 1);
            assert_eq!(usage.cached_input_tokens, Some(0));
        }
        other => panic!("expected Completed TurnEnd, got {other:?}"),
    }
}

#[tokio::test]
async fn tool_use_fixture_filters_update_topic_and_emits_read_file_pair() {
    let (adapter, _home) = adapter();
    let agent = gemini_agent();
    let events = collect_events(&adapter, &agent, &fixture("tool-use.stream")).await;

    // No update_topic events anywhere — neither ToolStarted nor ToolCompleted.
    for event in &events {
        match event {
            AdapterEvent::ToolStarted { name, .. } => {
                assert_ne!(name, "update_topic", "update_topic must be filtered");
            }
            AdapterEvent::ToolCompleted { tool_use_id, .. } => {
                assert!(
                    !tool_use_id.starts_with("update_topic_"),
                    "update_topic tool_result must be filtered"
                );
            }
            _ => {}
        }
    }

    // read_file lifecycle pair must be present.
    let read_started = events.iter().find(|e| {
        matches!(
            e,
            AdapterEvent::ToolStarted { name, kind: ToolKind::Builtin, .. } if name == "read_file"
        )
    });
    let read_completed = events.iter().find(|e| {
        matches!(
            e,
            AdapterEvent::ToolCompleted { tool_use_id, .. } if tool_use_id.starts_with("read_file_")
        )
    });
    assert!(read_started.is_some(), "expected read_file ToolStarted");
    assert!(read_completed.is_some(), "expected read_file ToolCompleted");

    // The two assistant chunks combine into the sentinel.
    let joined: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(joined, "SWITCHBOARD_GEMINI_PROBE_TOOL_5F8A21");

    assert!(matches!(
        find_terminal(&events),
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Completed,
            ..
        }
    ));
}

#[tokio::test]
async fn resume_fixture_emits_session_meta_with_resumed_session_id_model() {
    let (adapter, _home) = adapter();
    let agent = gemini_agent();
    let events = collect_events(&adapter, &agent, &fixture("resume.stream")).await;

    let session_meta = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::SessionMeta { .. }))
        .expect("resume.stream begins with init → SessionMeta");
    match session_meta {
        AdapterEvent::SessionMeta { raw, model, .. } => {
            assert_eq!(model, "gemini-3-flash-preview");
            // The raw init record carries the resumed session_id.
            assert_eq!(
                raw["session_id"], "00000000-0000-4000-8000-000000000001",
                "raw init must carry the session_id verbatim"
            );
        }
        _ => unreachable!(),
    }
    assert!(matches!(
        find_terminal(&events),
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Completed,
            ..
        }
    ));
}

#[tokio::test]
async fn error_invalid_model_fixture_emits_failed_with_harness_error_kind() {
    let (adapter, _home) = adapter();
    let agent = gemini_agent();
    let events = collect_events(&adapter, &agent, &fixture("error-invalid-model.stream")).await;

    match find_terminal(&events) {
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Failed { kind, message },
            ..
        } => {
            assert_eq!(*kind, FailureKind::HarnessError);
            assert!(
                message.contains("Requested entity was not found"),
                "expected error message to surface, got: {message:?}"
            );
        }
        other => panic!("expected Failed TurnEnd, got {other:?}"),
    }
}

#[tokio::test]
async fn auth_failure_inline_fixture_emits_auth_failure_kind() {
    // Inline-JSON fixture: M3.1 did not capture an auth-failure stream
    // because triggering one would break the developer's OAuth state. The
    // adapter's `result.status:"error"` path routes through
    // `is_gemini_auth_failure_message`, which matches any of three auth-signal
    // substrings. We pin the round-trip here.
    let tmp = TempDir::new().unwrap();
    let inline_path = tmp.path().join("auth-failure.stream.jsonl");
    let content = concat!(
        r#"{"type":"init","timestamp":"2026-05-17T00:00:00Z","session_id":"abc","model":"x"}"#,
        "\n",
        r#"{"type":"message","timestamp":"2026-05-17T00:00:00Z","role":"user","content":"hi"}"#,
        "\n",
        r#"{"type":"result","timestamp":"2026-05-17T00:00:00Z","status":"error","error":{"type":"auth","message":"API returned 401 Unauthorized — please re-authenticate"},"stats":{"total_tokens":0,"input_tokens":0,"output_tokens":0,"cached":0}}"#,
        "\n",
    );
    std::fs::write(&inline_path, content).unwrap();

    let (adapter, _home) = adapter();
    let agent = gemini_agent();
    let events = collect_events(&adapter, &agent, inline_path.to_str().unwrap()).await;

    match find_terminal(&events) {
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Failed { kind, message },
            ..
        } => {
            assert_eq!(*kind, FailureKind::AuthFailure);
            assert!(message.contains("401 Unauthorized"));
        }
        other => panic!("expected Failed(AuthFailure), got {other:?}"),
    }
}

#[tokio::test]
async fn eof_without_terminal_synthesizes_adapter_failure() {
    // Fixture: only init + a user message, no `result` event. The producer
    // EOFs without seeing a terminal; the adapter must synthesize one.
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("no-terminal.jsonl");
    let content = concat!(
        r#"{"type":"init","session_id":"abc","model":"x"}"#,
        "\n",
        r#"{"type":"message","role":"user","content":"hi"}"#,
        "\n",
    );
    std::fs::write(&path, content).unwrap();

    let (adapter, _home) = adapter();
    let agent = gemini_agent();
    let events = collect_events(&adapter, &agent, path.to_str().unwrap()).await;

    match find_terminal(&events) {
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Failed { kind, .. },
            ..
        } => {
            assert_eq!(*kind, FailureKind::AdapterFailure);
        }
        other => panic!("expected synthesized Failed(AdapterFailure), got {other:?}"),
    }
}

#[tokio::test]
async fn exit_42_with_actionable_stderr_classifies_via_helper() {
    // Fake binary supports `// exit:N` and `// stderr:<msg>`. Construct a
    // fixture with empty stdout (only comment lines) and exit 42 + a
    // non-auth stderr line → should land as AdapterFailure.
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("exit42-non-auth.jsonl");
    let content = "// stderr:YOLO mode is enabled. All tool calls will be automatically approved.\n\
                   // stderr:Error resuming session: Invalid session identifier \"x\".\n\
                   // exit:42\n";
    std::fs::write(&path, content).unwrap();

    let (adapter, _home) = adapter();
    let agent = gemini_agent();
    let events = collect_events(&adapter, &agent, path.to_str().unwrap()).await;

    match find_terminal(&events) {
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Failed { kind, message },
            ..
        } => {
            assert_eq!(*kind, FailureKind::AdapterFailure);
            assert!(
                message.contains("Invalid session identifier"),
                "expected the actionable stderr line in the message, got: {message:?}"
            );
        }
        other => panic!("expected Failed(AdapterFailure), got {other:?}"),
    }
}

#[tokio::test]
async fn exit_42_with_auth_stderr_classifies_as_auth_failure() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("exit42-auth.jsonl");
    let content = "// stderr:YOLO mode is enabled.\n\
                   // stderr:API returned 401 Unauthorized — please re-authenticate\n\
                   // exit:42\n";
    std::fs::write(&path, content).unwrap();

    let (adapter, _home) = adapter();
    let agent = gemini_agent();
    let events = collect_events(&adapter, &agent, path.to_str().unwrap()).await;

    match find_terminal(&events) {
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Failed { kind, .. },
            ..
        } => {
            assert_eq!(*kind, FailureKind::AuthFailure);
        }
        other => panic!("expected Failed(AuthFailure), got {other:?}"),
    }
}

#[tokio::test]
async fn dispatch_rejects_empty_prompt_with_invalid_prompt_error() {
    let (adapter, _home) = adapter();
    let agent = gemini_agent();
    let result = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "   \n\t",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await;
    assert!(matches!(result, Err(DispatchError::InvalidPrompt(_))));
}

#[tokio::test]
async fn unknown_event_type_is_skipped_and_does_not_break_stream() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("unknown-event.jsonl");
    let content = concat!(
        r#"{"type":"init","session_id":"abc","model":"x"}"#,
        "\n",
        r#"{"type":"some_future_event","payload":{}}"#,
        "\n",
        r#"{"type":"message","role":"assistant","content":"ok","delta":true}"#,
        "\n",
        r#"{"type":"result","status":"success","stats":{"input_tokens":1,"output_tokens":1,"cached":0}}"#,
        "\n",
    );
    std::fs::write(&path, content).unwrap();

    let (adapter, _home) = adapter();
    let agent = gemini_agent();
    let events = collect_events(&adapter, &agent, path.to_str().unwrap()).await;

    let chunks: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(chunks, "ok");
    assert!(matches!(
        find_terminal(&events),
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Completed,
            ..
        }
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn dispatch_puts_child_in_its_own_process_group() {
    // The fake binary writes its own pgid to a file when it sees the
    // `// pgid_to:<path>` directive. We then assert that pgid equals the
    // child's PID, which proves `Command::process_group(0)` was applied
    // (otherwise the child would inherit the parent's group).
    let tmp = TempDir::new().unwrap();
    let pgid_path = tmp.path().join("pgid.txt");
    let fixture_path = tmp.path().join("pgid-probe.jsonl");
    let content = format!(
        "// pgid_to:{}\n{}\n{}\n",
        pgid_path.display(),
        r#"{"type":"init","session_id":"abc","model":"x"}"#,
        r#"{"type":"result","status":"success","stats":{"input_tokens":0,"output_tokens":0,"cached":0}}"#,
    );
    std::fs::write(&fixture_path, content).unwrap();

    let (adapter, _home) = adapter();
    let agent = gemini_agent();
    let _events = collect_events(&adapter, &agent, fixture_path.to_str().unwrap()).await;

    let pgid_str = std::fs::read_to_string(&pgid_path).unwrap();
    let pgid: i32 = pgid_str.trim().parse().unwrap();
    // pgid equals the child's own PID → child is the group leader,
    // i.e., `process_group(0)` was applied. We can't observe the child PID
    // directly (it has exited by now), but the fake-binary contract is:
    // it writes the pgid it sees, and `getpgid(0)` (called by the binary)
    // returns the binary's own group. If the adapter didn't put the child
    // in its own group, this would equal the test runner's pgid instead.
    let runner_pgid = getpgid(Some(Pid::from_raw(0))).unwrap().as_raw();
    assert_ne!(
        pgid, runner_pgid,
        "fake_gemini's pgid must differ from the test runner's pgid \
         (indicating GeminiAdapter applied process_group(0) at spawn)"
    );
}
