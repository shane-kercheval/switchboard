/// Live integration tests gated behind `#[ignore]`.
///
/// Run with: `make test-live`
///
/// Requires `claude` installed and authenticated. Not part of CI for M1 — M2
/// sets up the integration CI workflow.
use std::path::Path;

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_harness::{AdapterEvent, ClaudeCodeAdapter, HarnessAdapter, TurnOutcome};
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

#[tokio::test]
#[ignore = "pre-M2 probe — requires claude installed — run with: make test-live"]
async fn live_resume_on_fresh_session_id_accepted() {
    // Probes whether `--resume <uuid>` succeeds when no session file exists yet.
    // If this test passes, --resume is safe to use unconditionally (first turn
    // included) and the session_file_exists FS check in ClaudeCodeAdapter can be
    // eliminated entirely. Run once before M2 to resolve the edge case documented
    // in the M1.3 section of the milestone plan.
    //
    // We bypass ClaudeCodeAdapter here and call claude directly so we can force
    // --resume regardless of what the filesystem check would decide.
    let session_id = Uuid::now_v7();

    let output = tokio::process::Command::new("claude")
        .args([
            "-p",
            "Reply with only the word PROBE",
            "--resume",
            &session_id.to_string(),
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--dangerously-skip-permissions",
        ])
        .current_dir("/tmp")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .expect("claude should be runnable");

    assert!(
        output.status.success(),
        "--resume on a fresh session_id failed (exit {}): {}\nIf this consistently fails, \
         the session_file_exists FS check is necessary and cannot be removed.",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}
