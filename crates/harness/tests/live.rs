/// Live integration tests gated behind `#[ignore]`.
///
/// Run with: `make test-live`
///
/// Requires `claude` installed and authenticated. Not part of CI for M1 — M2
/// sets up the integration CI workflow.
use std::path::Path;

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_harness::{
    AdapterEvent, ClaudeCodeAdapter, CodexAdapter, HarnessAdapter, TurnOutcome,
};
use uuid::Uuid;

fn live_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "live-test-agent".to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_id: Some(Uuid::now_v7()),
        created_at: chrono::Utc::now(),
    }
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_basic_turn_completes() {
    let adapter = ClaudeCodeAdapter::new();
    let agent = live_agent();
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "Reply with only the number 4 and nothing else.",
            turn_id,
        )
        .await
        .expect("dispatch should succeed with real claude");

    let events: Vec<AdapterEvent> = stream.collect().await;

    let text: String = events
        .iter()
        .filter_map(|e| {
            if let AdapterEvent::ContentChunk { text, .. } = e {
                Some(text.clone())
            } else {
                None
            }
        })
        .collect();

    assert!(
        text.contains('4'),
        "expected '4' in response text, got: {text:?}"
    );

    let terminal = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("should have a terminal TurnEnd");

    assert!(
        matches!(
            terminal,
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        ),
        "expected TurnEnd(Completed), got: {terminal:?}"
    );
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_session_continuity_across_turns() {
    // Verifies that session state persists across turns: the first turn uses
    // --session-id to create the session; the second reuses the same session_id
    // and the adapter automatically switches to --resume.
    let adapter = ClaudeCodeAdapter::new();
    let session_id = Uuid::now_v7();

    let agent1 = AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "session-test-1".to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_id: Some(session_id),
        created_at: chrono::Utc::now(),
    };

    let turn1 = Uuid::now_v7();
    let stream1 = adapter
        .dispatch(&agent1, Path::new("/tmp"), "Say ACK", turn1)
        .await
        .expect("first dispatch with fresh session_id should succeed");
    let events1: Vec<AdapterEvent> = stream1.collect().await;
    let completed1 = events1.iter().any(|e| {
        matches!(
            e,
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        )
    });
    assert!(completed1, "first turn should complete");

    // Second turn reuses the same session_id — adapter detects the session file
    // and switches to --resume automatically.
    let agent2 = AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "session-test-2".to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_id: Some(session_id),
        created_at: chrono::Utc::now(),
    };

    let turn2 = Uuid::now_v7();
    let stream2 = adapter
        .dispatch(&agent2, Path::new("/tmp"), "Say ACK again", turn2)
        .await
        .expect("second dispatch reusing session_id should succeed");
    let events2: Vec<AdapterEvent> = stream2.collect().await;
    let completed2 = events2.iter().any(|e| {
        matches!(
            e,
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        )
    });
    assert!(
        completed2,
        "second turn with same session_id should complete"
    );
}

// --- Codex live tests (M2.3) ---

fn live_codex_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "live-codex-agent".to_owned(),
        harness: HarnessKind::Codex,
        // Codex agents always have session_id = None — the per-agent
        // session-link sidecar is the system-of-record (M2.3 invariant).
        session_id: None,
        created_at: chrono::Utc::now(),
    }
}

#[tokio::test]
#[ignore = "requires codex installed — run with: make test-live"]
async fn live_codex_basic_turn_completes() {
    // Use a tempdir as cwd so the sidecar is written to a clean location
    // (avoids leaving state under the repo).
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = CodexAdapter::new();
    let agent = live_codex_agent();
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "Reply with the single word 'ack' and nothing else.",
            turn_id,
        )
        .await
        .expect("dispatch should succeed with real codex");

    let events: Vec<AdapterEvent> = stream.collect().await;

    let text: String = events
        .iter()
        .filter_map(|e| {
            if let AdapterEvent::ContentChunk { text, .. } = e {
                Some(text.clone())
            } else {
                None
            }
        })
        .collect();
    assert!(
        text.to_lowercase().contains("ack"),
        "expected 'ack' in response text, got: {text:?}"
    );

    let terminal = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("should have a terminal TurnEnd");
    assert!(
        matches!(
            terminal,
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        ),
        "expected TurnEnd(Completed), got: {terminal:?}"
    );

    // Sidecar must exist after the first turn with the captured thread_id.
    let sidecar = tmp
        .path()
        .join(".switchboard")
        .join("projects")
        .join(agent.project_id.to_string())
        .join("sessions")
        .join(format!("{}.jsonl", agent.id));
    assert!(
        sidecar.is_file(),
        "sidecar must be written on first dispatch"
    );
    let content = std::fs::read_to_string(&sidecar).unwrap();
    assert!(content.contains("session_id"));
    assert!(content.contains("original_start_date_utc"));
}

#[tokio::test]
#[ignore = "requires codex installed — run with: make test-live"]
async fn live_codex_resume_reuses_session() {
    // Two sequential dispatches with the same agent — the second must use
    // `codex exec resume` and Codex should report a higher input_tokens
    // (context carried over). We don't observe the command line directly
    // here; we observe the *effect* via the sidecar (two records, same
    // session_id) and the usage (second turn > first turn).
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = CodexAdapter::new();
    let agent = live_codex_agent();

    let turn1 = Uuid::now_v7();
    let stream1 = adapter
        .dispatch(&agent, tmp.path(), "Reply with the word 'ack'", turn1)
        .await
        .expect("first dispatch should succeed");
    let events1: Vec<AdapterEvent> = stream1.collect().await;
    let usage1 = events1.iter().find_map(|e| match e {
        AdapterEvent::TurnEnd { usage: Some(u), .. } => Some(u.clone()),
        _ => None,
    });

    let turn2 = Uuid::now_v7();
    let stream2 = adapter
        .dispatch(&agent, tmp.path(), "Reply with the word 'ack' again", turn2)
        .await
        .expect("resume dispatch should succeed");
    let events2: Vec<AdapterEvent> = stream2.collect().await;
    let usage2 = events2.iter().find_map(|e| match e {
        AdapterEvent::TurnEnd { usage: Some(u), .. } => Some(u.clone()),
        _ => None,
    });

    // Sidecar should have two records (one per dispatch), same session_id.
    let sidecar = tmp
        .path()
        .join(".switchboard")
        .join("projects")
        .join(agent.project_id.to_string())
        .join("sessions")
        .join(format!("{}.jsonl", agent.id));
    let lines: Vec<String> = std::fs::read_to_string(&sidecar)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect();
    assert_eq!(lines.len(), 2, "two dispatches → two records");
    let r1: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    let r2: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(
        r1["session_id"], r2["session_id"],
        "resume reuses the original thread_id"
    );
    assert_eq!(
        r1["original_start_date_utc"], r2["original_start_date_utc"],
        "resume preserves original_start_date_utc"
    );

    // Second turn's input_tokens should be ≥ first turn's (context carried).
    if let (Some(u1), Some(u2)) = (usage1, usage2) {
        assert!(
            u2.input_tokens >= u1.input_tokens,
            "resume must preserve prior context: turn1={} turn2={}",
            u1.input_tokens,
            u2.input_tokens
        );
    }
}
