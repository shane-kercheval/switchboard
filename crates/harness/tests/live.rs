/// Live integration tests gated behind `#[ignore]`.
///
/// Run with: `make test-live`
///
/// Requires `claude`, `codex`, and/or `gemini` installed and authenticated.
/// Developer-local only — not run in CI. See `AGENTS.md` "Live testing
/// against real harnesses" for the policy.
use std::path::Path;

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_harness::{
    AdapterEvent, AntigravityAdapter, ClaudeCodeAdapter, CodexAdapter, DispatchOptions,
    GeminiAdapter, HarnessAdapter, RateLimitSource, TurnOutcome,
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
async fn live_claude_basic_turn_completes() {
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

    // Drift detection for promoted events — symmetric with the Codex
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

    // Rate-limit drift detection (M3/M4 contract). Claude emits a
    // `rate_limit_event` on every normal turn; our parser lifts it to a
    // `StreamOnly` `RateLimitEvent` (persisted to the metadata sidecar for
    // restart continuity) whose `info` carries the fields the Sidebar's
    // rate-limit window line reads: `resetsAt` (epoch seconds) and
    // `rateLimitType` (the window-label source). A fixture-based test keeps
    // passing if Anthropic renames or drops these; this catches it live.
    // (We can't assert `isUsingOverage` — that appears only once the 5-hour
    // window is exhausted, which a tiny live prompt won't trigger.)
    let rate_limit = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::RateLimitEvent { .. }))
        .expect("Claude must emit a rate_limit_event on a normal turn");
    match rate_limit {
        AdapterEvent::RateLimitEvent { info, source, .. } => {
            assert_eq!(
                *source,
                RateLimitSource::StreamOnly,
                "Claude rate-limit has no session-file equivalent → must be StreamOnly (persisted)"
            );
            assert!(
                info.get("resetsAt")
                    .and_then(serde_json::Value::as_i64)
                    .is_some(),
                "rate_limit_info.resetsAt must be an epoch number (Sidebar window reset reads it): {info}"
            );
            assert!(
                info.get("rateLimitType")
                    .and_then(serde_json::Value::as_str)
                    .is_some(),
                "rate_limit_info.rateLimitType must be a string (Sidebar window label derives from it): {info}"
            );
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_resume_reuses_session() {
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

// --- Codex live tests ---

fn live_codex_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "live-codex-agent".to_owned(),
        harness: HarnessKind::Codex,
        // Codex agents always have session_id = None — the per-agent
        // session-link sidecar is the system-of-record.
        session_id: None,
        created_at: chrono::Utc::now(),
    }
}

#[tokio::test]
#[ignore = "requires codex installed — run with: make test-live"]
// One cohesive end-to-end assertion sequence (completion + enrichment ordering
// + rate-limit/SessionMeta shape + sidecar); splitting it across helpers would
// scatter a single turn's drift-detection checks for no real gain.
#[allow(clippy::too_many_lines)]
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

    // Post-terminal enrichment must fire for Codex turns:
    // - TurnEnd.usage.context_window is enriched from the session file's
    //   task_started.model_context_window (Codex's stream doesn't carry it).
    // - RateLimitEvent fires every turn from token_count.rate_limits.
    // - SessionMeta fires on the first turn carrying model + cli_version +
    //   the merged MCP servers / skills registries.
    match terminal {
        AdapterEvent::TurnEnd { usage: Some(u), .. } => {
            assert!(
                u.context_window.is_some(),
                "TurnEnd.usage.context_window must be enriched from session file (got None)"
            );
        }
        _ => panic!("expected TurnEnd with Some(usage), got: {terminal:?}"),
    }
    let rate_limit_idx = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::RateLimitEvent { .. }))
        .expect("RateLimitEvent must fire post-terminal for Codex");
    let session_meta_idx = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::SessionMeta { .. }))
        .expect("SessionMeta must fire on first turn for Codex");
    assert!(
        terminal_idx < rate_limit_idx && rate_limit_idx < session_meta_idx,
        "enrichment events must arrive after TurnEnd in order: TurnEnd → RateLimitEvent → SessionMeta"
    );

    // Rate-limit payload-shape drift detection (M4 contract). The ordering
    // check above proves the event fires; this proves its `info` still carries
    // the fields the Sidebar's Codex windows read: `primary.used_percent` (the
    // gauge — relied on since the original single cell), plus `window_minutes`
    // (the window-label source) and `resets_at` (the tooltip reset time).
    // SessionFileBacked — Codex's own session file is canonical, so we don't
    // re-persist it. `secondary` is intentionally not asserted (a fresh
    // account may not have a weekly window yet; the Sidebar shows it only when
    // present).
    match &events[rate_limit_idx] {
        AdapterEvent::RateLimitEvent { info, source, .. } => {
            assert_eq!(
                *source,
                RateLimitSource::SessionFileBacked,
                "Codex rate-limit is read from its session file (class B) → not re-persisted"
            );
            let primary = info
                .get("primary")
                .expect("rate_limits.primary must be present: {info}");
            assert!(
                primary
                    .get("used_percent")
                    .and_then(serde_json::Value::as_f64)
                    .is_some(),
                "primary.used_percent must be a number (Sidebar gauge reads it): {info}"
            );
            assert!(
                primary
                    .get("window_minutes")
                    .and_then(serde_json::Value::as_i64)
                    .is_some(),
                "primary.window_minutes must be present (Sidebar window label derives from it): {info}"
            );
            assert!(
                primary
                    .get("resets_at")
                    .and_then(serde_json::Value::as_i64)
                    .is_some(),
                "primary.resets_at must be present (Sidebar tooltip reset time reads it): {info}"
            );
        }
        _ => unreachable!(),
    }

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

