/// Live integration tests gated behind `#[ignore]`.
///
/// Run with: `make test-live`
///
/// Requires `claude`, `codex`, and/or `gemini` installed and authenticated.
/// Developer-local only — not run in CI. See `AGENTS.md` "Live testing
/// against real harnesses" for the policy.
use std::path::Path;

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind, SessionLocator};
use switchboard_harness::{
    AdapterEvent, AntigravityAdapter, ClaudeCodeAdapter, CodexAdapter, ContentKind,
    DispatchOptions, GeminiAdapter, HarnessAdapter, RateLimitSource, TurnOutcome,
};
use uuid::Uuid;

fn live_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "live-test-agent".to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_locator: Some(SessionLocator::Uuid(Uuid::now_v7())),
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
async fn live_claude_rate_limit_precedes_result() {
    // Per-turn cost/overage stamping (M3) depends on the Claude parser seeing
    // the turn's `rate_limit_event` (which carries `isUsingOverage`) BEFORE the
    // terminal event: the `ParserState` overage stash is set when the
    // rate-limit is parsed and read when the `TurnEnd` is built. If a CLI bump
    // ever reordered or dropped the rate-limit, an overage turn would render
    // with no cost and no marker — silent under-reporting on the money path,
    // the worst failure direction for this feature. Fixture tests can't catch
    // that drift (they replay the assumed order); this pins the ordering +
    // presence invariant against the live CLI.
    //
    // The `isUsingOverage == true` branch itself is NOT live-coverable — we
    // can't force overage on demand — so that residual risk is accepted
    // knowingly; this guards the ordering/presence the stamp rests on.
    let adapter = ClaudeCodeAdapter::new();
    let agent = live_agent();
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "Reply with only the word ack.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");
    let events: Vec<AdapterEvent> = stream.collect().await;

    let rate_limit_idx = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::RateLimitEvent { .. }))
        .expect("Claude must emit a rate_limit_event every turn (the overage stash depends on it)");
    let turn_end_idx = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("should have a terminal TurnEnd");
    assert!(
        rate_limit_idx < turn_end_idx,
        "rate_limit_event must precede the terminal TurnEnd so the parser's overage stash is set \
         before the turn is stamped; got rate_limit at {rate_limit_idx}, TurnEnd at {turn_end_idx}"
    );
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_thinking_emits_liveness() {
    // While the model reasons, the CLI streams `thinking_delta` /
    // `signature_delta`. On a redacting model (Opus 4.8 — the dev's default,
    // so what this test exercises) the thinking text is empty and the adapter
    // surfaces `Liveness`; on a non-redacting model (Sonnet 4.6) it surfaces a
    // `Thinking` `ContentChunk`. Either keeps the frontend heartbeat from
    // falsely failing a long thinking turn. If a CLI bump stops streaming
    // during thinking, this test catches it (the fixture test proves the
    // parser maps the delta; this proves the delta still arrives live).
    //
    // COVERAGE GAP: the non-empty `Thinking` branch is NOT live-covered. This
    // test runs on the dev default (Opus → redacted), so live runs only ever
    // hit `Liveness`. We can't pin Sonnet because the adapter has no `--model`
    // plumbing yet; until per-agent model selection lands
    // (`docs/implementation_plans/2026-05-30-per-agent-model-selection.md` M2),
    // the "real Sonnet still returns un-redacted reasoning" contract is held
    // only by the `thinking_delta_with_text_yields_thinking_chunk` unit test
    // plus the per-model re-probe mandate (`harness-behavior.md` §3.2).
    let adapter = ClaudeCodeAdapter::new();
    let agent = live_agent();
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "Think step by step and reason carefully before answering. ultrathink. \
             Then reply with only the number 4 and nothing else.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");

    let events: Vec<AdapterEvent> = stream.collect().await;

    // A thinking block must produce a sign of life that re-arms the heartbeat:
    // either `Liveness` (a redacting model like Opus 4.8) or
    // `ContentChunk { Thinking }` (a non-redacting model like Sonnet 4.6).
    // Both are product-correct — assert the behavior, not which variant
    // arrives, so the per-model redaction split doesn't read as a regression.
    let sign_of_life = events.iter().any(|e| {
        matches!(e, AdapterEvent::Liveness { turn_id: t } if *t == turn_id)
            || matches!(
                e,
                AdapterEvent::ContentChunk { turn_id: t, kind: ContentKind::Thinking, .. }
                    if *t == turn_id
            )
    });
    assert!(
        sign_of_life,
        "expected a thinking sign-of-life (Liveness or Thinking ContentChunk), got none; events: {events:?}"
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
async fn live_claude_multi_call_turn_context_occupancy_is_final_call() {
    // Context-occupancy drift guard for tool-use (multi-call) turns.
    //
    // A turn that calls a tool makes ≥2 model calls. Claude's terminal
    // `result.usage` reports usage SUMMED across those calls (verified on
    // 2.1.161: a 2-call turn's input / cache_read / cache_creation are the
    // per-call sums). Using that sum as window occupancy double-counts the
    // shared cached prefix and over-reports ~N× for an N-call turn. The adapter
    // therefore derives `context_input_tokens` from the FINAL assistant
    // message's own usage (the real current window contents), not from
    // `result.usage`.
    //
    // This is the exact assumption that can only be checked against the real
    // CLI: a fixture proves our parser maps the shapes, but only a live run
    // proves Claude still (a) emits per-message usage on each assistant message
    // and (b) sums it into `result.usage`. If a CLI bump changes either, the
    // occupancy bar silently regresses — this catches it.
    let adapter = ClaudeCodeAdapter::new();
    let agent = live_agent();
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "Use the Bash tool to run `echo hi`, then reply with only the word: done.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");

    let events: Vec<AdapterEvent> = stream.collect().await;

    // The test is only meaningful if a tool actually ran (forcing a 2nd model
    // call). If this fails, the prompt no longer triggers a tool call — fix the
    // prompt, don't weaken the assertion.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::ToolStarted { .. })),
        "prompt must trigger a tool call so the turn makes ≥2 model calls; events: {events:?}"
    );

    let terminal = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("should have a terminal TurnEnd");
    let AdapterEvent::TurnEnd {
        outcome: TurnOutcome::Completed,
        usage: Some(usage),
        ..
    } = terminal
    else {
        panic!("expected TurnEnd(Completed) with usage, got: {terminal:?}");
    };

    // The summed-across-calls total that `result.usage` reports (the trap).
    let summed_total = usage.input_tokens
        + usage.cached_input_tokens.unwrap_or(0)
        + usage.cache_creation_input_tokens.unwrap_or(0);
    let occupancy = usage
        .context_input_tokens
        .expect("context_input_tokens must be populated for a Claude turn");

    assert!(occupancy > 0, "occupancy must be non-zero, got 0");
    assert!(
        occupancy < summed_total,
        "occupancy ({occupancy}) must be the FINAL call's prompt size — strictly less than the \
         across-call sum `result.usage` reports ({summed_total}). Equal/greater means the adapter \
         regressed to using the summed total, which over-reports the context bar on tool-use turns."
    );
    if let Some(window) = usage.context_window {
        assert!(
            occupancy <= u64::from(window),
            "occupancy ({occupancy}) must not exceed the context window ({window})"
        );
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
        session_locator: Some(SessionLocator::Uuid(session_id)),
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
        session_locator: Some(SessionLocator::Uuid(session_id)),
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

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_dash_leading_prompt_completes() {
    // Canary: a prompt beginning with `-` must dispatch and complete. `claude`'s
    // parser (commander) takes the prompt as a positional, so without the `--`
    // separator a leading-dash prompt aborts with `unknown option '- …'`. Doubles
    // as a drift tripwire if a CLI bump changes prompt parsing.
    let adapter = ClaudeCodeAdapter::new();
    let agent = live_agent();
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "- Reply with the single word 'ack' and nothing else.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with a dash-leading prompt");
    let events: Vec<AdapterEvent> = stream.collect().await;

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk { text, .. } => Some(text.clone()),
            _ => None,
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
        "dash-leading prompt must complete, got: {terminal:?}"
    );
}

