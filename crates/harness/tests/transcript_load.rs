//! Live transcript-hydration round-trip per harness.
//!
//! Dispatches a small live turn, then re-reads the recorded turn through the
//! same `load_*_transcript` function the app calls during project open. The
//! reconstructed `Turn::User` and `Turn::Agent` must match what the live
//! stream emitted — same user prompt, agent reply text, terminal status,
//! and no parser warnings.
//!
//! The Codex test exercises the **sidecar-driven** lookup path (matches
//! `commands::load_transcript_impl`'s production path). The Codex
//! attach-existing-session locator (`find_codex_session_file_for_attach`)
//! has its own M2.4-era coverage and is not duplicated here.
//!
//! Run with: `make test-live`.

use std::path::PathBuf;

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_harness::{
    AdapterEvent, ClaudeCodeAdapter, CodexAdapter, DispatchOptions, HarnessAdapter, Turn,
    TurnStatus, codex::sidecar,
};
use uuid::Uuid;

fn real_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME must be set for live tests")
}

fn collect_text(events: &[AdapterEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_transcript_load_round_trips() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let adapter = ClaudeCodeAdapter::new();
    let agent = AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "transcript-claude".to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_id: Some(Uuid::now_v7()),
        created_at: chrono::Utc::now(),
    };
    let prompt = "Reply with only the single word 'ack' and nothing else.";
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            prompt,
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");
    let live_events: Vec<AdapterEvent> = stream.collect().await;
    let live_text = collect_text(&live_events);

    let transcript = switchboard_harness::load_claude_transcript(
        &real_home(),
        tmp.path(),
        agent.session_id.unwrap(),
        agent.id,
    )
    .expect("load_claude_transcript must succeed");

    assert!(
        transcript.warnings.is_empty(),
        "expected no parser warnings; got: {:?}",
        transcript.warnings
    );
    assert!(
        transcript.meta.is_some(),
        "meta must be present after a live turn"
    );
    let model = transcript.meta.as_ref().map(|m| m.model.as_str()).unwrap();
    assert!(!model.is_empty(), "meta.model must be populated");

    let (user, agent_turn) = first_user_and_agent(&transcript.turns);
    assert_user(user, &agent.id, prompt);
    assert_agent_completed(agent_turn, &agent.id, &live_text);
}

#[tokio::test]
#[ignore = "requires codex installed — run with: make test-live"]
async fn live_codex_transcript_load_via_sidecar_round_trips() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let adapter = CodexAdapter::new();
    let agent = AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "transcript-codex".to_owned(),
        harness: HarnessKind::Codex,
        session_id: None,
        created_at: chrono::Utc::now(),
    };
    let prompt = "Reply with only the single word 'ack' and nothing else.";
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            prompt,
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real codex");
    let live_events: Vec<AdapterEvent> = stream.collect().await;
    let live_text = collect_text(&live_events);

    let sidecar = sidecar::sidecar_path(tmp.path(), agent.project_id, agent.id);
    let record = sidecar::read_latest(&sidecar)
        .expect("sidecar must read cleanly")
        .expect("sidecar must contain a record after a live dispatch");

    let transcript = switchboard_harness::load_codex_transcript(
        &real_home(),
        tmp.path(),
        &record.session_id,
        Some(record.session_partition_date),
        agent.id,
    )
    .expect("load_codex_transcript must succeed");

    assert!(
        transcript.warnings.is_empty(),
        "expected no parser warnings; got: {:?}",
        transcript.warnings
    );
    assert!(
        transcript.meta.is_some(),
        "meta must be present after a live turn"
    );
    let model = transcript.meta.as_ref().map(|m| m.model.as_str()).unwrap();
    assert!(!model.is_empty(), "meta.model must be populated");

    let (user, agent_turn) = first_user_and_agent(&transcript.turns);
    assert_user(user, &agent.id, prompt);
    assert_agent_completed(agent_turn, &agent.id, &live_text);
}

fn first_user_and_agent(turns: &[Turn]) -> (&Turn, &Turn) {
    let user = turns
        .iter()
        .find(|t| matches!(t, Turn::User { .. }))
        .expect("hydrated transcript must contain a Turn::User");
    let agent = turns
        .iter()
        .find(|t| matches!(t, Turn::Agent { .. }))
        .expect("hydrated transcript must contain a Turn::Agent");
    (user, agent)
}

fn assert_user(turn: &Turn, expected_agent_id: &Uuid, expected_text: &str) {
    let Turn::User { agent_id, text, .. } = turn else {
        panic!("expected Turn::User; got: {turn:?}");
    };
    assert_eq!(agent_id, expected_agent_id, "user.agent_id must match");
    assert_eq!(text, expected_text, "hydrated user.text must match prompt");
}

fn assert_agent_completed(turn: &Turn, expected_agent_id: &Uuid, expected_live_text: &str) {
    let Turn::Agent {
        agent_id,
        status,
        items,
        ..
    } = turn
    else {
        panic!("expected Turn::Agent; got: {turn:?}");
    };
    assert_eq!(agent_id, expected_agent_id, "agent.agent_id must match");
    assert_eq!(*status, TurnStatus::Complete, "agent turn must be Complete");

    // Walk text items and confirm the live reply appears in the hydrated
    // stream. Substring (not equality) because the live stream concatenates
    // chunk-by-chunk and the hydrated form may be assembled from one or
    // more text records.
    let hydrated_text: String = items
        .iter()
        .filter_map(|item| match item {
            switchboard_harness::TurnItem::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let live_trimmed = expected_live_text.trim();
    assert!(
        !live_trimmed.is_empty(),
        "live stream produced no text — test cannot validate"
    );
    assert!(
        hydrated_text.contains(live_trimmed),
        "hydrated agent text must contain live stream text;\nlive: {live_trimmed:?}\nhydrated: {hydrated_text:?}"
    );
}
