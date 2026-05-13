use std::path::Path;

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_harness::{
    AdapterEvent, FailureKind, HarnessAdapter, MockHarnessAdapter, MockScenario, TurnId,
    TurnOutcome,
};
use uuid::Uuid;

fn fake_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "mock-test-agent".to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_id: Some(Uuid::now_v7()),
        created_at: chrono::Utc::now(),
    }
}

async fn drain(adapter: &MockHarnessAdapter, prompt: &str) -> Vec<AdapterEvent> {
    let turn_id: TurnId = Uuid::now_v7();
    let stream = adapter
        .dispatch(&fake_agent(), Path::new("/tmp"), prompt, turn_id)
        .await
        .expect("mock dispatch should not fail");
    stream.collect().await
}

#[tokio::test]
async fn streaming_scenario_yields_chunks_then_completed() {
    let adapter = MockHarnessAdapter::new();
    let events = drain(&adapter, "hello").await;

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

    assert_eq!(chunks.len(), 3, "Streaming scenario emits 3 ContentChunks");
    let joined = chunks.join("");
    assert!(
        joined.contains("Mock response to:"),
        "should contain the preamble"
    );
    assert!(joined.contains("hello"), "should echo the prompt");
    assert!(
        joined.contains("mock harness"),
        "should contain the mock harness tag"
    );

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
async fn mock_turn_ids_match_dispatch_argument() {
    let adapter = MockHarnessAdapter::new();
    let turn_id: TurnId = Uuid::now_v7();
    let stream = adapter
        .dispatch(&fake_agent(), Path::new("/tmp"), "test", turn_id)
        .await
        .unwrap();
    let events: Vec<AdapterEvent> = stream.collect().await;

    for event in &events {
        let event_turn_id = match event {
            AdapterEvent::ContentChunk { turn_id: tid, .. }
            | AdapterEvent::TurnEnd { turn_id: tid, .. } => *tid,
            _ => unreachable!("M1 AdapterEvent has only ContentChunk and TurnEnd variants"),
        };
        assert_eq!(
            event_turn_id, turn_id,
            "all events must carry the dispatcher-provided turn_id"
        );
    }
}

#[tokio::test]
async fn panic_scenario_ends_stream_without_turn_end() {
    // MockScenario::Panic intentionally violates the stream contract.
    // The stream ends after the first ContentChunk (task panics before TurnEnd).
    // This test validates the panic scenario works as designed — the M1.4
    // dispatcher test uses this to verify AgentIdleGuard restores Idle on panic.
    let adapter = MockHarnessAdapter::with_scenario(MockScenario::Panic);
    let events = drain(&adapter, "test").await;

    let has_turn_end = events
        .iter()
        .any(|e| matches!(e, AdapterEvent::TurnEnd { .. }));
    assert!(
        !has_turn_end,
        "Panic scenario must end without TurnEnd (intentional contract violation)"
    );

    let has_chunk = events
        .iter()
        .any(|e| matches!(e, AdapterEvent::ContentChunk { .. }));
    assert!(
        has_chunk,
        "Panic scenario should emit at least one ContentChunk before panicking"
    );
}

#[tokio::test]
async fn streaming_scenario_does_not_return_dispatch_error() {
    let adapter = MockHarnessAdapter::new();
    let turn_id: TurnId = Uuid::now_v7();
    let result = adapter
        .dispatch(&fake_agent(), Path::new("/tmp"), "test", turn_id)
        .await;
    assert!(result.is_ok(), "mock should never return a DispatchError");
}

#[tokio::test]
async fn completed_turn_outcome_wire_shape_roundtrips() {
    // Checks that TurnEnd(Completed) from the Streaming scenario serializes and
    // deserializes correctly via the NormalizedEvent lifting path (used by M1.4 dispatcher).
    use switchboard_harness::NormalizedEvent;

    let adapter = MockHarnessAdapter::new();
    let events = drain(&adapter, "test").await;
    let turn_end = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("must have TurnEnd");

    let normalized = NormalizedEvent::from(turn_end.clone());
    let json = serde_json::to_string(&normalized).unwrap();
    let parsed: NormalizedEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(normalized, parsed);
}

#[tokio::test]
async fn failed_turn_outcome_wire_shape_roundtrips() {
    // Checks that TurnEnd(Failed) serializes and deserializes correctly via the
    // NormalizedEvent lifting path. Exercises the From<AdapterEvent> lift for the
    // Failed variant, which the completed test above does not cover.
    use switchboard_harness::NormalizedEvent;
    use uuid::Uuid;

    let turn_id: TurnId = Uuid::now_v7();
    let event = AdapterEvent::TurnEnd {
        turn_id,
        outcome: TurnOutcome::Failed {
            kind: FailureKind::HarnessError,
            message: "bad model".to_owned(),
        },
        ended_at: chrono::Utc::now(),
    };

    let normalized = NormalizedEvent::from(event);
    let json = serde_json::to_string(&normalized).unwrap();
    let parsed: NormalizedEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(normalized, parsed);
}