// --- Codex live tests ---

fn live_codex_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "live-codex-agent".to_owned(),
        harness: HarnessKind::Codex,
        // A fresh Codex agent has no locator until its first dispatch captures
        // one (emitted as a `SessionLocatorCaptured` event, persisted by the
        // dispatcher onto the record).
        session_locator: None,
        created_at: chrono::Utc::now(),
    }
}

/// The Codex locator carried by a dispatch's capture event, or `None` on resume.
fn codex_capture(events: &[AdapterEvent]) -> Option<(String, chrono::NaiveDate)> {
    events.iter().find_map(|e| match e {
        AdapterEvent::SessionLocatorCaptured {
            locator:
                SessionLocator::Codex {
                    thread_id,
                    partition_date,
                },
        } => Some((thread_id.clone(), *partition_date)),
        _ => None,
    })
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

    // The first turn emits a capture event with the thread_id + partition-date
    // (the dispatcher persists it to the registry; no sidecar is written).
    let (thread_id, _date) =
        codex_capture(&events).expect("first dispatch emits a captured Codex locator");
    assert!(
        !thread_id.is_empty(),
        "captured thread_id must be non-empty"
    );

    let sidecar = tmp
        .path()
        .join(".switchboard")
        .join("projects")
        .join(agent.project_id.to_string())
        .join("sessions")
        .join(format!("{}.jsonl", agent.id));
    assert!(
        !sidecar.exists(),
        "the adapter no longer writes a session-link sidecar"
    );
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

    // Turn 1: ask Codex to remember a specific word (fresh agent → first
    // dispatch captures the locator).
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
    let events1: Vec<AdapterEvent> = stream1.collect().await;

    // Simulate the dispatcher: fold the captured locator onto the agent so the
    // next dispatch resumes via the registry-stored locator (the production
    // factory live-reads it from `agents_by_id`).
    let (thread_id, partition_date) =
        codex_capture(&events1).expect("first dispatch emits a captured Codex locator");
    let resumed_agent = AgentRecord {
        session_locator: Some(SessionLocator::Codex {
            thread_id,
            partition_date,
        }),
        ..agent.clone()
    };

    // Turn 2 (resume): ask Codex to recall the word. The prompt deliberately
    // begins with `-` (a markdown bullet) — the exact shape that crashed the
    // resume path in production before the `--` end-of-options separator was
    // added to `build_args`. A leading-dash prompt would otherwise make clap
    // abort with `unexpected argument '- '` and fail the turn.
    let turn2 = Uuid::now_v7();
    let stream2 = adapter
        .dispatch(
            &resumed_agent,
            tmp.path(),
            "- What word did I ask you to remember? Reply with only that word.",
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

    // A resume reuses the record's locator and emits no further capture event.
    assert!(
        codex_capture(&events2).is_none(),
        "a resume must not re-capture the locator"
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
        session_locator: Some(SessionLocator::Uuid(Uuid::new_v4())),
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

#[tokio::test]
#[ignore = "requires gemini installed — run with: make test-live"]
async fn live_gemini_streamed_answer_completes_despite_trailing_error() {
    // Gemini frequently (observed ~60% of runs on CLI 0.44.0, model auto)
    // appends an empty/malformed trailing step after a complete answer,
    // tainting the turn's `result.status` to `"error"`. This test pins the
    // post-fix invariant: a turn that streamed a complete answer ALWAYS
    // completes, regardless of whether the benign trailing error fired this
    // run. It does NOT force the error (it's non-deterministic) — the
    // deterministic proof of the rescue path is the fixture test
    // `benign_trailing_error_fixture_completes_despite_result_error` in
    // `gemini_adapter.rs`. A read-and-summarize prompt over a small file
    // reliably triggers the tool-use shape where the quirk appears.
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("hello.txt"),
        "Hello, world! This is a tiny test file.",
    )
    .unwrap();

    let adapter = GeminiAdapter::new();
    let agent = live_gemini_agent();
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "Read and summarize the files in this directory in one short sentence.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real gemini");

    let events: Vec<AdapterEvent> = stream.collect().await;

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !text.trim().is_empty(),
        "expected the model to stream a summary, got empty text"
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
        "a turn that streamed a complete answer must complete even if Gemini \
         appended a benign trailing error, got: {terminal:?}"
    );
}

