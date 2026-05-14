use std::path::Path;

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_harness::{
    AdapterEvent, ClaudeCodeAdapter, DispatchError, FailureKind, HarnessAdapter, TurnOutcome,
};
use uuid::Uuid;

const FAKE_CLAUDE: &str = env!("CARGO_BIN_EXE_fake_claude");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(name: &str) -> String {
    format!("{FIXTURES}/{name}.jsonl")
}

fn fake_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "test-agent".to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_id: Some(Uuid::now_v7()),
        created_at: chrono::Utc::now(),
    }
}

fn adapter() -> ClaudeCodeAdapter {
    ClaudeCodeAdapter::with_binary_path(FAKE_CLAUDE)
}

async fn collect_events(
    adapter: &ClaudeCodeAdapter,
    agent: &AgentRecord,
    fixture_path: &str,
) -> Vec<AdapterEvent> {
    let turn_id = Uuid::now_v7();
    let stream = adapter
        .dispatch(agent, Path::new("/tmp"), fixture_path, turn_id)
        .await
        .expect("dispatch should succeed");
    stream.collect().await
}

#[tokio::test]
async fn text_only_fixture_yields_chunks_then_completed() {
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("text-only")).await;

    let chunks: Vec<&str> = events
        .iter()
        .filter_map(|e| {
            if let AdapterEvent::ContentChunk { text, .. } = e {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(!chunks.is_empty(), "expected at least one ContentChunk");
    let joined = chunks.join("");
    assert!(
        joined.contains('4'),
        "expected '4' in joined text, got: {joined:?}"
    );

    // Exactly one terminal event, and it must be Completed.
    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(terminals.len(), 1, "expected exactly one TurnEnd");
    assert!(
        matches!(
            terminals[0],
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        ),
        "expected TurnEnd(Completed), got: {terminals:?}"
    );
}

#[tokio::test]
async fn text_only_no_double_emit_from_assistant_message() {
    // The terminal `assistant` message carries the same text as the deltas.
    // The parser must emit text ONLY from text_delta events, never from the
    // assistant-message wrapper — otherwise the text would appear twice.
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("text-only")).await;

    let chunk_text: String = events
        .iter()
        .filter_map(|e| {
            if let AdapterEvent::ContentChunk { text, .. } = e {
                Some(text.clone())
            } else {
                None
            }
        })
        .collect();

    // The fixture has 5 deltas that join to "Two plus two equals 4."
    // If double-emit were happening, we'd see that text repeated.
    let expected = "Two plus two equals 4.";
    assert_eq!(
        chunk_text, expected,
        "ContentChunk text should match delta stream exactly (no double-emit)"
    );
}

#[tokio::test]
async fn tool_use_fixture_skips_tool_events_and_yields_text() {
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("tool-use")).await;

    // Only text ContentChunks — tool input_json_delta events must be skipped.
    let chunks: Vec<&str> = events
        .iter()
        .filter_map(|e| {
            if let AdapterEvent::ContentChunk { text, .. } = e {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(chunks, vec!["Done."], "expected only the final text chunk");

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
async fn failed_turn_fixture_yields_harness_error() {
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("failed-turn")).await;

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(terminals.len(), 1);
    assert!(
        matches!(
            terminals[0],
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Failed {
                    kind: FailureKind::HarnessError,
                    ..
                },
                ..
            }
        ),
        "expected TurnEnd(Failed(HarnessError))"
    );
}

#[tokio::test]
async fn truncated_stream_synthesizes_adapter_failure() {
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("truncated")).await;

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(
        terminals.len(),
        1,
        "adapter must synthesize exactly one TurnEnd"
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
        "expected TurnEnd(Failed(AdapterFailure)) for truncated stream"
    );
}

#[tokio::test]
async fn malformed_json_synthesizes_adapter_failure() {
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("malformed-json")).await;

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(terminals.len(), 1);
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
                "message should mention malformed JSON, got: {message:?}"
            );
        }
        other => panic!("expected TurnEnd(Failed(AdapterFailure)), got: {other:?}"),
    }
}

#[tokio::test]
async fn exit1_after_completed_does_not_re_emit() {
    // After observing TurnEnd(Completed), a non-zero subprocess exit must be
    // logged but NOT cause a second TurnEnd to be emitted. Consumers see exactly
    // one terminal event: the Completed one.
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("exit1-after-completed")).await;

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(
        terminals.len(),
        1,
        "must not re-emit a second TurnEnd after reconciliation"
    );
    assert!(matches!(
        terminals[0],
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Completed,
            ..
        }
    ));
}

#[tokio::test]
async fn binary_not_found_returns_dispatch_error() {
    let bad_adapter =
        ClaudeCodeAdapter::with_binary_path("/nonexistent/path/to/claude-does-not-exist");
    let agent = fake_agent();
    let turn_id = Uuid::now_v7();
    let result = bad_adapter
        .dispatch(&agent, Path::new("/tmp"), "hi", turn_id)
        .await;
    match result {
        Err(DispatchError::BinaryNotFound) => {}
        Err(other) => panic!("expected BinaryNotFound, got: {other}"),
        Ok(_) => panic!("expected Err(BinaryNotFound), got Ok"),
    }
}

#[tokio::test]
async fn stderr_drain_no_deadlock() {
    // fake_claude always writes to stderr. This test verifies the adapter
    // completes (no deadlock) when the subprocess produces stderr output.
    // Uses the text-only fixture so we know the stream completes cleanly.
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("text-only")).await;
    assert!(
        !events.is_empty(),
        "should have received events despite stderr output"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
    );
}
