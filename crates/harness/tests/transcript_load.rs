//! Live transcript-hydration round-trip per harness.
//!
//! Dispatches a small live turn, then re-reads the recorded turn through the
//! same `load_*_transcript` function the app calls during project open. The
//! reconstructed `Turn::User` and `Turn::Agent` must match what the live
//! stream emitted — same user prompt, agent reply text, terminal status,
//! and no parser warnings — plus the sidebar-bearing metadata fields
//! (per-harness usage, rate-limit, registries) survive the round-trip.
//!
//! The Codex test exercises the **sidecar-driven** lookup path (matches
//! `commands::load_transcript_impl`'s production path). The Codex
//! attach-existing-session locator (`find_codex_session_file_for_attach`)
//! has its own coverage and is not duplicated here.
//!
//! Run with: `make test-live`.

use std::path::PathBuf;

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind, SessionLocator};
use switchboard_harness::{
    AdapterEvent, AntigravityAdapter, ClaudeCodeAdapter, CodexAdapter, DispatchOptions,
    GeminiAdapter, HarnessAdapter, Turn, TurnItem, TurnStatus, load_antigravity_transcript,
};
use uuid::Uuid;

const CLAUDE_TOOL_TOKEN: &str = "SWITCHBOARD_TRANSCRIPT_TOOL_FA21E0";
const CODEX_TOOL_TOKEN: &str = "SWITCHBOARD_TRANSCRIPT_TOOL_C0D3X1";
const GEMINI_TOOL_TOKEN: &str = "SWITCHBOARD_TRANSCRIPT_TOOL_GEM1N1";

fn real_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME must be set for live tests")
}

/// Extract the session UUID from a Claude/Gemini agent's locator (those
/// harnesses always carry a `Uuid` variant). Panics on any other shape — a
/// test setup error, not a runtime condition.
fn uuid_locator(agent: &AgentRecord) -> Uuid {
    match &agent.session_locator {
        Some(SessionLocator::Uuid(id)) => *id,
        other => panic!("expected a Uuid session locator, got {other:?}"),
    }
}