#[tokio::test]
#[ignore = "requires gemini installed — run with: make test-live"]
async fn live_gemini_dash_leading_prompt_completes() {
    // Canary: a prompt beginning with `-` must dispatch and complete. `gemini`'s
    // parser (yargs) otherwise rejects a `-`-leading `-p` value with `Not enough
    // arguments following: p` — the adapter passes it as `--prompt=<value>`.
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = GeminiAdapter::new();
    let agent = live_gemini_agent();
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "- Reply with the single word 'ack' and nothing else.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with a dash-leading prompt");
    let events: Vec<AdapterEvent> = stream.collect().await;

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk { text, .. } => Some(text.clone()),
            _ => None,
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
        "dash-leading prompt must complete, got: {terminal:?}"
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
        // captures it post-spawn and emits it as a `SessionLocatorCaptured`
        // event (the dispatcher persists it onto the record). Always None.
        session_locator: None,
        created_at: chrono::Utc::now(),
    }
}

/// The Antigravity conversation UUID carried by a dispatch's capture event, or
/// `None` on a resume (which reuses the record's locator and emits nothing).
fn antigravity_capture(events: &[AdapterEvent]) -> Option<Uuid> {
    events.iter().find_map(|e| match e {
        AdapterEvent::SessionLocatorCaptured {
            locator: SessionLocator::Uuid(uuid),
        } => Some(*uuid),
        _ => None,
    })
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

    // The first turn emits a capture event carrying the conversation UUID (the
    // dispatcher persists it to the registry; no sidecar is written).
    let conversation_id =
        antigravity_capture(&events).expect("first dispatch emits a captured Antigravity locator");
    assert!(
        !conversation_id.is_nil(),
        "captured conversation UUID must be non-nil"
    );

    let sidecar = tmp
        .path()
        .join(".switchboard")
        .join("projects")
        .join(agent.project_id.to_string())
        .join("sessions")
        .join(format!("{}.antigravity.jsonl", agent.id));
    assert!(
        !sidecar.exists(),
        "the adapter no longer writes a session-link sidecar"
    );
}