// --- Gemini live tests ---

fn live_gemini_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "live-gemini-agent".to_owned(),
        harness: HarnessKind::Gemini,
        // Gemini follows the Claude shape — caller-controlled session UUID.
        // **UUID v4, not v7**: Gemini's session-file filename uses the first
        // 8 hex chars of the session UUID, and v7s minted in the same
        // millisecond share their first 8 chars. v4 makes the collision
        // probability ~1/2^32. The production `Project::register_agent`
        // honors this contract; tests must mint v4 explicitly here too.
        session_id: Some(Uuid::new_v4()),
        created_at: chrono::Utc::now(),
    }
}

#[tokio::test]
#[ignore = "requires gemini installed — run with: make test-live"]
async fn live_gemini_basic_turn_completes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = GeminiAdapter::new();
    let agent = live_gemini_agent();
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
        .expect("dispatch should succeed with real gemini");

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

    // Gemini emits `SessionMeta` from its stream `init` event on every
    // dispatch. The contract is asymmetric vs. Claude / Codex:
    // - `model` populates from `init.model`.
    // - `tools` is always `vec![]` — Gemini's `init` doesn't carry tools.
    // - `harness_version` comes from a lazy `gemini --version` fetch on
    //   first dispatch; empty string is tolerated if the version probe
    //   fails (the field is display-only).
    // - `mcp_servers` / `skills` come from the adapter's
    //   loader injection (settings.json / ~/.agents/skills). Structural
    //   checks only — content is developer-environment-dependent (we
    //   don't pin a particular setup), matching Codex's live-test
    //   discipline.
    let session_meta = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::SessionMeta { .. }))
        .expect("Gemini must emit SessionMeta from stream init on every dispatch");
    match session_meta {
        AdapterEvent::SessionMeta {
            model,
            tools,
            mcp_servers,
            skills,
            ..
        } => {
            assert!(!model.is_empty(), "SessionMeta.model must be non-empty");
            assert!(tools.is_empty(), "Gemini SessionMeta.tools is vec![]");
            // Structural check only — the loader emits real entries when
            // settings.json / ~/.agents/skills carry config, [] otherwise.
            // Both shapes are valid; pinning content would couple the
            // test to the developer's machine.
            let _: &Vec<_> = mcp_servers;
            let _: &Vec<_> = skills;
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
#[ignore = "requires gemini installed — run with: make test-live"]
async fn live_gemini_resume_reuses_session() {
    // Memorize-then-recall: definitive proof that `--resume` restores the
    // prior turn's context (matches the Codex pattern, strictly stronger
    // than "two completes succeed"). The adapter must detect the session
    // file from turn 1 and switch from `--session-id` to `--resume` on
    // turn 2. A regression in the path-lookup helper would surface here
    // as the recall failing.
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = GeminiAdapter::new();
    let agent = live_gemini_agent();

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
        "--resume must restore the prior turn's context: turn2 reply was {recall_text:?}"
    );
}

// --- Antigravity live tests ---

fn live_antigravity_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "live-antigravity-agent".to_owned(),
        harness: HarnessKind::Antigravity,
        // Antigravity assigns the conversation UUID server-side; the adapter
        // captures it post-spawn into the per-agent sidecar. Always None.
        session_id: None,
        created_at: chrono::Utc::now(),
    }
}