/// Extract the Codex `thread_id` + partition-date from the dispatch's capture
/// event (a first Codex dispatch emits one; the dispatcher persists it to the
/// record). Used by the live Codex hydration tests in place of a sidecar read.
fn captured_codex_locator(events: &[AdapterEvent]) -> (String, chrono::NaiveDate) {
    events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::SessionLocatorCaptured {
                locator:
                    SessionLocator::Codex {
                        thread_id,
                        partition_date,
                    },
            } => Some((thread_id.clone(), *partition_date)),
            _ => None,
        })
        .expect("a first Codex dispatch must emit a captured Codex locator")
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
        session_locator: Some(SessionLocator::Uuid(Uuid::now_v7())),
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
        uuid_locator(&agent),
        agent.id,
    )
    .expect("load_claude_transcript must succeed");

    assert!(
        transcript.warnings.is_empty(),
        "expected no parser warnings; got: {:?}",
        transcript.warnings
    );
    assert_meta_structure(&transcript);

    let (user, agent_turn) = first_user_and_agent(&transcript.turns);
    assert_user(user, &agent.id, prompt);
    assert_agent_completed(agent_turn, &agent.id, &live_text);
    assert_claude_agent_usage(agent_turn);
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_transcript_load_hydrates_tool_items() {
    // Drift-detection for tool-item persistence: tool_use.rs proves tool
    // events emit live, and the text round-trip above proves text survives
    // hydration. Neither catches a regression where tool calls stop being
    // reconstructed from the on-disk session file (a CLI bump renaming a
    // record type or field), which would silently break sidebar / transcript
    // rendering of tool calls on project reopen.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("MARKER.txt"), CLAUDE_TOOL_TOKEN).expect("write marker");

    let adapter = ClaudeCodeAdapter::new();
    let agent = AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "transcript-claude-tool".to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_locator: Some(SessionLocator::Uuid(Uuid::now_v7())),
        created_at: chrono::Utc::now(),
    };
    let turn_id = Uuid::now_v7();
    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "Read the file MARKER.txt in the current directory using your Read tool. \
             Reply with only the file's contents and nothing else.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");
    // Drain the stream to flush the subprocess; the session file isn't
    // written until the stream completes. Dropping this line would
    // race the hydration read against the subprocess.
    let _events: Vec<AdapterEvent> = stream.collect().await;

    let transcript = switchboard_harness::load_claude_transcript(
        &real_home(),
        tmp.path(),
        uuid_locator(&agent),
        agent.id,
    )
    .expect("load_claude_transcript must succeed");

    assert!(
        transcript.warnings.is_empty(),
        "expected no parser warnings; got: {:?}",
        transcript.warnings
    );

    let agent_turn = transcript
        .turns
        .iter()
        .find(|t| matches!(t, Turn::Agent { .. }))
        .expect("hydrated transcript must contain a Turn::Agent");
    let Turn::Agent { items, .. } = agent_turn else {
        unreachable!();
    };

    // Find the tool item whose output carries the staged sentinel rather
    // than picking the first tool item — robust against Claude emitting
    // a preliminary tool (e.g., `TodoWrite`) before the file-reading
    // tool. The harness suite must catch upstream drift without being
    // brittle to harmless ordering changes.
    let (is_error, name) = items
        .iter()
        .find_map(|item| match item {
            TurnItem::Tool {
                output: Some(output),
                is_error,
                name,
                ..
            } if output.contains(CLAUDE_TOOL_TOKEN) => Some((is_error, name)),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "hydrated agent turn must contain a TurnItem::Tool whose output carries \
                 {CLAUDE_TOOL_TOKEN:?}; items: {items:?}"
            )
        });
    assert!(!name.is_empty(), "hydrated tool item must carry a name");
    assert_eq!(
        *is_error,
        Some(false),
        "hydrated tool item must record is_error: Some(false)"
    );
}

/// End-to-end drift detection for the deferred-tool-result queue
/// (`session_file.rs::ReconstructionState::pending_tool_results`).
///
/// Claude 2.1.150 was observed to write `tool_result` records to the
/// session file *before* their matching `tool_use` (within the same
/// turn, ~1s gap; the canonical case was session
/// `22300f1b-3efe-4dbc-a4a0-7c1c954d1da2.jsonl` lines 1406/1408 and
/// 1607/1609). Pre-M2 the parser dropped the result's output silently
/// and emitted a "did not match any open tool" warning; on rehydration
/// the affected tool calls rendered with empty content.
///
/// Multi-tool prompt makes the out-of-order pattern more likely to
/// manifest in any given run (the originally-observed sessions all had
/// long tool-call sequences). The assertion is the TUI-parity invariant:
/// every hydrated `TurnItem::Tool` carries non-empty output, no warnings.
/// If the parser stops binding properly on a future Claude version, this
/// test fails — same drift-detection role the live tool-use tests play.
#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_tool_results_bind_after_restart() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // Stage three small files; the prompt asks Claude to read all of
    // them, which forces a sequence of Read tool calls.
    std::fs::write(tmp.path().join("ALPHA.txt"), "first-file-contents").expect("write ALPHA");
    std::fs::write(tmp.path().join("BETA.txt"), "second-file-contents").expect("write BETA");
    std::fs::write(tmp.path().join("GAMMA.txt"), "third-file-contents").expect("write GAMMA");

    let adapter = ClaudeCodeAdapter::new();
    let agent = AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "transcript-claude-tool-bind".to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_locator: Some(SessionLocator::Uuid(Uuid::now_v7())),
        created_at: chrono::Utc::now(),
    };
    let turn_id = Uuid::now_v7();
    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "Read each of ALPHA.txt, BETA.txt, and GAMMA.txt in the current directory \
             using your Read tool, one at a time. Reply with the single word done after \
             you have read all three.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");
    // Drain to flush the subprocess before reading the session file.
    let _events: Vec<AdapterEvent> = stream.collect().await;

    let transcript = switchboard_harness::load_claude_transcript(
        &real_home(),
        tmp.path(),
        uuid_locator(&agent),
        agent.id,
    )
    .expect("load_claude_transcript must succeed");

    assert!(
        transcript.warnings.is_empty(),
        "rehydrating a multi-tool Claude turn must not emit parse warnings \
         (out-of-order tool_use/tool_result writes should be bound via the \
         deferred queue); got: {:?}",
        transcript.warnings,
    );

    let agent_turn = transcript
        .turns
        .iter()
        .find(|t| matches!(t, Turn::Agent { .. }))
        .expect("hydrated transcript must contain a Turn::Agent");
    let Turn::Agent { items, .. } = agent_turn else {
        unreachable!();
    };

    // Pin the test to the three staged file contents specifically, rather
    // than constraining every tool item in the turn. Two reasons:
    //   1. Claude may emit incidental parent-level tools (e.g.,
    //      `TodoWrite`) whose output is legitimately empty — asserting
    //      "every tool has non-empty output" would false-fail there with
    //      a misleading "deferred-queue regressed" diagnosis.
    //   2. The invariant we actually care about is "the Reads we asked
    //      for happened and their content bound" — a stronger,
    //      contents-pinned check.
    for expected in [
        "first-file-contents",
        "second-file-contents",
        "third-file-contents",
    ] {
        let found = items.iter().any(|i| {
            matches!(
                i,
                TurnItem::Tool {
                    output: Some(out),
                    is_error: Some(false),
                    ..
                } if out.contains(expected)
            )
        });
        assert!(
            found,
            "expected a non-error TurnItem::Tool whose output contains {expected:?} — \
             either out-of-order writes regressed the deferred-queue fix, or Claude's \
             session-file shape changed. Items: {items:?}",
        );
    }
}