#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_cli_log_names_the_conversation() {
    // The adapter's PRIMARY conversation-id capture reads `agy`'s own
    // `--log-file` (a `Created conversation <uuid>` line) — deterministic and
    // concurrency-safe, unlike the prompt-correlation fallback. That log line is
    // a Google-internal debug string, so a CLI bump could move it; if it did,
    // the adapter would silently fall back and the other live tests would still
    // pass, masking the drift. This test guards the log contract directly: run
    // real `agy` exactly as the adapter does and assert its `--log-file` still
    // names the conversation in the form `conversation_id_from_log` parses.
    let tmp = tempfile::TempDir::new().unwrap();
    let log = tmp.path().join("agy.log");
    let status = tokio::process::Command::new("agy")
        .args([
            "-p",
            "Reply with the single word 'ack' and nothing else.",
            "--dangerously-skip-permissions",
            "--log-file",
        ])
        .arg(&log)
        .current_dir(tmp.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .expect("agy should spawn");
    assert!(status.success(), "agy exited non-zero: {status:?}");

    let content = std::fs::read_to_string(&log).expect("agy should write the --log-file");
    let names_conversation = content.lines().any(|line| {
        ["Created conversation ", "conversation="]
            .iter()
            .any(|marker| {
                line.split_once(marker)
                    .map(|(_, rest)| rest.chars().take(36).collect::<String>())
                    .and_then(|token| Uuid::parse_str(&token).ok())
                    .is_some()
            })
    });
    assert!(
        names_conversation,
        "agy --log-file no longer names the conversation in a parseable form; the adapter's \
         primary capture (antigravity::conversation_id_from_log) needs updating. Log:\n{content}"
    );
}

#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_adversarial_prompt_still_completes() {
    // Guards the *fallback* capture's load-bearing assumption that `agy` echoes
    // the dispatched prompt verbatim into the transcript's `<USER_REQUEST>`
    // body. Primary capture now reads the conversation id from the CLI log, so
    // this dispatch binds via the log (not correlation); but the
    // prompt-correlation fallback still runs if that log line ever moves, and a
    // fallback miss is fatal (unresumable → failed turn). This sends the gnarly
    // multi-line/quoted/unicode shapes most likely to be reformatted and asserts
    // the real CLI both completes and echoes them verbatim — so the fallback's
    // exact-match gate stays sound. If this fails, loosen
    // `transcript_echoes_prompt` to a whitespace-normalized comparison (see its
    // docstring).
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

    // And the conversation UUID was captured — i.e. correlation actually
    // matched and emitted the locator capture event.
    assert!(
        antigravity_capture(&events).is_some(),
        "correlation must have matched the adversarial prompt and emitted a captured locator"
    );
}

#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_resume_reuses_session() {
    // Memorize-then-recall: proof that `--conversation <uuid>` (driven by the
    // registry-stored locator) restores the prior turn's server-side context.
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
    let events1: Vec<AdapterEvent> = stream1.collect().await;

    // Simulate the dispatcher: fold the captured conversation UUID onto the
    // agent so the next dispatch resumes via the registry-stored locator (the
    // production factory live-reads it from `agents_by_id`).
    let conversation_id =
        antigravity_capture(&events1).expect("first dispatch emits a captured Antigravity locator");
    let resumed_agent = AgentRecord {
        session_locator: Some(SessionLocator::Uuid(conversation_id)),
        ..agent.clone()
    };

    let turn2 = Uuid::now_v7();
    let stream2 = adapter
        .dispatch(
            &resumed_agent,
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

    // A resume reuses the record's locator and emits no further capture event
    // (unless `agy` forked an expired conversation, which this prompt won't).
    assert!(
        antigravity_capture(&events2).is_none(),
        "a resume must not re-capture the locator"
    );
}

#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_dash_leading_prompt_completes() {
    // Canary / drift tripwire: `agy` currently tolerates a `-`-leading `-p`
    // value (so the adapter passes the prompt unchanged), unlike claude/gemini/
    // codex. This guards that assumption — if a CLI bump makes `agy` dash-
    // sensitive, this fails and the adapter needs the same treatment.
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::new();
    let agent = live_antigravity_agent();
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "- Reply with the single word 'ack' and nothing else.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with a dash-leading prompt");
    let events: Vec<AdapterEvent> = stream.collect().await;

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
        "dash-leading prompt must complete, got: {terminal:?}"
    );
}