#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_basic_turn_completes() {
    // cwd is a tempdir so the sidecar lands in a clean location. Note: `agy`
    // also writes a `.antigravitycli/` dir into cwd as a side effect — fine
    // here because the tempdir is discarded.
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::new();
    let agent = live_antigravity_agent();
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
        .expect("dispatch should succeed with real agy");
    let events: Vec<AdapterEvent> = stream.collect().await;

    // Assistant text comes from the transcript's per-turn `PLANNER_RESPONSE`
    // record, not stdout: `agy`'s stdout replays the whole conversation on
    // resume, so the transcript is the clean per-turn source (tool lifecycle
    // and thinking are tailed from the same file).
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        text.to_lowercase().contains("ack"),
        "expected 'ack' in transcript-derived response text, got: {text:?}"
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

    // SessionMeta fires post-terminal. Model is best-effort from the user
    // settings envelope; mcp_servers / skills come from the dispatch-time
    // loader injection (`~/.gemini/config/mcp_config.json` and
    // `~/.gemini/config/plugins/*/skills/*`). Structural-only checks — the
    // dev env varies, so we assert presence and types, not values (matching
    // Codex/Gemini live discipline).
    let session_meta = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::SessionMeta { .. }))
        .expect("Antigravity must emit SessionMeta post-terminal");
    match session_meta {
        AdapterEvent::SessionMeta {
            tools,
            mcp_servers,
            skills,
            ..
        } => {
            assert!(tools.is_empty(), "Antigravity SessionMeta.tools is vec![]");
            let _: &Vec<_> = mcp_servers;
            let _: &Vec<_> = skills;
        }
        other => panic!("expected SessionMeta, got {other:?}"),
    }

    // Sidecar must exist after the first turn with the captured conversation
    // UUID — the system-of-record for resume / hydration.
    let sidecar = tmp
        .path()
        .join(".switchboard")
        .join("projects")
        .join(agent.project_id.to_string())
        .join("sessions")
        .join(format!("{}.antigravity.jsonl", agent.id));
    assert!(
        sidecar.is_file(),
        "sidecar must be written on first dispatch once the conversation UUID is captured"
    );
    let content = std::fs::read_to_string(&sidecar).unwrap();
    assert!(content.contains("conversation_id"));
}

#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_adversarial_prompt_still_completes() {
    // Guards the load-bearing assumption that `agy` echoes the dispatched
    // prompt verbatim into the transcript's `<USER_REQUEST>` body. Capture
    // correlates on exact prompt match and a miss is now fatal (unresumable
    // → failed turn), so if `agy` reformats a non-trivial prompt (reindents
    // multi-line, escapes quotes, mangles unicode), correlation would miss
    // and this turn would FAIL. A `Completed` outcome proves the prompt was
    // echoed verbatim and the exact-match gate is sound for realistic
    // prompts. If this test fails, loosen `transcript_echoes_prompt` to a
    // whitespace-normalized comparison (see its docstring).
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::new();
    let agent = live_antigravity_agent();
    let turn_id = Uuid::now_v7();

    // Multi-line, indented, quoted, and unicode — the shapes most likely to
    // be reformatted. Keep the actual task tiny so the response is cheap.
    let prompt = "Follow these steps exactly:\n  1. Note the word \"café\".\n  2. Ignore everything else.\nReply with only the single word: ack";

    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            prompt,
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real agy");
    let events: Vec<AdapterEvent> = stream.collect().await;

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
        "adversarial prompt must still correlate + complete (verbatim-echo assumption); \
         got: {terminal:?}. If this is the unresumable AdapterFailure, agy reformatted the \
         prompt — loosen transcript_echoes_prompt to a whitespace-normalized match."
    );

    // And the sidecar was captured — i.e. correlation actually matched.
    let sidecar = tmp
        .path()
        .join(".switchboard")
        .join("projects")
        .join(agent.project_id.to_string())
        .join("sessions")
        .join(format!("{}.antigravity.jsonl", agent.id));
    assert!(
        sidecar.is_file(),
        "correlation must have matched the adversarial prompt and persisted the sidecar"
    );
}

#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_resume_reuses_session() {
    // Memorize-then-recall: proof that `--conversation <uuid>` (driven by the
    // sidecar-captured UUID) restores the prior turn's server-side context.
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::new();
    let agent = live_antigravity_agent();

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
        "--conversation resume must restore prior context: turn2 reply was {recall_text:?}"
    );

    // Two dispatches → two sidecar records, same conversation_id (resume
    // reuses the captured UUID rather than minting a new conversation).
    let sidecar = tmp
        .path()
        .join(".switchboard")
        .join("projects")
        .join(agent.project_id.to_string())
        .join("sessions")
        .join(format!("{}.antigravity.jsonl", agent.id));
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
        r1["conversation_id"], r2["conversation_id"],
        "resume must reuse the captured conversation UUID"
    );
}