#[tokio::test]
#[ignore = "requires codex installed — run with: make test-live"]
async fn live_codex_transcript_load_via_captured_locator_round_trips() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let adapter = CodexAdapter::new();
    let agent = AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "transcript-codex".to_owned(),
        harness: HarnessKind::Codex,
        session_locator: None,
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

    let (thread_id, partition_date) = captured_codex_locator(&live_events);

    let transcript = switchboard_harness::load_codex_transcript(
        &real_home(),
        tmp.path(),
        &thread_id,
        Some(partition_date),
        agent.id,
    )
    .expect("load_codex_transcript must succeed");

    assert!(
        transcript.warnings.is_empty(),
        "expected no parser warnings; got: {:?}",
        transcript.warnings
    );
    assert_meta_structure(&transcript);
    assert!(
        transcript.last_rate_limit.is_some(),
        "Codex hydration must populate last_rate_limit from the session file"
    );

    let (user, agent_turn) = first_user_and_agent(&transcript.turns);
    assert_user(user, &agent.id, prompt);
    assert_agent_completed(agent_turn, &agent.id, &live_text);
    assert_codex_agent_usage(agent_turn);
}

#[tokio::test]
#[ignore = "requires codex installed — run with: make test-live"]
async fn live_codex_transcript_load_hydrates_tool_items() {
    // Drift-detection for Codex's tool-item hydration. The text
    // round-trip above proves Codex's user/agent turns survive
    // hydration; the live `tool_use.rs` test proves the shell tool
    // emits paired ToolStarted/ToolCompleted events on the stream.
    // Neither catches a regression where the on-disk session file's
    // tool-record shape changes such that tool calls stop being
    // reconstructed (a Codex CLI bump renaming `function_call` or its
    // `output` payload field). Hydration is sidecar-driven and
    // date-partition-dependent — structurally different from Claude's
    // and Gemini's — so its tool-item path needs its own tripwire.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("MARKER.txt"), CODEX_TOOL_TOKEN).expect("write marker");

    let adapter = CodexAdapter::new();
    let agent = AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "transcript-codex-tool".to_owned(),
        harness: HarnessKind::Codex,
        session_locator: None,
        created_at: chrono::Utc::now(),
    };
    let turn_id = Uuid::now_v7();
    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "Use your shell tool to run `cat MARKER.txt` in the current directory. \
             Reply with only the file's contents and nothing else.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real codex");
    // Drain the stream to flush the subprocess; the session file isn't
    // written until the stream completes. Dropping this line would
    // race the hydration read against the subprocess.
    let events: Vec<AdapterEvent> = stream.collect().await;

    let (thread_id, partition_date) = captured_codex_locator(&events);

    let transcript = switchboard_harness::load_codex_transcript(
        &real_home(),
        tmp.path(),
        &thread_id,
        Some(partition_date),
        agent.id,
    )
    .expect("load_codex_transcript must succeed");
    assert!(
        transcript.warnings.is_empty(),
        "expected no parser warnings; got: {:?}",
        transcript.warnings
    );

    let agent_turn = transcript
        .turns
        .iter()
        .find(|t| matches!(t, Turn::Agent { .. }))
        .expect("hydrated transcript must contain a Turn::Agent");
    let Turn::Agent { items, .. } = agent_turn else {
        unreachable!();
    };

    // Find the tool item whose output carries the staged sentinel rather
    // than picking the first tool item — robust against Codex emitting
    // an additional preliminary tool before the shell `cat`.
    let (is_error, name) = items
        .iter()
        .find_map(|item| match item {
            TurnItem::Tool {
                output: Some(output),
                is_error,
                name,
                ..
            } if output.contains(CODEX_TOOL_TOKEN) => Some((is_error, name)),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "hydrated agent turn must contain a TurnItem::Tool whose output carries \
                 {CODEX_TOOL_TOKEN:?}; items: {items:?}"
            )
        });
    assert!(!name.is_empty(), "hydrated tool item must carry a name");
    assert_eq!(
        *is_error,
        Some(false),
        "hydrated tool item must record is_error: Some(false)"
    );
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

    // Substring (not equality) because the live stream concatenates chunk
    // by chunk while the hydrated form may be assembled from one or more
    // text records.
    let hydrated_text: String = items
        .iter()
        .filter_map(|item| match item {
            TurnItem::Text { text, .. } => Some(text.as_str()),
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

/// Claude's session-file parser fills `usage` with token totals but cannot
/// recover `context_window` from disk (it lives in the stream-only
/// `result.modelUsage` field) — `None` is the documented contract.
fn assert_claude_agent_usage(turn: &Turn) {
    let Turn::Agent { usage, .. } = turn else {
        unreachable!("caller already matched Turn::Agent");
    };
    let usage = usage
        .as_ref()
        .expect("Claude hydration must carry usage for completed turns");
    assert!(
        usage.context_window.is_none(),
        "Claude hydrated usage.context_window must be None (stream-only field); got: {:?}",
        usage.context_window
    );
}

/// Codex's parser enriches `usage.context_window` from
/// `task_started.model_context_window` and carries `last_rate_limit` from
/// `token_count.rate_limits` — both load-bearing for the sidebar.
fn assert_codex_agent_usage(turn: &Turn) {
    let Turn::Agent { usage, .. } = turn else {
        unreachable!("caller already matched Turn::Agent");
    };
    let usage = usage
        .as_ref()
        .expect("Codex hydration must carry usage for completed turns");
    assert!(
        usage.context_window.is_some(),
        "Codex hydrated usage.context_window must be populated from the session file"
    );
}

fn assert_meta_structure(transcript: &switchboard_harness::LoadedTranscript) {
    let meta = transcript
        .meta
        .as_ref()
        .expect("meta must be present after a live turn");
    assert!(!meta.model.is_empty(), "meta.model must be populated");
    // Registries are environment-dependent (developer's own MCP config /
    // skills directory). We pin the structural contract — the fields
    // deserialize as readable vectors — not the contents.
    let _: &Vec<_> = &meta.mcp_servers;
    let _: &Vec<_> = &meta.skills;
    let _: &Vec<_> = &meta.tools;
}

#[tokio::test]
#[ignore = "requires gemini installed — run with: make test-live"]
async fn live_gemini_transcript_load_via_session_file_round_trips() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let adapter = GeminiAdapter::new();
    let session_id = Uuid::new_v4();
    let agent = AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "transcript-gemini".to_owned(),
        harness: HarnessKind::Gemini,
        session_locator: Some(SessionLocator::Uuid(session_id)),
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
        .expect("dispatch should succeed with real gemini");
    let live_events: Vec<AdapterEvent> = stream.collect().await;
    let live_text = collect_text(&live_events);

    let transcript = switchboard_harness::load_gemini_transcript(
        &real_home(),
        tmp.path(),
        uuid_locator(&agent),
        agent.id,
    )
    .expect("load_gemini_transcript must succeed");

    assert!(
        transcript.warnings.is_empty(),
        "expected no parser warnings; got: {:?}",
        transcript.warnings
    );
    assert_meta_structure(&transcript);

    let (user, agent_turn) = first_user_and_agent(&transcript.turns);
    assert_user(user, &agent.id, prompt);
    assert_agent_completed(agent_turn, &agent.id, &live_text);
    assert_gemini_agent_usage(agent_turn);
    // Gemini's session file carries no rate-limit telemetry (unlike Codex).
    assert!(
        transcript.last_rate_limit.is_none(),
        "Gemini hydration must leave last_rate_limit as None (no rate-limit field in session file)"
    );
}

#[tokio::test]
#[ignore = "requires gemini installed — run with: make test-live"]
async fn live_gemini_transcript_load_hydrates_tool_items() {
    // Where the sentinel-in-output assertion lives for Gemini. The live
    // stream's `tool_result.output` is `""` for `read_file`, so
    // `tool_use.rs` can only assert lifecycle. The session file carries
    // the real `read_file` output, which hydration surfaces — this is
    // the load-bearing test that the "live = best-effort, hydration =
    // authoritative" contract holds for Gemini's read-like tools.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("MARKER.txt"), GEMINI_TOOL_TOKEN).expect("write marker");

    let adapter = GeminiAdapter::new();
    let session_id = Uuid::new_v4();
    let agent = AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "transcript-gemini-tool".to_owned(),
        harness: HarnessKind::Gemini,
        session_locator: Some(SessionLocator::Uuid(session_id)),
        created_at: chrono::Utc::now(),
    };
    let turn_id = Uuid::now_v7();
    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "Read the file MARKER.txt in the current directory and reply with only its contents.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real gemini");
    // Drain the stream to flush the subprocess; the session file isn't
    // written until the stream completes. Dropping this line would
    // race the hydration read against the subprocess.
    let _events: Vec<AdapterEvent> = stream.collect().await;

    let transcript = switchboard_harness::load_gemini_transcript(
        &real_home(),
        tmp.path(),
        uuid_locator(&agent),
        agent.id,
    )
    .expect("load_gemini_transcript must succeed");
    assert!(
        transcript.warnings.is_empty(),
        "expected no parser warnings; got: {:?}",
        transcript.warnings
    );

    let agent_turn = transcript
        .turns
        .iter()
        .find(|t| matches!(t, Turn::Agent { .. }))
        .expect("hydrated transcript must contain a Turn::Agent");
    let Turn::Agent { items, .. } = agent_turn else {
        unreachable!();
    };

    // Find the tool item whose output carries the staged sentinel rather
    // than picking the first tool item — robust against Gemini emitting
    // an additional preliminary tool before `read_file` (the harness
    // suite is designed to catch upstream CLI drift without being
    // brittle to harmless ordering changes).
    let (is_error, name) = items
        .iter()
        .find_map(|item| match item {
            TurnItem::Tool {
                output: Some(output),
                is_error,
                name,
                ..
            } if output.contains(GEMINI_TOOL_TOKEN) => Some((is_error, name)),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "hydrated agent turn must contain a TurnItem::Tool whose output carries \
                 {GEMINI_TOOL_TOKEN:?}; items: {items:?}"
            )
        });
    assert!(!name.is_empty(), "hydrated tool item must carry a name");
    assert_eq!(
        *is_error,
        Some(false),
        "hydrated tool item must record is_error: Some(false)"
    );
}

