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
    AdapterEvent, ClaudeCodeAdapter, CodexAdapter, DispatchOptions, HarnessAdapter, TurnOutcome,
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
            DispatchOptions::default(),
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

    // Drift detection for promoted M2 events — symmetric with the Codex
    // live test's enrichment assertions. Claude emits `SessionMeta` from
    // its `system/init` stream event on every dispatch; the wire-format
    // contract says `model`, `harness_version`, and `tools` are populated.
    // `TurnEnd.usage.context_window` comes from `result.modelUsage.<model>.contextWindow`.
    // If Anthropic's CLI ever silently drops or renames these fields, this
    // test catches it before it ships to users (fixture-based tests would
    // keep passing against the old recorded shape).
    let session_meta = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::SessionMeta { .. }))
        .expect("Claude must emit SessionMeta from system/init on every dispatch");
    match session_meta {
        AdapterEvent::SessionMeta {
            model,
            harness_version,
            tools,
            ..
        } => {
            assert!(!model.is_empty(), "SessionMeta.model must be non-empty");
            assert!(
                !harness_version.is_empty(),
                "SessionMeta.harness_version must be non-empty"
            );
            assert!(
                !tools.is_empty(),
                "SessionMeta.tools must list at least Claude's builtin tools"
            );
        }
        _ => unreachable!(),
    }
    match terminal {
        AdapterEvent::TurnEnd { usage: Some(u), .. } => {
            assert!(
                u.context_window.is_some(),
                "TurnEnd.usage.context_window must be populated from result.modelUsage (got None)"
            );
        }
        _ => panic!("expected TurnEnd with Some(usage), got: {terminal:?}"),
    }
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
        .dispatch(
            &agent1,
            Path::new("/tmp"),
            "Say ACK",
            turn1,
            DispatchOptions::default(),
        )
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
        .dispatch(
            &agent2,
            Path::new("/tmp"),
            "Say ACK again",
            turn2,
            DispatchOptions::default(),
        )
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
            DispatchOptions::default(),
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

    let terminal_idx = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("should have a terminal TurnEnd");
    let terminal = &events[terminal_idx];
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

    // M2.4 post-terminal enrichment must fire for Codex turns:
    // - TurnEnd.usage.context_window is enriched from the session file's
    //   task_started.model_context_window (Codex's stream doesn't carry it).
    // - RateLimitEvent fires every turn from token_count.rate_limits.
    // - SessionMeta fires on the first turn carrying model + cli_version +
    //   the merged MCP servers / skills registries.
    match terminal {
        AdapterEvent::TurnEnd { usage: Some(u), .. } => {
            assert!(
                u.context_window.is_some(),
                "M2.4: TurnEnd.usage.context_window must be enriched from session file (got None)"
            );
        }
        _ => panic!("expected TurnEnd with Some(usage), got: {terminal:?}"),
    }
    let rate_limit_idx = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::RateLimitEvent { .. }))
        .expect("M2.4: RateLimitEvent must fire post-terminal for Codex");
    let session_meta_idx = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::SessionMeta { .. }))
        .expect("M2.4: SessionMeta must fire on first turn for Codex");
    assert!(
        terminal_idx < rate_limit_idx && rate_limit_idx < session_meta_idx,
        "enrichment events must arrive after TurnEnd in order: TurnEnd → RateLimitEvent → SessionMeta"
    );

    // SessionMeta shape: structural-only checks. mcp_servers / skills lists
    // are developer-environment-dependent (we don't pin a particular ~/.codex
    // setup), so just assert the model + harness_version are non-empty and
    // tools is the documented vec![].
    match &events[session_meta_idx] {
        AdapterEvent::SessionMeta {
            model,
            harness_version,
            tools,
            ..
        } => {
            assert!(!model.is_empty(), "model must be set from turn_context");
            assert!(
                !harness_version.is_empty(),
                "harness_version must be set from session_meta.cli_version"
            );
            assert!(tools.is_empty(), "tools is vec![] for Codex");
        }
        _ => unreachable!(),
    }

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
    assert!(content.contains("session_partition_date"));
}

#[tokio::test]
#[ignore = "requires codex installed — run with: make test-live"]
async fn live_codex_resume_reuses_session() {
    // Memorize-then-recall: definitive proof that resume restores prior
    // turn's context. Token-count growth would also signal "system prompts
    // and tool registry are being resent" — a weaker test. The recall
    // pattern fails iff Codex genuinely loses the conversation state.
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = CodexAdapter::new();
    let agent = live_codex_agent();

    // Turn 1: ask Codex to remember a specific word.
    let turn1 = Uuid::now_v7();
    let stream1 = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "Remember the word 'mango'. Reply with only 'ok'.",
            turn1,
            DispatchOptions::default(),
        )
        .await
        .expect("first dispatch should succeed");
    let _events1: Vec<AdapterEvent> = stream1.collect().await;

    // Turn 2 (resume): ask Codex to recall the word.
    let turn2 = Uuid::now_v7();
    let stream2 = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "What word did I ask you to remember? Reply with only that word.",
            turn2,
            DispatchOptions::default(),
        )
        .await
        .expect("resume dispatch should succeed");
    let events2: Vec<AdapterEvent> = stream2.collect().await;
    let recall_text: String = events2
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        recall_text.to_lowercase().contains("mango"),
        "resume must restore the prior turn's context: turn2 reply was {recall_text:?}"
    );

    // Sidecar should have two records (one per dispatch), same session_id.
    // Codex echoes the same thread_id on resume.
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
        "real Codex echoes the same thread_id on resume"
    );
    assert_eq!(
        r1["session_partition_date"], r2["session_partition_date"],
        "resume preserves session_partition_date"
    );
}