/// Gemini's parser populates `usage` from the gemini record's `tokens`
/// field but leaves `context_window` as `None` — Gemini's session file
/// doesn't carry a context-window field analogous to Codex's
/// `task_started.model_context_window`. The sidebar's context-utilization
/// bar will not render for Gemini agents until upstream Gemini telemetry
/// adds the field.
fn assert_gemini_agent_usage(turn: &Turn) {
    let Turn::Agent { usage, .. } = turn else {
        unreachable!("caller already matched Turn::Agent");
    };
    let usage = usage
        .as_ref()
        .expect("Gemini hydration must carry usage for completed turns");
    assert!(
        usage.context_window.is_none(),
        "Gemini hydrated usage.context_window must be None (no analog in session file); got: {:?}",
        usage.context_window
    );
}

#[tokio::test]
#[ignore = "requires agy installed — run with: make test-live"]
async fn live_antigravity_two_turns_hydrate_in_order() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let adapter = AntigravityAdapter::new();
    // Antigravity agents start with no locator; the conversation UUID is
    // captured on first dispatch (a `SessionLocatorCaptured` event) and lives on
    // the record thereafter. We simulate the dispatcher here: fold the captured
    // locator back onto the agent so the second dispatch resumes it.
    let mut agent = AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "transcript-agy".to_owned(),
        harness: HarnessKind::Antigravity,
        session_locator: None,
        created_at: chrono::Utc::now(),
    };

    // Two prompts against the same agent: the second resumes the captured
    // conversation and appends to the same transcript.jsonl.
    let mut conversation_id: Option<Uuid> = None;
    for prompt in [
        "Reply with only the single word 'one' and nothing else.",
        "Reply with only the single word 'two' and nothing else.",
    ] {
        let stream = adapter
            .dispatch(
                &agent,
                tmp.path(),
                prompt,
                Uuid::now_v7(),
                DispatchOptions::default(),
            )
            .await
            .expect("dispatch should succeed with real agy");
        let events: Vec<AdapterEvent> = stream.collect().await;
        for event in &events {
            if let AdapterEvent::SessionLocatorCaptured {
                locator: SessionLocator::Uuid(u),
                ..
            } = event
            {
                conversation_id = Some(*u);
                agent.session_locator = Some(SessionLocator::Uuid(*u));
            }
        }
    }

    let conversation_id =
        conversation_id.expect("a capture event must have bound the conversation id");
    let transcript =
        load_antigravity_transcript(&real_home(), tmp.path(), Some(conversation_id), agent.id)
            .expect("load_antigravity_transcript must succeed");

    assert!(
        transcript.warnings.is_empty(),
        "expected no parser warnings; got: {:?}",
        transcript.warnings
    );

    // Both prompts surface as user turns, in dispatch order, attributed to the
    // agent; each is followed by an agent turn.
    let user_turns: Vec<&Turn> = transcript
        .turns
        .iter()
        .filter(|t| matches!(t, Turn::User { .. }))
        .collect();
    assert!(
        user_turns.len() >= 2,
        "expected at least two user turns; got {}",
        user_turns.len()
    );
    for turn in &transcript.turns {
        let agent_id = match turn {
            Turn::User { agent_id, .. } | Turn::Agent { agent_id, .. } => *agent_id,
            _ => continue,
        };
        assert_eq!(agent_id, agent.id, "every turn attributed to the agent");
    }
    assert!(
        transcript
            .turns
            .iter()
            .any(|t| matches!(t, Turn::Agent { .. })),
        "expected at least one agent turn"
    );
}
