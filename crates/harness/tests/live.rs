/// Live integration tests gated behind `#[ignore]`.
///
/// Run with: `make test-live`
///
/// Requires `claude`, `codex`, and/or `agy` installed and authenticated.
/// Developer-local only — not run in CI. See `AGENTS.md` "Live testing
/// against real harnesses" for the policy.
use std::path::Path;

use futures::StreamExt;
use serde_json::json;
use switchboard_core::{AgentRecord, HarnessKind, SessionLocator};
use switchboard_harness::{
    AdapterEvent, AntigravityAdapter, ClaudeCodeAdapter, CodexAdapter, ContentKind,
    ContextWindowSource, DispatchOptions, EditChange, FailureKind, HarnessAdapter, RateLimitSource,
    ToolFacet, Turn, TurnItem, TurnOutcome, UserPromptSource, claude_session_file_path,
    load_antigravity_transcript, load_claude_transcript, load_codex_transcript,
};
use uuid::Uuid;

/// The per-turn `(model, effort)` from the (single) `TurnEnd` in an event stream.
fn turn_end_model_effort(events: &[AdapterEvent]) -> Option<(Option<String>, Option<String>)> {
    events.iter().find_map(|e| match e {
        AdapterEvent::TurnEnd { model, effort, .. } => Some((model.clone(), effort.clone())),
        _ => None,
    })
}

/// The `first_message_id` (the live-emitted `hydration_key`) from the `TurnEnd`.
fn turn_end_first_message_id(events: &[AdapterEvent]) -> Option<String> {
    events.iter().find_map(|e| match e {
        AdapterEvent::TurnEnd {
            first_message_id, ..
        } => first_message_id.clone(),
        _ => None,
    })
}

/// The `hydration_key` of every hydrated agent turn that has one, in order.
fn agent_hydration_keys(t: &switchboard_harness::LoadedTranscript) -> Vec<String> {
    t.turns
        .iter()
        .filter_map(|turn| match turn {
            Turn::Agent { hydration_key, .. } => hydration_key.clone(),
            _ => None,
        })
        .collect()
}

/// Per-turn `model` of every hydrated agent turn, in order.
fn hydrated_turn_models(t: &switchboard_harness::LoadedTranscript) -> Vec<Option<String>> {
    t.turns
        .iter()
        .filter_map(|turn| match turn {
            Turn::Agent { model, .. } => Some(model.clone()),
            _ => None,
        })
        .collect()
}

fn home_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").expect("HOME set"))
}

fn live_agent() -> AgentRecord {
    AgentRecord {
        session_home: None,
        model: None,
        effort: None,
        profiles: switchboard_core::AgentProfiles::default(),
        forked_from_session: None,
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
async fn live_claude_background_agent_completes_as_one_turn() {
    // Drift guard for the background-agent dispatch grammar (probed at
    // 2.1.198): one `claude -p` process runs N+1 internal init→result cycles,
    // and the adapter must present them as ONE turn — a single Completed
    // terminal after ALL cycles' text. If Anthropic changes the cycle
    // grammar (result timing, task-event vocabulary, exit behavior), this
    // catches it before it ships. Cost note: deliberately above the
    // one-word-reply discipline (~$0.10–0.20/run — it must genuinely run a
    // background sub-agent); the fixture-driven `background-agent*` tests
    // are the free, hermetic coverage the default suite runs instead.
    let adapter = ClaudeCodeAdapter::new();
    let agent = live_agent();
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "Use your Agent tool to launch exactly ONE subagent with run_in_background set to \
             true, whose prompt is: Reply with the single word ack. Immediately after launching \
             it, output the single word: waiting. Do not poll it. When the background task \
             completion notification arrives, output the single word: done. Keep every response \
             minimal.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");

    let events: Vec<AdapterEvent> = stream.collect().await;

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(
        terminals.len(),
        1,
        "one dispatch = one terminal, regardless of internal cycles"
    );
    assert!(
        matches!(
            terminals[0],
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        ),
        "expected TurnEnd(Completed), got: {:?}",
        terminals[0]
    );

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("waiting") && text.contains("done"),
        "both cycles' text must stream (pre- and post-notification), got: {text:?}"
    );
    assert!(
        matches!(events.last(), Some(AdapterEvent::TurnEnd { .. })),
        "the terminal arrives after all content"
    );
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

    // Rate-limit drift detection. Claude emits a
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

/// Live-matched-key drift guard: the dedup identity the live
/// `TurnEnd` carries — `first_message_id`, surfaced to the frontend as
/// `hydration_key` — must equal the `hydration_key` the session-file parser
/// reconstructs for the same turn. That equality is exactly what makes Claude
/// "live-matched" and lets the whole-file refresh dedup a turn that streamed
/// live against its on-disk copy. A CLI bump that diverged the streamed first
/// `message.id` from the on-disk one would silently break that dedup (re-read
/// would duplicate the turn); this catches it before users do.
///
/// Note: the "ack" prompt yields a single-message turn, where the first and
/// final assistant `message.id` coincide — so this also incidentally exercises
/// `stable_message_id` parity. The first/final *distinction* is pinned by the
/// hermetic `tool_use_turn_anchors_keys_first_and_final` tests.
#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_hydration_key_matches_live_turn_end() {
    let adapter = ClaudeCodeAdapter::new();
    let agent = live_agent();
    let Some(SessionLocator::Uuid(session_id)) = agent.session_locator else {
        unreachable!("live claude agent has a uuid locator")
    };
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "Reply with only the word ack and nothing else.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");
    let events: Vec<AdapterEvent> = stream.collect().await;

    let live_key = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::TurnEnd {
                first_message_id, ..
            } => Some(first_message_id.clone()),
            _ => None,
        })
        .expect("a terminal TurnEnd")
        .expect("Claude's TurnEnd must carry a first_message_id (the live-matched dedup key)");

    let loaded = load_claude_transcript(&home_dir(), Path::new("/tmp"), session_id, agent.id)
        .expect("loading the just-written session file should succeed");
    let disk_key = loaded
        .turns
        .iter()
        .find_map(|t| match t {
            Turn::Agent { hydration_key, .. } => hydration_key.clone(),
            _ => None,
        })
        .expect("the hydrated agent turn must carry a hydration_key");

    assert_eq!(
        live_key, disk_key,
        "the live TurnEnd's first-message id must equal the parser's reconstructed hydration_key"
    );
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_dispatched_prompt_classified_sdk() {
    // The transcript-merge dedup keys on Claude writing `promptSource:"sdk"` for
    // a Switchboard (SDK) dispatch, which the parser classifies as
    // `UserPromptSource::Sdk`. If a CLI version renames/drops the field the
    // parser falls back to `Unknown` and the merge silently reverts to the
    // fragile count-based path — this catches that drift before it ships.
    let adapter = ClaudeCodeAdapter::new();
    let agent = live_agent();
    let Some(SessionLocator::Uuid(session_id)) = agent.session_locator else {
        unreachable!("live claude agent has a uuid locator")
    };
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "Reply with only the word ack and nothing else.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");
    let _events: Vec<AdapterEvent> = stream.collect().await;

    let loaded = load_claude_transcript(&home_dir(), Path::new("/tmp"), session_id, agent.id)
        .expect("loading the just-written session file should succeed");
    let source = loaded
        .turns
        .iter()
        .find_map(|t| match t {
            Turn::User { source, .. } => Some(*source),
            _ => None,
        })
        .expect("the dispatched prompt must appear as a user turn");
    assert_eq!(
        source,
        UserPromptSource::Sdk,
        "an SDK dispatch must write promptSource:\"sdk\" (parsed as Sdk); a fallback to Unknown means the field drifted"
    );
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_dispatched_prompt_never_carries_is_meta() {
    // `is_meta_continuation` (session_file.rs) rests on the invariant that a
    // genuinely dispatched prompt never carries `isMeta:true` — if a CLI
    // version stamped it on a real prompt, the parser would silently drop the
    // prompt and merge the following turn backward into the previous one.
    // Reads the raw session-file record for the dispatched prompt to catch
    // that drift, mirroring the `promptSource:"sdk"` guard above.
    const PROMPT: &str = "Reply with only the word ack and nothing else.";
    let adapter = ClaudeCodeAdapter::new();
    let agent = live_agent();
    let Some(SessionLocator::Uuid(session_id)) = agent.session_locator else {
        unreachable!("live claude agent has a uuid locator")
    };
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            PROMPT,
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");
    let _events: Vec<AdapterEvent> = stream.collect().await;

    let cwd = Path::new("/tmp").canonicalize().expect("canonicalize /tmp");
    let path = claude_session_file_path(&home_dir(), &cwd, &session_id);
    let content = std::fs::read_to_string(&path).expect("session file readable");
    let prompt_record = content
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|r| {
            r.get("type").and_then(serde_json::Value::as_str) == Some("user")
                && r.get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(serde_json::Value::as_str)
                    == Some(PROMPT)
        })
        .expect("the dispatched prompt must appear as a raw user record");
    assert_ne!(
        prompt_record
            .get("isMeta")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "a dispatched prompt carrying isMeta would be dropped as a mid-turn continuation — the is_meta_continuation invariant drifted"
    );
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_rate_limit_precedes_result() {
    // Per-turn cost/overage stamping depends on the Claude parser seeing
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
    // `signature_delta`. On a redacting model the thinking text is empty and
    // the adapter surfaces `Liveness`; on a non-redacting model it surfaces a
    // `Thinking` `ContentChunk`. Either keeps the frontend heartbeat from
    // falsely failing a long thinking turn. If a CLI bump stops streaming
    // during thinking, this test catches it (the fixture test proves the
    // parser maps the delta; this proves the delta still arrives live).
    //
    // As of Sonnet 5, every first-party Claude model redacts thinking in `-p`
    // (Opus 4.8 / Fable 5 / Sonnet 5 — signature only, empty text; see
    // harness-behavior.md §3.2), so today this only exercises the `Liveness`
    // branch. The assertion stays branch-agnostic so it survives a future
    // server-flag flip back to un-redacted reasoning.
    let adapter = ClaudeCodeAdapter::new();
    let agent = live_agent();
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            // A genuine multi-step word problem with a one-token answer. Trivial
            // prompts (e.g. "reply with 4") no longer engage thinking on current
            // Opus/Sonnet even with `ultrathink`, so the prompt must require real
            // reasoning to reliably produce a thinking block.
            "ultrathink. A farmer has 17 sheep; all but 9 run away. He then buys \
             twice as many as he has left, then sells 5. Reason step by step, then \
             reply with only the final number.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");

    let events: Vec<AdapterEvent> = stream.collect().await;

    // A thinking block must produce a sign of life that re-arms the heartbeat:
    // either `Liveness` (a redacting model, the current case for every
    // first-party model) or `ContentChunk { Thinking }` (a non-redacting model).
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

/// The `model` of the first `SessionMeta` in an event stream, if any.
fn session_meta_model(events: &[AdapterEvent]) -> Option<String> {
    events.iter().find_map(|e| match e {
        AdapterEvent::SessionMeta { model, .. } => Some(model.clone()),
        _ => None,
    })
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_model_and_effort_dispatch() {
    // The selected model surfaces in `SessionMeta` (proving `--model` took
    // effect end-to-end), and the dispatched effort is stamped on the terminal.
    // Claude's live stream carries no effort of its own (verified @ 2.1.241), so
    // the stamp is the adapter echoing what it passed — that is what lets the
    // live footer match the reopened one. The disk half is covered by
    // `live_claude_session_file_effort_matches_the_dispatched_level`.
    let adapter = ClaudeCodeAdapter::new();
    let mut agent = live_agent();
    agent.model = Some("sonnet".to_owned());
    agent.effort = Some("low".to_owned());
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

    let model = session_meta_model(&events).expect("Claude emits SessionMeta with model");
    assert!(
        model.contains("sonnet"),
        "selected `--model sonnet` must surface in SessionMeta.model; got {model:?}"
    );
    let terminal = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("terminal TurnEnd");
    assert!(
        matches!(
            terminal,
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        ),
        "dispatch with model+effort must complete; got {terminal:?}"
    );
    let AdapterEvent::TurnEnd { effort, .. } = terminal else {
        unreachable!("matched TurnEnd above")
    };
    assert_eq!(
        effort.as_deref(),
        Some("low"),
        "the dispatched --effort must be stamped on the live terminal"
    );
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_session_file_effort_matches_the_dispatched_level() {
    // Drift guard for the 2.1.212 contract AND for the live-stamp gate that
    // rests on it. Claude writes a **top-level** `effort` on each assistant
    // record (not under `message`, unlike the model) — but only for models that
    // have a reasoning-effort axis. Both halves are asserted per model here,
    // because both are upstream behavior a fixture cannot detect:
    //
    //   - an effort-recording family that stopped recording would strand the
    //     reopen path while the live stamp kept claiming a level;
    //   - Haiku *starting* to record would mean the gate is needlessly
    //     withholding a level we could show.
    //
    // It is also the staleness tripwire for the gate's exact-id allowlist: when
    // an alias moves to a new generation the new id is not listed, the live echo
    // stops, and the live-vs-disk assertion below fails — telling us to probe
    // the new id rather than letting it echo unverified.
    //
    // Either drift is silent offline, which is what let the ungated stamp ship.
    // Every alias the gate echoes for, plus the one it withholds for. Covering
    // only a subset would leave an upstream change to an uncovered family
    // silently recreating the bug this gate exists to prevent.
    for (model, expected) in [
        ("opus", Some("low")),
        ("sonnet", Some("low")),
        ("fable", Some("low")),
        ("haiku", None),
    ] {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let adapter = ClaudeCodeAdapter::new();
        let mut agent = live_agent();
        agent.model = Some(model.to_owned());
        agent.effort = Some("low".to_owned());

        let stream = adapter
            .dispatch(
                &agent,
                tmp.path(),
                "Reply with only the number 4 and nothing else.",
                Uuid::now_v7(),
                DispatchOptions::default(),
            )
            .await
            .expect("dispatch should succeed with real claude");
        let events: Vec<AdapterEvent> = stream.collect().await;

        let locator = match agent.session_locator {
            Some(SessionLocator::Uuid(id)) => id,
            other => panic!("expected a uuid locator, got {other:?}"),
        };
        let transcript = load_claude_transcript(&home_dir(), tmp.path(), locator, agent.id)
            .expect("load_claude_transcript must succeed");
        let disk: Vec<Option<String>> = transcript
            .turns
            .iter()
            .filter_map(|t| match t {
                switchboard_harness::Turn::Agent { effort, .. } => Some(effort.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            disk,
            vec![expected.map(str::to_owned)],
            "--model {model} --effort low: unexpected on-disk effort"
        );

        // The live stamp must agree with what the file records — that agreement
        // is the entire justification for echoing a dispatched value at all.
        let terminal = events
            .iter()
            .find_map(|e| match e {
                AdapterEvent::TurnEnd { effort, .. } => Some(effort.clone()),
                _ => None,
            })
            .expect("terminal TurnEnd");
        assert_eq!(
            terminal,
            expected.map(str::to_owned),
            "--model {model}: live stamp must match the on-disk record"
        );
    }
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_sonnet_thinking_is_redacted() {
    // Pins the current reality: Sonnet (5) redacts extended thinking in `-p`
    // mode — the block streams (signature only, empty text) so the adapter
    // surfaces `Liveness`, but no un-redacted `Thinking` prose. As of Sonnet 5,
    // every first-party Claude model redacts (Opus 4.8 / Fable 5 / Sonnet 5);
    // Sonnet 4.6 was the lone exception and its alias now points to Sonnet 5
    // (`harness-behavior.md` §3.2/§7.1). This is the drift guard: the redaction
    // is server-flag-gated and has moved before (it un-redacted Sonnet 4.6 at
    // 2.1.159, re-redacted at Sonnet 5), so a future flip back to un-redacted
    // reasoning trips the `no Thinking text` assertion and tells us to re-enable
    // rendering. A genuinely multi-step prompt (with `--effort high`) is needed
    // to reliably *engage* thinking; the answer stays tiny per cost discipline.
    let adapter = ClaudeCodeAdapter::new();
    let mut agent = live_agent();
    agent.model = Some("sonnet".to_owned());
    agent.effort = Some("high".to_owned());
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "ultrathink. Reason step by step whether 1000003 is prime, then reply \
             with only yes or no.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");
    let events: Vec<AdapterEvent> = stream.collect().await;

    // Thinking was engaged and streamed: a redacted block surfaces as `Liveness`
    // (the heartbeat that keeps a long thinking turn from falsely failing).
    let engaged_thinking = events
        .iter()
        .any(|e| matches!(e, AdapterEvent::Liveness { turn_id: t } if *t == turn_id));
    assert!(
        engaged_thinking,
        "expected the thinking block to stream as `Liveness` (redacted); got none. \
         If thinking stopped streaming entirely, re-probe (harness-behavior.md §3.2). \
         events: {events:?}"
    );

    // …but the prose is redacted: no un-redacted `Thinking` content chunk.
    let thinking_text: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk {
                kind: ContentKind::Thinking,
                text,
                ..
            } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        thinking_text.trim().is_empty(),
        "Sonnet now redacts thinking — expected no un-redacted `Thinking` prose, but \
         got some. The server redaction flag may have flipped back; re-probe per-model \
         (harness-behavior.md §3.2) and re-enable reasoning rendering. text: {thinking_text:?}"
    );
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_haiku_thinking_is_not_redacted() {
    // The only live coverage of the **un-redacted** thinking path.
    //
    // Thinking redaction is per-model and server-flag-gated: Opus 4.8 and
    // Sonnet 5 redact the `thinking` block to empty, Haiku 4.5 does not
    // (harness-behavior.md §3.2/§7.1). Three pieces of production code run
    // *only* when a model returns real reasoning prose — `parser.rs` mapping a
    // non-empty `thinking_delta` to `ContentKind::Thinking` (an empty one
    // becomes `Liveness`), `claude_code/session_file.rs` reconstructing a
    // non-empty `thinking` block on reopen, and the `ThinkingWidget` having
    // anything to render. Every other Claude live test runs against a redacting
    // model, so without this test those paths are exercised by unit tests and
    // fixtures alone, and a break in the live↔parser contract would ship green.
    //
    // It doubles as the Haiku drift guard, in the direction that fails
    // silently: if Haiku starts redacting, reasoning simply vanishes from the
    // UI with no error. This assertion turns that into a red test.
    //
    // Counterpart to `live_claude_sonnet_thinking_is_redacted`, which pins the
    // opposite behavior on the redacting models. Both must be re-probed
    // per-model on a CLI bump — the gate has moved four times.
    //
    // Sensitive to the model *choosing* to reason: `ultrathink` + `--effort
    // high` + a genuinely multi-step prompt reliably induce it (3/3 observed @
    // 2.1.205), but engagement is probabilistic, not contractual. A failure
    // here means either Haiku redacted (a real finding) or it answered without
    // thinking (re-run before believing it). The answer stays tiny per cost
    // discipline; Haiku is also the cheapest model.
    let adapter = ClaudeCodeAdapter::new();
    let mut agent = live_agent();
    agent.model = Some("haiku".to_owned());
    agent.effort = Some("high".to_owned());
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "ultrathink. Reason step by step whether 1000003 is prime, then reply \
             with only yes or no.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");
    let events: Vec<AdapterEvent> = stream.collect().await;

    let model = session_meta_model(&events).expect("Claude emits SessionMeta with model");
    assert!(
        model.contains("haiku"),
        "selected `--model haiku` must surface in SessionMeta.model; got {model:?}"
    );

    // The reasoning prose arrives as `Thinking` content chunks — the path that
    // exists only for a non-redacting model.
    let thinking_text: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk {
                kind: ContentKind::Thinking,
                text,
                ..
            } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !thinking_text.trim().is_empty(),
        "Haiku returned no un-redacted `Thinking` prose. Either the server redaction \
         flag now covers Haiku — re-probe per-model (harness-behavior.md §3.2/§7.1), \
         update the docs, and expect the ThinkingWidget to go blank for Haiku agents — \
         or the model answered without reasoning (re-run to distinguish). events: {events:?}"
    );

    let terminal = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("terminal TurnEnd");
    assert!(
        matches!(
            terminal,
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        ),
        "a thinking turn must still complete; got {terminal:?}"
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
        context_window_source,
        model,
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
    let occupancy_after_turn = usage
        .context_tokens_after_turn
        .expect("context_tokens_after_turn must be populated for a Claude turn");

    assert!(occupancy > 0, "occupancy must be non-zero, got 0");
    assert!(
        occupancy_after_turn >= occupancy,
        "post-turn occupancy must include the final parent call's output"
    );
    assert!(
        occupancy < summed_total,
        "occupancy ({occupancy}) must be the FINAL call's prompt size — strictly less than the \
         across-call sum `result.usage` reports ({summed_total}). Equal/greater means the adapter \
         regressed to using the summed total, which over-reports the context bar on tool-use turns."
    );
    let window = usage
        .context_window
        .expect("a normal Claude multi-call turn must report a context window");
    let ContextWindowSource::StreamOnly {
        model: selected_model,
    } = context_window_source
        .as_ref()
        .expect("a Claude context window must carry stream-only provenance")
    else {
        panic!("Claude context window must be stream-only");
    };
    assert_eq!(
        Some(selected_model),
        model.as_ref(),
        "context-window provenance must retain the resolved final assistant model"
    );
    assert!(
        occupancy_after_turn <= u64::from(window),
        "post-turn occupancy ({occupancy_after_turn}) must not exceed the context window ({window})"
    );
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
        session_home: None,
        model: None,
        effort: None,
        profiles: switchboard_core::AgentProfiles::default(),
        forked_from_session: None,
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
        session_home: None,
        model: None,
        effort: None,
        profiles: switchboard_core::AgentProfiles::default(),
        forked_from_session: None,
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

/// The same two-turn resume, but from a cwd whose name contains `_`.
///
/// `live_claude_resume_reuses_session` runs in `/tmp`, so it only exercises
/// paths made of `/` and alphanumerics — it passed for months while any agent
/// in a directory like `switchboard-mcp_oauth` was permanently stranded.
/// Claude Code collapses every non-alphanumeric character in the cwd (`_` and
/// spaces included) when naming its session-storage directory; if
/// Switchboard's `encode_cwd` disagrees, the adapter looks in a directory that
/// does not exist, concludes "first turn," and passes `--session-id` for a
/// session that already exists — which claude rejects with "Session ID … is
/// already in use" on every subsequent turn.
///
/// Pins the encoding against the real CLI, where a unit test can only pin it
/// against our own belief about the CLI.
#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_resume_reuses_session_in_underscored_cwd() {
    let adapter = ClaudeCodeAdapter::new();
    let session_id = Uuid::now_v7();

    // A *stable* path, not a fresh TempDir: claude creates a session directory
    // under the developer's real `~/.claude/projects/`, which this test cannot
    // clean up. A random cwd per run would leave a new directory behind every
    // time and clutter their own `claude --resume` picker. The `/tmp`-based
    // sibling test reuses one directory for the same reason.
    let cwd = std::env::temp_dir().join("sw_live_probe_underscored");
    std::fs::create_dir_all(&cwd).expect("create underscored cwd");
    let cwd = cwd.canonicalize().expect("canonicalize cwd");
    assert!(
        cwd.to_string_lossy().contains('_'),
        "the cwd must contain an underscore for this test to mean anything"
    );

    let agent = |name: &str| AgentRecord {
        session_home: None,
        model: None,
        effort: None,
        profiles: switchboard_core::AgentProfiles::default(),
        forked_from_session: None,
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: name.to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_locator: Some(SessionLocator::Uuid(session_id)),
        created_at: chrono::Utc::now(),
    };

    let completed = async |a: AgentRecord, prompt: &str| -> bool {
        let stream = adapter
            .dispatch(&a, &cwd, prompt, Uuid::now_v7(), DispatchOptions::default())
            .await
            .expect("dispatch should succeed");
        let events: Vec<AdapterEvent> = stream.collect().await;
        events.iter().any(|e| {
            matches!(
                e,
                AdapterEvent::TurnEnd {
                    outcome: TurnOutcome::Completed,
                    ..
                }
            )
        })
    };

    assert!(
        completed(agent("underscore-1"), "Say ACK").await,
        "first turn should create the session"
    );

    // Assert the encoding *directly*, not just via the second turn succeeding.
    // Without this the test infers correctness from claude currently rejecting
    // a reused `--session-id`; a future CLI that tolerated that would let this
    // go green with a wrong encoder — restoring the exact false confidence the
    // test exists to remove. This also fails with a diagnosable message.
    let expected = claude_session_file_path(&home_dir(), &cwd, &session_id);
    assert!(
        expected.exists(),
        "claude wrote its session somewhere other than {}; `encode_cwd` and the \
         CLI disagree about how to encode {}",
        expected.display(),
        cwd.display()
    );

    assert!(
        completed(agent("underscore-2"), "Say ACK again").await,
        "second turn must resume, not re-create: a `--session-id` here fails \
         with \"Session ID … is already in use\""
    );
}

/// Every keyed agent turn of `parent` must appear in `child` with identical
/// `hydration_key`, `started_at`, **and rendered content**. This is what the
/// imported-history rendering and the keyed merge stand on — a CLI bump that
/// dropped inherited replies, restamped copied records at fork time, or
/// regenerated per-turn ids would keep prompt-level assertions green while
/// breaking hydration. Content is compared too, because identity alone would
/// pass a fork that preserved record ids while emptying the replies.
/// (Raw-record lineage — `parentUuid` chains, `promptSource` — is asserted by
/// M3's raw fixture; the loader normalizes those away.)
fn assert_agent_identities_inherited(parent: &[Turn], child: &[Turn]) {
    /// (`key`, `started_at`) → the turn's concatenated text items, so a match
    /// asserts the reply survived rather than just its envelope.
    fn keyed_content(turns: &[Turn]) -> Vec<((String, chrono::DateTime<chrono::Utc>), String)> {
        turns
            .iter()
            .filter_map(|t| match t {
                Turn::Agent {
                    hydration_key: Some(key),
                    started_at,
                    items,
                    ..
                } => {
                    let text: String = items
                        .iter()
                        .filter_map(|i| match i {
                            TurnItem::Text { text, .. } => Some(text.clone()),
                            _ => None,
                        })
                        .collect();
                    Some(((key.clone(), *started_at), text))
                }
                _ => None,
            })
            .collect()
    }
    let parent_turns = keyed_content(parent);
    let child_turns = keyed_content(child);
    assert!(
        !parent_turns.is_empty(),
        "the seeded parent must have at least one keyed agent turn"
    );
    for (identity, text) in &parent_turns {
        let matched = child_turns
            .iter()
            .find(|(child_identity, _)| child_identity == identity);
        let (_, child_text) = matched.unwrap_or_else(|| {
            panic!(
                "the parent's agent turn {identity:?} must appear in the child with identical \
                 identity; child has: {:?}",
                child_turns.iter().map(|(i, _)| i).collect::<Vec<_>>()
            )
        });
        assert_eq!(
            child_text, text,
            "inherited agent turn {identity:?} must keep its content"
        );
    }
}

/// Assert a parent-seeding turn completed. Both fork tests seed a parent
/// session before exercising the fork; without this, an auth/quota failure on
/// the seed surfaces much later as `got: ""` at the BANANA assertion — the
/// worst place to start debugging a live test months from now.
fn assert_seed_completed(events: &[AdapterEvent]) {
    assert!(
        events.iter().any(|e| matches!(
            e,
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        )),
        "seeding the parent session did not complete (auth/quota?): {events:?}"
    );
}

/// Dispatch one live turn and return its events plus the concatenated streamed
/// text. The fork tests each run several turns; inlining the collect-and-filter
/// at every one of them is what pushed them past the line limit.
async fn fork_turn(
    adapter: &ClaudeCodeAdapter,
    agent: &AgentRecord,
    cwd: &Path,
    prompt: &str,
) -> (Vec<AdapterEvent>, String) {
    let stream = adapter
        .dispatch(
            agent,
            cwd,
            prompt,
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("live dispatch should succeed");
    let events: Vec<AdapterEvent> = stream.collect().await;
    let text = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    (events, text)
}

/// The whole fork contract against the real CLI: a forked agent's first
/// dispatch inherits the parent's context, lands on the session id **we**
/// pre-generated (not one Claude chose), leaves the parent's file untouched,
/// and writes a child file the transcript parser reads back as full inherited
/// history. A CLI bump that changed `--fork-session`'s interaction with
/// `--session-id`, or stopped copying prior turns into the child, would break
/// forking in ways no fixture test can see.
#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_fork_inherits_context_on_the_caller_assigned_session() {
    let adapter = ClaudeCodeAdapter::new();
    // Stable cwd, canonicalized — same reasoning as the sibling resume tests: a
    // fresh random directory per run would leave a new project dir behind in the
    // developer's real `~/.claude/projects/` every time. Canonicalizing is
    // load-bearing (macOS `temp_dir()` is a symlink): the path used for the
    // session-file lookup must be the one claude itself resolves.
    let cwd = std::env::temp_dir().join("sw_live_probe_fork");
    std::fs::create_dir_all(&cwd).expect("create fork probe cwd");
    let cwd = cwd.canonicalize().expect("canonicalize cwd");

    let parent_session = Uuid::now_v7();
    let parent = AgentRecord {
        session_locator: Some(SessionLocator::Uuid(parent_session)),
        ..live_agent()
    };
    let (seed_events, _) = fork_turn(
        &adapter,
        &parent,
        &cwd,
        "Remember: the secret word is BANANA. Reply with only the word ack.",
    )
    .await;
    assert_seed_completed(&seed_events);

    let parent_path = claude_session_file_path(&home_dir(), &cwd, &parent_session);
    let parent_before = std::fs::read(&parent_path).expect("parent session file");

    // The fork: its own pre-generated locator plus provenance pointing at the
    // parent's session. `build_args` turns that into
    // `--resume <parent> --session-id <own> --fork-session`.
    let fork_session = Uuid::now_v7();
    let fork = AgentRecord {
        session_locator: Some(SessionLocator::Uuid(fork_session)),
        forked_from_session: Some(parent_session),
        ..live_agent()
    };
    let (events, text) = fork_turn(
        &adapter,
        &fork,
        &cwd,
        "What is the secret word? Reply with only that word.",
    )
    .await;
    assert!(
        text.contains("BANANA"),
        "the fork must inherit the parent's context, got: {text:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        )),
        "the fork's first turn must complete"
    );

    // The branch landed on OUR id — this is what lets a fork pre-generate its
    // locator at registration instead of capturing one from the stream.
    assert!(
        claude_session_file_path(&home_dir(), &cwd, &fork_session).exists(),
        "the fork must write the session file we named, not one claude chose"
    );
    assert_eq!(
        std::fs::read(&parent_path).expect("parent session file"),
        parent_before,
        "forking must not modify the parent's session file"
    );

    // The parser reads the child back as inherited history, not just the new
    // turn — this is what makes the forked agent's transcript render.
    let loaded = load_claude_transcript(&home_dir(), &cwd, fork_session, fork.id)
        .expect("the forked session file must load");
    let prompts: Vec<String> = loaded
        .turns
        .iter()
        .filter_map(|t| match t {
            Turn::User { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        prompts.iter().any(|p| p.contains("secret word is BANANA")),
        "the parent's prompt must be present in the fork's transcript, got: {prompts:?}"
    );
    assert!(
        prompts
            .iter()
            .any(|p| p.contains("What is the secret word")),
        "the fork's own prompt must be present too, got: {prompts:?}"
    );

    // Inherited AGENT turns must survive with identity intact — see
    // `assert_agent_identities_inherited`.
    let parent_loaded = load_claude_transcript(&home_dir(), &cwd, parent_session, parent.id)
        .expect("the parent session file must load");
    assert_agent_identities_inherited(&parent_loaded.turns, &loaded.turns);

    // Second turn: provenance is still set, but the fork's own file now exists,
    // so this must be a plain resume — re-forking here would discard the fork's
    // own history on every send.
    fork_turn(&adapter, &fork, &cwd, "Reply with only the word ack2.").await;
    let after = load_claude_transcript(&home_dir(), &cwd, fork_session, fork.id)
        .expect("the forked session file must still load");
    let after_prompts = after
        .turns
        .iter()
        .filter(|t| matches!(t, Turn::User { .. }))
        .count();
    assert!(
        after_prompts > prompts.len(),
        "the second turn must append to the fork's own session, not re-fork it \
         (prompts before: {}, after: {after_prompts})",
        prompts.len()
    );
}

/// Cancelling a fork's **first** dispatch must never strand the agent. Because
/// fork-vs-resume is derived from whether the fork's own session file exists,
/// recovery must hold for whichever state the cancel leaves behind: file
/// absent → the next send re-forks; file present → it resumes the copy.
/// (Probing found only those two states — never a partial file — but that is a
/// bounded observation at one CLI version, not a guarantee; see
/// harness-behavior.md §3.5. The recovery invariant asserted here is
/// state-independent, and the deterministic per-state arg coverage lives in
/// the `build_args` unit tests. Which state a given run exercises depends on
/// timing; with a tiny parent it cannot be the size-sensitive copy window the
/// probe explored at 160 KB.)
///
/// The cancelled turn's prompt is deliberately long-running — an exception to
/// AGENTS.md's tiny-response cost discipline, because the test needs a window
/// in which the cancel can actually land.
#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_fork_recovers_from_a_cancelled_first_dispatch() {
    let adapter = ClaudeCodeAdapter::new();
    // Stable, canonicalized cwd — see the sibling fork test.
    let cwd = std::env::temp_dir().join("sw_live_probe_fork_cancel");
    std::fs::create_dir_all(&cwd).expect("create fork-cancel probe cwd");
    let cwd = cwd.canonicalize().expect("canonicalize cwd");

    let parent_session = Uuid::now_v7();
    let parent = AgentRecord {
        session_locator: Some(SessionLocator::Uuid(parent_session)),
        ..live_agent()
    };
    let (seed_events, _) = fork_turn(
        &adapter,
        &parent,
        &cwd,
        "Remember: the secret word is BANANA. Reply with only the word ack.",
    )
    .await;
    assert_seed_completed(&seed_events);

    let fork_session = Uuid::now_v7();
    let fork = AgentRecord {
        session_locator: Some(SessionLocator::Uuid(fork_session)),
        forked_from_session: Some(parent_session),
        ..live_agent()
    };

    // Cancel the fork's first dispatch mid-flight, exactly as the dispatcher's
    // cancel path does (the token kills the subprocess group).
    let token = tokio_util::sync::CancellationToken::new();
    let stream = adapter
        .dispatch(
            &fork,
            &cwd,
            "Count slowly from 1 to 50, one number per line.",
            Uuid::now_v7(),
            DispatchOptions {
                cancel_token: token.clone(),
                ..DispatchOptions::default()
            },
        )
        .await
        .expect("the fork's first dispatch should spawn");
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        token.cancel();
    });
    let cancelled_events: Vec<AdapterEvent> = stream.collect().await;
    canceller.await.expect("canceller task");

    // The cancel must have genuinely interrupted the turn. The adapter's
    // cancel contract is to end the stream WITHOUT a terminal event (the
    // dispatcher synthesizes the Cancelled terminal) — so any `TurnEnd` here
    // means claude finished inside the sleep and this run proved nothing the
    // happy-path test doesn't. Fail loudly rather than pass vacuously; if
    // models get fast enough to finish 50 lines in 3 s, widen the prompt.
    assert!(
        !cancelled_events
            .iter()
            .any(|e| matches!(e, AdapterEvent::TurnEnd { .. })),
        "the first dispatch completed before the cancel landed — the test \
         exercised nothing; widen the prompt. Events: {cancelled_events:?}"
    );

    // Whichever state the cancel left behind, the next send must produce a fork
    // that knows the parent's context — re-forking if the file never appeared,
    // resuming the completed copy if it did.
    let (recovery_events, text) = fork_turn(
        &adapter,
        &fork,
        &cwd,
        "What is the secret word? Reply with only that word.",
    )
    .await;
    assert!(
        text.contains("BANANA"),
        "a cancelled first dispatch must not cost the fork its inherited context, got: {text:?}"
    );
    // Streaming the right text isn't recovery on its own — the turn must also
    // reach a Completed terminal, or the fork is still broken in a way the
    // content assertion alone would miss.
    assert!(
        recovery_events.iter().any(|e| matches!(
            e,
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        )),
        "the recovery turn must complete: {recovery_events:?}"
    );
    assert!(
        claude_session_file_path(&home_dir(), &cwd, &fork_session).exists(),
        "the fork must end up on its own pre-generated session id"
    );
}

/// The backend contract the staleness refresh stands on: continuing a session
/// (a second turn appended to the file, exactly as a TUI continuation does)
/// makes a re-read return the new turn with a **new, distinct** `hydration_key`
/// while the first turn's key stays **identical** — so the frontend keyed merge
/// adds the new turn exactly once and never re-duplicates the existing one. A
/// CLI bump that reused an id across turns, or changed an existing turn's id on
/// resume, would silently break refresh (drop or duplicate turns); this catches
/// it live.
#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_refresh_picks_up_appended_turn() {
    let adapter = ClaudeCodeAdapter::new();
    let session_id = Uuid::now_v7();
    let agent_id = Uuid::now_v7();
    let agent = AgentRecord {
        session_home: None,
        model: None,
        effort: None,
        profiles: switchboard_core::AgentProfiles::default(),
        forked_from_session: None,
        id: agent_id,
        project_id: Uuid::now_v7(),
        name: "refresh-test".to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_locator: Some(SessionLocator::Uuid(session_id)),
        created_at: chrono::Utc::now(),
    };

    let _: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "Say ACK",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("first dispatch should succeed")
        .collect()
        .await;
    let after_one = load_claude_transcript(&home_dir(), Path::new("/tmp"), session_id, agent_id)
        .expect("hydrate after the first turn");
    let keys_one = agent_hydration_keys(&after_one);
    assert_eq!(keys_one.len(), 1, "one agent turn after the first dispatch");

    // Second turn resumes the same session → appends to the same file, like a TUI
    // continuation the refresh exists to pick up.
    let _: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "Say ACK again",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("second dispatch should succeed")
        .collect()
        .await;
    let after_two = load_claude_transcript(&home_dir(), Path::new("/tmp"), session_id, agent_id)
        .expect("hydrate after the second turn");
    let keys_two = agent_hydration_keys(&after_two);

    assert_eq!(
        keys_two.len(),
        2,
        "two agent turns after the second dispatch"
    );
    assert_eq!(
        keys_two[0], keys_one[0],
        "the first turn's key is unchanged across the re-read (no spurious dup on refresh)"
    );
    assert_ne!(
        keys_two[0], keys_two[1],
        "the appended turn carries a new, distinct key (merge adds it exactly once)"
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

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live-claude"]
async fn live_claude_unescaped_plugin_is_still_a_zero_turn_synthetic_command() {
    // Negative control for the adapter workaround below. If Claude stops
    // intercepting a bare slash-leading positional, this test fails and tells us
    // the transport-only space may no longer be necessary.
    let mut command = tokio::process::Command::new("claude");
    command
        .args([
            "-p",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "--verbose",
            "--dangerously-skip-permissions",
            "--session-id",
            &Uuid::now_v7().to_string(),
            "--",
            "/plugin",
        ])
        .current_dir("/tmp")
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(30), command.output())
        .await
        .expect("bare /plugin probe timed out")
        .expect("claude should run");
    assert!(
        output.status.success(),
        "bare /plugin probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert!(records.iter().any(|record| {
        record.get("type").and_then(serde_json::Value::as_str) == Some("assistant")
            && record
                .pointer("/message/model")
                .and_then(serde_json::Value::as_str)
                == Some("<synthetic>")
            && record
                .pointer("/message/content")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|blocks| {
                    blocks.iter().any(|block| {
                        block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                            && block
                                .get("text")
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|text| !text.is_empty())
                    })
                })
    }));
    assert!(records.iter().any(|record| {
        record.get("type").and_then(serde_json::Value::as_str) == Some("result")
            && record.get("num_turns").and_then(serde_json::Value::as_u64) == Some(0)
            && record.get("is_error").and_then(serde_json::Value::as_bool) == Some(false)
            && record
                .get("modelUsage")
                .and_then(serde_json::Value::as_object)
                .is_some_and(serde_json::Map::is_empty)
    }));
    assert!(!records.iter().any(|record| {
        record.get("type").and_then(serde_json::Value::as_str) == Some("assistant")
            && record
                .pointer("/message/model")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|model| model != "<synthetic>")
    }));
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live-claude"]
async fn live_claude_slash_leading_prompt_reaches_model() {
    // Claude 2.1.223 routes a bare `/plugin` positional through its local command
    // parser and emits a zero-token `<synthetic>` response. The adapter's single
    // transport-only leading space must keep an ordinary Switchboard message on
    // the model path. This is a real-CLI drift guard because fixture tests can
    // prove our argv shape but not Anthropic's parsing behavior.
    let adapter = ClaudeCodeAdapter::new();
    let agent = live_agent();
    let events: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "/plugin",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with a slash-leading prompt")
        .collect()
        .await;

    let text: String = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::ContentChunk { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !text.trim().is_empty(),
        "bare slash-leading prompt must produce a model response"
    );
    assert!(matches!(
        events
            .iter()
            .find(|event| matches!(event, AdapterEvent::TurnEnd { .. })),
        Some(AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Completed,
            model: Some(_),
            ..
        })
    ));
}

// --- Codex live tests ---

fn live_codex_agent() -> AgentRecord {
    AgentRecord {
        session_home: None,
        model: None,
        effort: None,
        profiles: switchboard_core::AgentProfiles::default(),
        forked_from_session: None,
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
    // - TurnEnd.usage token fields are replaced by the session file's
    //   token_count.info.last_token_usage (the stream's turn.completed.usage
    //   is thread-cumulative). The live parser stamps context_input_tokens
    //   None, so a Some here proves the real CLI still writes the
    //   last_token_usage shape the overlay depends on — drift guard.
    // - RateLimitEvent fires every turn from token_count.rate_limits.
    // - SessionMeta fires on the first turn carrying model + cli_version +
    //   the merged MCP servers / skills registries.
    match terminal {
        AdapterEvent::TurnEnd { usage: Some(u), .. } => {
            assert!(
                u.context_window.is_some(),
                "TurnEnd.usage.context_window must be enriched from session file (got None)"
            );
            let occupancy = u
                .context_input_tokens
                .expect("per-turn usage overlay must fill context_input_tokens from the session file's last_token_usage");
            assert!(
                occupancy > 0 && occupancy <= u64::from(u.context_window.unwrap()),
                "occupancy must be a sane fraction of the window, got {occupancy} of {:?}",
                u.context_window
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

    // Rate-limit payload-shape drift detection. The ordering
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
async fn live_codex_model_and_effort_dispatch() {
    // `-m <model>` is plan-gated (only the account's entitled models are
    // accepted), so we pin `gpt-5.6-luna` — the cheapest current-generation
    // model — rather than switching models; the across-turns *effort*
    // assertion lives elsewhere. Here we prove the flags are accepted
    // end-to-end (dispatch completes, model surfaces in SessionMeta) — a
    // rejected `-m`/`-c` would 400 and fail the turn.
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = CodexAdapter::new();
    let mut agent = live_codex_agent();
    agent.model = Some("gpt-5.6-luna".to_owned());
    agent.effort = Some("high".to_owned());
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

    let terminal = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("terminal TurnEnd");
    assert!(
        matches!(
            terminal,
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        ),
        "dispatch with model+effort must complete; got {terminal:?}"
    );
    let model = session_meta_model(&events).expect("Codex emits SessionMeta with model on turn 1");
    assert!(
        model.contains("luna"),
        "selected `-m gpt-5.6-luna` must surface in SessionMeta.model; got {model:?}"
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
        model: None,
        effort: None,
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

// --- Antigravity live tests ---

fn live_antigravity_agent() -> AgentRecord {
    AgentRecord {
        session_home: None,
        model: None,
        effort: None,
        profiles: switchboard_core::AgentProfiles::default(),
        forked_from_session: None,
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

/// Antigravity's sporadic server-side turn failure, matched **exactly**.
///
/// Measured out-of-band @ agy 1.1.19 (2026-08-23) at roughly 3–5% of turns,
/// independent of concurrency: 12/12 sequential turns clean, 6/6 at two-way,
/// 8/8 at eight-way, and 2 failures across 36 turns at four-way. It is
/// upstream and not something the adapter can prevent — production correctly
/// surfaces it verbatim as a `HarnessError` and the user re-sends.
const ANTIGRAVITY_TRANSIENT_ERROR: &str = "Agent execution terminated due to error.";

/// The same failure, as `agy` formats it on **stderr** in text mode. Adopting
/// `--output-format stream-json` moved this error onto stdout inside the
/// `result` event, where it arrives *unprefixed* — so both spellings are live
/// depending on which channel reported it, and a predicate keyed to only one
/// silently stops retrying. (That is exactly what happened when the stream
/// landed: the retry went quiet until a live run surfaced it. The exact-match
/// discipline is what made that loud instead of invisible.)
fn is_antigravity_transient_message(message: &str) -> bool {
    let message = message.trim();
    message == ANTIGRAVITY_TRANSIENT_ERROR
        || message
            .strip_prefix("Error: ")
            .is_some_and(|rest| rest == ANTIGRAVITY_TRANSIENT_ERROR)
}

/// Whether a turn's events are **exactly** that transient failure and nothing
/// else.
///
/// Three conditions, each closing a way a real failure could be retried away:
/// the stream carries exactly one terminal event (a double-`TurnEnd` is itself
/// a defect and must not be retried past), that terminal is a `HarnessError`
/// (so an `AuthFailure` or `AdapterFailure` carrying similar text never
/// matches), and its message equals the known string outright — not a
/// substring, so a compound message or a future rewording fails the test
/// instead of being absorbed.
fn is_antigravity_transient(events: &[AdapterEvent]) -> bool {
    let mut terminals = events.iter().filter_map(|e| match e {
        AdapterEvent::TurnEnd { outcome, .. } => Some(outcome),
        _ => None,
    });
    let (Some(outcome), None) = (terminals.next(), terminals.next()) else {
        return false;
    };
    matches!(
        outcome,
        TurnOutcome::Failed {
            kind: FailureKind::HarnessError,
            message,
        } if is_antigravity_transient_message(message)
    )
}

/// Whether a direct `agy` run's stderr is **solely** the known transient.
///
/// Fail-closed on purpose. An earlier draft accepted "the transient is present
/// and no other line starts with `Error:`", which had a real hole: `agy`'s
/// auth line (`Authentication required…`) is not `Error:`-prefixed, so a
/// transient-plus-auth run would have been retried — masking the one failure
/// we least want hidden. Requiring the transient to be the only non-empty line
/// closes that without enumerating which other diagnostics are dangerous.
///
/// The cost is that unrelated benign chatter would stop the retry firing. That
/// is the safe direction to fail: a spurious red run is visible and fixed by
/// re-running, whereas a masked failure is silent. Every transient observed
/// while characterizing this (agy 1.1.19) wrote exactly this one line. If a
/// harmless preamble is ever observed, model that shape explicitly rather than
/// widening this to accept arbitrary company.
fn stderr_is_solely_antigravity_transient(stderr: &str) -> bool {
    let mut lines = stderr.lines().map(str::trim).filter(|l| !l.is_empty());
    let (Some(only), None) = (lines.next(), lines.next()) else {
        return false;
    };
    is_antigravity_transient_message(only)
}

/// Whether a direct `agy --output-format stream-json` run failed with **only**
/// the known transient.
///
/// Fail-closed for the same reason as its stderr sibling, and held to the same
/// standard: exactly one terminal `result`, its error exactly the transient,
/// and no unparseable stdout line alongside it. A compound stream — the
/// transient plus a second terminal, or plus a diagnostic we cannot read — is
/// precisely the shape a real wire-format regression would take, and this
/// guard protects a drift tripwire, so retrying past that would blind the one
/// test whose whole job is to notice.
fn stdout_is_solely_antigravity_transient(stdout: &str) -> bool {
    let mut results = Vec::new();
    for line in stdout.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            // Unreadable output next to the transient: refuse to retry.
            return false;
        };
        if event["event"] == "result" {
            results.push(event);
        }
    }
    let [result] = results.as_slice() else {
        return false;
    };
    result["result"]["error"]
        .as_str()
        .is_some_and(is_antigravity_transient_message)
}

/// Whether a live dispatch may be retried after the known transient.
///
/// Default is [`RetryPolicy::Never`]: a caller that does not think about it
/// gets no retry, so forgetting costs a visible red run rather than a silently
/// duplicated request. Opting in is an assertion by the test author that
/// re-running this exact prompt is harmless — no dependence on prior
/// conversation state, and no side effect outside a disposable workspace that
/// running twice would corrupt.
#[derive(Clone, Copy, PartialEq)]
enum RetryPolicy {
    /// The prompt is idempotent and its workspace disposable.
    RetryOnKnownTransient,
    Never,
}

/// Dispatch one live Antigravity turn, retrying **once** when the caller opted
/// in and the turn came back as exactly [`ANTIGRAVITY_TRANSIENT_ERROR`].
///
/// **Why a retry exists at all.** The measured ~4% per-turn transient rate left
/// a suite this size at roughly even odds of a red run, and at that frequency
/// "just re-run it" becomes the habit — which is how a real regression gets
/// waved through. (This session's first suite run had exactly one failure, and
/// it was the genuine 1.1.19 tool-record regression.)
///
/// **Why it is opt-in rather than automatic.** Retrying re-sends a prompt that
/// may already have taken effect. A resume appends the request to a
/// conversation that already received it; even a first turn can run tools
/// before failing. Neither is undone by the remote conversation being
/// abandoned. So safety is a property of the *prompt*, which only the test
/// author knows — hence an explicit argument, defaulting to no retry.
///
/// **A resume is never retried, whatever the caller passes.** An Antigravity
/// record carries a `session_locator` only once a real conversation UUID has
/// been captured, so its presence is exactly "this dispatch resumes". Blocking
/// it here rather than trusting call sites makes the unsafe case
/// unrepresentable instead of merely discouraged.
///
/// It is **test-only**: production keeps surfacing this verbatim, because
/// auto-retrying real turns is a product decision that also spends the user's
/// quota. It retries **once**, and prints whenever it fires — or declines to —
/// so a rising failure rate stays visible instead of being absorbed.
async fn antigravity_live_turn(
    adapter: &AntigravityAdapter,
    agent: &AgentRecord,
    cwd: &Path,
    prompt: &str,
    turn_id: Uuid,
    policy: RetryPolicy,
) -> Vec<AdapterEvent> {
    let retryable = policy == RetryPolicy::RetryOnKnownTransient && agent.session_locator.is_none();
    for attempt in 1..=2 {
        let stream = adapter
            .dispatch(agent, cwd, prompt, turn_id, DispatchOptions::default())
            .await
            .expect("dispatch should succeed with real agy");
        let events: Vec<AdapterEvent> = stream.collect().await;
        if !is_antigravity_transient(&events) || attempt == 2 {
            return events;
        }
        if !retryable {
            eprintln!(
                "live_antigravity: upstream transient ({ANTIGRAVITY_TRANSIENT_ERROR}) NOT retried                  — this dispatch is a resume or opted out; re-run the suite"
            );
            return events;
        }
        eprintln!(
            "live_antigravity: retrying once after upstream transient ({ANTIGRAVITY_TRANSIENT_ERROR})"
        );
    }
    unreachable!("loop returns on the final attempt")
}

/// Guards the retry's exact-match discipline. Not `#[ignore]`d: it is a pure
/// predicate test and should run in `make test`, because the live path that
/// would otherwise exercise it fires on only ~4% of turns — far too rare to
/// catch a regression here. The failure this prevents is someone loosening
/// `==` to `contains` (or dropping the kind check) and thereby retrying away
/// genuine failures.
#[test]
fn antigravity_transient_matcher_is_exact() {
    fn failed(kind: FailureKind, message: &str) -> Vec<AdapterEvent> {
        vec![AdapterEvent::TurnEnd {
            turn_id: Uuid::now_v7(),
            outcome: TurnOutcome::Failed {
                kind,
                message: message.to_owned(),
            },
            ended_at: chrono::Utc::now(),
            usage: None,
            context_window_source: None,
            spend: None,
            model: None,
            effort: None,
            stable_message_id: None,
            first_message_id: None,
        }]
    }

    // Both spellings are live: unprefixed from the stream's `result.error`,
    // prefixed from agy's text-mode stderr formatting.
    assert!(is_antigravity_transient(&failed(
        FailureKind::HarnessError,
        ANTIGRAVITY_TRANSIENT_ERROR
    )));
    assert!(is_antigravity_transient(&failed(
        FailureKind::HarnessError,
        &format!("Error: {ANTIGRAVITY_TRANSIENT_ERROR}")
    )));

    // Near-misses that must NOT be retried away.
    for (kind, message) in [
        // A superstring — the substring temptation.
        (
            FailureKind::HarnessError,
            "Error: Agent execution terminated due to error. Details: quota",
        ),
        // Right text, wrong classification.
        (FailureKind::AdapterFailure, ANTIGRAVITY_TRANSIENT_ERROR),
        (FailureKind::AuthFailure, ANTIGRAVITY_TRANSIENT_ERROR),
        // Any other harness error, including a reworded future variant.
        (
            FailureKind::HarnessError,
            "Error: agent execution terminated due to an error",
        ),
        (
            FailureKind::HarnessError,
            "Error: timeout waiting for response",
        ),
    ] {
        let label = format!("{kind:?}");
        assert!(
            !is_antigravity_transient(&failed(kind, message)),
            "must not treat {message:?} ({label}) as the known transient"
        );
    }

    // A second terminal event makes the stream malformed; that is a defect in
    // its own right and must not be retried past, even though one of the two
    // terminals matches.
    let mut double = failed(FailureKind::HarnessError, ANTIGRAVITY_TRANSIENT_ERROR);
    double.extend(failed(FailureKind::HarnessError, "Error: something else"));
    assert!(!is_antigravity_transient(&double));

    // A non-terminal event alongside the transient is irrelevant — only the
    // terminal count is load-bearing.
    let mut with_chunk = vec![AdapterEvent::ContentChunk {
        turn_id: Uuid::now_v7(),
        kind: ContentKind::Text,
        text: "partial".to_owned(),
    }];
    with_chunk.extend(failed(
        FailureKind::HarnessError,
        ANTIGRAVITY_TRANSIENT_ERROR,
    ));
    assert!(is_antigravity_transient(&with_chunk));

    // A completed turn is never a retry candidate.
    assert!(!is_antigravity_transient(&[AdapterEvent::TurnEnd {
        turn_id: Uuid::now_v7(),
        outcome: TurnOutcome::Completed,
        ended_at: chrono::Utc::now(),
        usage: None,
        context_window_source: None,
        spend: None,
        model: None,
        effort: None,
        stable_message_id: None,
        first_message_id: None,
    }]));
    assert!(!is_antigravity_transient(&[]));
}

/// The direct-CLI retry criterion, held to the same exactness as the
/// event-side one. Also not `#[ignore]`d — same reasoning.
#[test]
fn stderr_transient_matcher_requires_the_transient_alone() {
    assert!(stderr_is_solely_antigravity_transient(
        ANTIGRAVITY_TRANSIENT_ERROR
    ));
    // Surrounding blank lines are not diagnostics.
    assert!(stderr_is_solely_antigravity_transient(&format!(
        "\n{ANTIGRAVITY_TRANSIENT_ERROR}\n\n"
    )));

    for stderr in [
        // A doubled prefix is not one of the two accepted spellings.
        &format!("Error: Error: {ANTIGRAVITY_TRANSIENT_ERROR}") as &str,
        // The hole this predicate exists to close: agy's auth line is not
        // `Error:`-prefixed, so an "any other Error: line" rule would have
        // retried this and hidden a login failure.
        &format!(
            "Authentication required. Please visit the URL to log in:\n{ANTIGRAVITY_TRANSIENT_ERROR}"
        ) as &str,
        // A compound failure whose second cause would be masked.
        &format!("{ANTIGRAVITY_TRANSIENT_ERROR}\nError: timeout waiting for response"),
        // Fails closed on unrecognized company rather than guessing it benign.
        &format!("some unrelated notice\n{ANTIGRAVITY_TRANSIENT_ERROR}"),
        // Near-miss text.
        "Error: agent execution terminated due to an error",
        "",
    ] {
        assert!(
            !stderr_is_solely_antigravity_transient(stderr),
            "must not retry on stderr: {stderr:?}"
        );
    }
}

/// The stream-side retry criterion, held to the same exactness as the stderr
/// one. Not `#[ignore]`d — the live path exercises it on only a few percent of
/// turns, far too rare to catch a regression here.
#[test]
fn stdout_transient_matcher_requires_a_single_clean_result() {
    let transient =
        |e: &str| format!(r#"{{"event":"result","result":{{"status":"ERROR","error":"{e}"}}}}"#);
    let init = r#"{"event":"init","conversation_id":"5a8dd0c7-3450-4048-a5fb-27ae8f663dee"}"#;

    // Ordinary shape: init, steps, one terminal carrying only the transient.
    assert!(stdout_is_solely_antigravity_transient(&format!(
        "{init}
{}
",
        transient(ANTIGRAVITY_TRANSIENT_ERROR)
    )));

    for stdout in [
        // Two terminals — malformed, and one of them would be masked.
        format!(
            "{}
{}",
            transient(ANTIGRAVITY_TRANSIENT_ERROR),
            transient("something else entirely")
        ),
        // Unreadable output beside the transient: refuse rather than guess.
        format!(
            "{}
not json at all",
            transient(ANTIGRAVITY_TRANSIENT_ERROR)
        ),
        // A different error.
        transient("timeout waiting for response"),
        // No terminal at all.
        init.to_owned(),
        String::new(),
    ] {
        assert!(
            !stdout_is_solely_antigravity_transient(&stdout),
            "must not retry on stdout: {stdout:?}"
        );
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

    let events = antigravity_live_turn(
        &adapter,
        &agent,
        tmp.path(),
        "Reply with the single word 'ack' and nothing else.",
        turn_id,
        // Fresh conversation, no tools, disposable tempdir.
        RetryPolicy::RetryOnKnownTransient,
    )
    .await;

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
    // Codex live discipline).
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
    // Same one-shot retry as `antigravity_live_turn`, and the same narrow
    // criterion — this test spawns `agy` directly rather than going through the
    // adapter, so it needs its own copy of the guard. stderr is captured (not
    // null) purely so the transient can be matched exactly; any other non-zero
    // exit still fails on the first attempt.
    let mut output = None;
    for attempt in 1..=2 {
        let run = tokio::process::Command::new("agy")
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
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .expect("agy should spawn");
        let transient = !run.status.success()
            && stderr_is_solely_antigravity_transient(&String::from_utf8_lossy(&run.stderr));
        if attempt == 2 || !transient {
            output = Some(run);
            break;
        }
        eprintln!(
            "live_antigravity: retrying once after upstream transient ({ANTIGRAVITY_TRANSIENT_ERROR})"
        );
    }
    let output = output.expect("loop assigns on the final attempt");
    assert!(
        output.status.success(),
        "agy exited non-zero: {:?}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );

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

    let events = antigravity_live_turn(
        &adapter,
        &agent,
        tmp.path(),
        prompt,
        turn_id,
        RetryPolicy::RetryOnKnownTransient,
    )
    .await;

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
    let events1 = antigravity_live_turn(
        &adapter,
        &agent,
        tmp.path(),
        "Remember the word 'mango'. Reply with only 'ok'.",
        turn1,
        // Turn 1 opens a fresh conversation and calls no tools.
        RetryPolicy::RetryOnKnownTransient,
    )
    .await;

    // Simulate the dispatcher: fold the captured conversation UUID onto the
    // agent so the next dispatch resumes via the registry-stored locator (the
    // production factory live-reads it from `agents_by_id`).
    let conversation_id =
        antigravity_capture(&events1).expect("first dispatch emits a captured Antigravity locator");
    let resumed_agent = AgentRecord {
        model: None,
        effort: None,
        session_locator: Some(SessionLocator::Uuid(conversation_id)),
        ..agent.clone()
    };

    let turn2 = Uuid::now_v7();
    let events2 = antigravity_live_turn(
        &adapter,
        &resumed_agent,
        tmp.path(),
        "What word did I ask you to remember? Reply with only that word.",
        turn2,
        // Turn 2 resumes: re-sending would append the request to a
        // conversation that already received it. (The helper also blocks
        // this structurally; stated here so the call site reads honestly.)
        RetryPolicy::Never,
    )
    .await;
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
    // value (so the adapter passes the prompt unchanged), unlike claude/
    // codex. This guards that assumption — if a CLI bump makes `agy` dash-
    // sensitive, this fails and the adapter needs the same treatment.
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::new();
    let agent = live_antigravity_agent();
    let turn_id = Uuid::now_v7();

    let events = antigravity_live_turn(
        &adapter,
        &agent,
        tmp.path(),
        "- Reply with the single word 'ack' and nothing else.",
        turn_id,
        // Fresh conversation, no tools, disposable tempdir.
        RetryPolicy::RetryOnKnownTransient,
    )
    .await;

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

/// Drift tripwire for the **step→call invariant** the tool pairing now rests
/// on: a call announced by a planner record at step `P` has its result at
/// `P + 1 + call_index`.
///
/// This is an inferred property of agy's undocumented internal step numbering,
/// derived from ten planner records across five captured transcripts. Fixtures
/// freeze that inference; only a real run can notice Google changing it. If it
/// ever shifts, tool results start attaching to the wrong call — silently, and
/// with plausible-looking output — so this is worth a live turn.
///
/// Uses a multi-tool prompt because a single-call turn cannot distinguish
/// `P + 1 + call_index` from a simple "next step" rule.
#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_tool_result_steps_follow_the_call_index() {
    // Runs the scenario up to twice. See `MULTI_TOOL_PROMPT` for why one
    // attempt isn't enough and `is_benign_merge` for why the retry predicate
    // must *prove* the benign case rather than infer it.
    for attempt in 1..=2 {
        let (outcome, content) = run_multi_tool_step_probe().await;
        let (expected, observed) = step_pairs(&content);

        if expected.len() < 2 {
            if attempt == 1 && is_benign_merge(&outcome, &content) {
                eprintln!(
                    "live_antigravity: model merged both jobs into one run_command; \
                     retrying once to exercise the multi-call invariant"
                );
                continue;
            }
            panic!(
                "multi-tool precondition not exercised after retry \
                 (expected >= 2 tool calls, saw {}; outcome {outcome:?}); transcript: {content}",
                expected.len()
            );
        }

        assert_eq!(
            observed, expected,
            "tool result steps must be `planner_step + 1 + call_index`; if this fails, \
             agy's step numbering changed and tool results will attach to the wrong call. \
             transcript: {content}"
        );
        return;
    }
    unreachable!("loop returns or panics on the final attempt")
}

/// Whether a single-tool-call turn is the **known benign non-exercise**: the
/// model completed the turn but did both requested jobs inside one shell call.
///
/// This must be *proven*, not inferred from what's missing. G32 means agy
/// writes **no transcript record at all** for a tool call it rejects — so
/// "the model merged the two jobs" and "the file-reading tool failed and left
/// no trace" produce a byte-identical transcript: one `run_command` entry and
/// nothing else. Retrying on that shape alone would let this test absorb the
/// exact class of upstream regression it exists to catch.
///
/// So the bar is positive evidence on three counts: the turn `Completed`, the
/// sole call is `run_command`, and its `CommandLine` contains **both**
/// sentinels — the echo token and the filename it was told not to read from the
/// shell. Anything else fails immediately, unretried.
fn is_benign_merge(outcome: &TurnOutcome, content: &str) -> bool {
    if !matches!(outcome, TurnOutcome::Completed) {
        return false;
    }
    let mut calls = planner_tool_calls(content);
    let (Some(call), None) = (calls.next(), calls.next()) else {
        return false;
    };
    if call["name"].as_str() != Some("run_command") {
        return false;
    }
    let command_line = call["args"]["CommandLine"].as_str().unwrap_or_default();
    command_line.contains(STEP_PROBE_ECHO_TOKEN) && command_line.contains(STEP_PROBE_FILE)
}

/// Every tool call announced by a planner record, in file order.
fn planner_tool_calls(content: &str) -> impl Iterator<Item = serde_json::Value> + '_ {
    transcript_records(content)
        .filter(|r| r["type"] == "PLANNER_RESPONSE")
        .flat_map(|r| {
            r["tool_calls"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
        })
}

/// Sentinels the merged-shell-command classifier looks for. Distinct strings so
/// neither can satisfy the other's check.
const STEP_PROBE_ECHO_TOKEN: &str = "STEP_PROBE_ONE";
const STEP_PROBE_FILE: &str = "STEP_PROBE.txt";

/// Guards `is_benign_merge`'s discipline. **Not `#[ignore]`d**: this classifier
/// decides whether the live drift alarm is allowed to re-arm, so it must not be
/// exercised only by the live path it guards — the same reasoning that keeps
/// `antigravity_transient_matcher_is_exact` in the default suite.
///
/// The rejection cases are the point. Under G32 a failed tool leaves no
/// transcript record, so a genuine upstream breakage is *shape-identical* to a
/// benign merge; only the positive evidence below separates them.
#[test]
fn benign_merge_requires_positive_evidence_of_both_jobs() {
    fn planner(tool_calls: &serde_json::Value) -> String {
        json!({
            "step_index": 2, "source": "MODEL", "type": "PLANNER_RESPONSE",
            "status": "DONE", "tool_calls": tool_calls,
        })
        .to_string()
    }
    fn shell(command_line: &str) -> serde_json::Value {
        json!([{"name": "run_command", "args": {"CommandLine": command_line}}])
    }

    let merged = planner(&shell("echo STEP_PROBE_ONE; cat STEP_PROBE.txt"));
    assert!(
        is_benign_merge(&TurnOutcome::Completed, &merged),
        "one shell call that demonstrably did both jobs is the benign case"
    );

    // The regression this exists to catch: the model ran the echo and never
    // read the file. Identical shape to a merge, minus the proof.
    assert!(
        !is_benign_merge(
            &TurnOutcome::Completed,
            &planner(&shell("echo STEP_PROBE_ONE"))
        ),
        "a command missing the file sentinel is an omitted read, not a merge"
    );
    assert!(
        !is_benign_merge(
            &TurnOutcome::Completed,
            &planner(&shell("cat STEP_PROBE.txt"))
        ),
        "a command missing the echo sentinel is not a merge either"
    );

    // A lone non-shell call could mean the shell tool itself broke.
    let lone_read =
        planner(&json!([{"name": "view_file", "args": {"AbsolutePath": "STEP_PROBE.txt"}}]));
    assert!(!is_benign_merge(&TurnOutcome::Completed, &lone_read));

    // The tool-name check is load-bearing on its own, not merely implied by the
    // sentinel check: an unfamiliar tool that happens to carry both sentinels in
    // a `CommandLine` arg is a shape we do not understand, and "we do not
    // understand this" must never resolve to "benign."
    let unknown_tool = planner(&json!([{
        "name": "some_future_tool",
        "args": {"CommandLine": "echo STEP_PROBE_ONE; cat STEP_PROBE.txt"},
    }]));
    assert!(
        !is_benign_merge(&TurnOutcome::Completed, &unknown_tool),
        "only run_command is a recognized merge; anything else fails loud"
    );

    // A turn that didn't complete is never classified — the shortfall may be
    // the failure itself, not a model choice.
    assert!(
        !is_benign_merge(
            &TurnOutcome::Failed {
                kind: FailureKind::HarnessError,
                message: "boom".to_owned(),
            },
            &merged
        ),
        "an incomplete turn must never be treated as a benign merge"
    );

    // Two real calls aren't a merge at all; the caller never asks, but the
    // classifier must not claim it.
    let two_calls = planner(&json!([
        {"name": "run_command", "args": {"CommandLine": "echo STEP_PROBE_ONE"}},
        {"name": "view_file", "args": {"AbsolutePath": "STEP_PROBE.txt"}},
    ]));
    assert!(!is_benign_merge(&TurnOutcome::Completed, &two_calls));

    assert!(!is_benign_merge(&TurnOutcome::Completed, ""));
}

/// Asks for one shell command and one file read, each as its own tool call.
///
/// **This does not guarantee two calls, and must not be described as if it
/// did.** `agy` has no flag to restrict which tools the model may use
/// (checked @ 1.1.20: only `--dangerously-skip-permissions` and
/// `--disable-slash-commands` exist), so the model can always satisfy both
/// jobs with a single `run_command` running `echo …; cat STEP_PROBE.txt` — the
/// same merge that made an earlier "two shell commands" version flaky. Naming
/// the tools and forbidding the shell read makes the merge unlikely, not
/// impossible; the caller retries once and then fails loudly.
///
/// The deterministic proof of the step invariant lives in the fixture tests
/// (`tool-failure-cross-planner`, `tool-vocabulary`). This live test exists
/// only to catch the real CLI renumbering its steps.
const MULTI_TOOL_PROMPT: &str = "Do exactly two things, using a separate tool call for each. \
     First, use your shell-command tool to run: echo STEP_PROBE_ONE \
     Second, use your file-reading tool to read the file STEP_PROBE.txt in the workspace \
     directory. Do not read that file with the shell. \
     Then reply with only the word done.";

/// Dispatch one multi-tool probe turn; return its terminal outcome and raw
/// transcript. The outcome is load-bearing — `is_benign_merge` refuses to
/// classify a turn it can't first confirm completed.
async fn run_multi_tool_step_probe() -> (TurnOutcome, String) {
    // Non-hidden prefix: see the note in `tests/tool_use.rs` — `tempfile`'s
    // default is `.tmp`, and `agy` historically refused a hidden workspace.
    let tmp = tempfile::Builder::new()
        .prefix("agy-step-probe")
        .tempdir()
        .expect("tempdir");
    std::fs::write(tmp.path().join("STEP_PROBE.txt"), "step probe fixture\n")
        .expect("seed the file the second tool call reads");
    let adapter = AntigravityAdapter::new();
    let agent = live_antigravity_agent();

    let events = antigravity_live_turn(
        &adapter,
        &agent,
        tmp.path(),
        MULTI_TOOL_PROMPT,
        Uuid::now_v7(),
        // Fresh conversation; an idempotent echo plus a read of a file we
        // seeded ourselves, both inside a tempdir.
        RetryPolicy::RetryOnKnownTransient,
    )
    .await;

    let outcome = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::TurnEnd { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .expect("a TurnEnd");
    let conversation = antigravity_capture(&events).expect("captured conversation id");
    let transcript =
        switchboard_harness::antigravity::paths::transcript_path(&home_dir(), conversation);
    let content = std::fs::read_to_string(&transcript)
        .unwrap_or_else(|e| panic!("reading {}: {e}", transcript.display()));
    (outcome, content)
}

/// From a raw transcript: the steps results *should* land on (one per tool call,
/// at `planner_step + 1 + call_index`) and the steps they *did* land on.
fn step_pairs(content: &str) -> (Vec<i64>, Vec<i64>) {
    let mut expected: Vec<i64> = Vec::new();
    let mut observed: Vec<i64> = Vec::new();
    for record in transcript_records(content) {
        if record["source"] != "MODEL" {
            continue;
        }
        let step = record["step_index"].as_i64().unwrap_or_default();
        if record["type"] == "PLANNER_RESPONSE" {
            if let Some(calls) = record["tool_calls"].as_array() {
                for index in 0..calls.len() {
                    expected.push(step + 1 + i64::try_from(index).unwrap());
                }
            }
        } else {
            observed.push(step);
        }
    }
    expected.sort_unstable();
    observed.sort_unstable();
    (expected, observed)
}

fn transcript_records(content: &str) -> impl Iterator<Item = serde_json::Value> + '_ {
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
}

/// Drift tripwire for the `stream-json` contract, which is now the adapter's
/// **primary** conversation-id capture and terminal signal. A fixture replays
/// our own recorded shape and so cannot catch Google reshaping the payload;
/// only a real run can. Asserts the three event types exist, that `init`
/// carries a parseable conversation id, and that `result` reports a status —
/// the fields classification depends on.
///
/// Runs `agy` directly rather than through the adapter because the adapter
/// deliberately consumes only part of the stream; this guards the wire format
/// itself.
#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_stream_json_init_and_result_shapes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let log = tmp.path().join("agy.log");
    let mut output = None;
    for attempt in 1..=2 {
        let run = tokio::process::Command::new("agy")
            .args([
                "-p",
                "Reply with the single word 'ack' and nothing else.",
                "--output-format",
                "stream-json",
                "--disable-slash-commands",
                "--dangerously-skip-permissions",
                "--log-file",
            ])
            .arg(&log)
            .current_dir(tmp.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .expect("agy should spawn");
        // Under stream-json the transient is reported inside the `result`
        // event on **stdout**, not as a stderr line — so this run detects it
        // there. The text-mode sibling below keeps the stderr predicate.
        let transient = !run.status.success()
            && stdout_is_solely_antigravity_transient(&String::from_utf8_lossy(&run.stdout));
        if attempt == 2 || !transient {
            output = Some(run);
            break;
        }
        eprintln!(
            "live_antigravity: retrying once after upstream transient ({ANTIGRAVITY_TRANSIENT_ERROR})"
        );
    }
    let output = output.expect("loop assigns on the final attempt");
    assert!(
        output.status.success(),
        "agy exited non-zero: {:?}; stdout: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect();
    assert!(
        !events.is_empty(),
        "stream-json produced no parseable NDJSON; stdout: {stdout:?}"
    );

    let init = events
        .iter()
        .find(|e| e["event"] == "init")
        .unwrap_or_else(|| panic!("no `init` event; got: {events:?}"));
    let conversation_id = init["conversation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("`init` carries no conversation_id: {init}"));
    assert!(
        Uuid::parse_str(conversation_id).is_ok(),
        "`init.conversation_id` must parse as a UUID; got {conversation_id:?}"
    );

    assert!(
        events
            .iter()
            .any(|e| e["event"] == "step_update" && e["step_update"]["step_type"].is_string()),
        "no `step_update` carrying a `step_type`; got: {events:?}"
    );

    let result = events
        .iter()
        .find(|e| e["event"] == "result")
        .unwrap_or_else(|| panic!("no terminal `result` event; got: {events:?}"));
    assert_eq!(
        result["result"]["status"].as_str(),
        Some("SUCCESS"),
        "expected a SUCCESS status on a trivial turn; got {result}"
    );
}

/// Drift tripwire for the two dispatch-armoring guards in `build_args`, run
/// against the real CLI because both live entirely in `agy`'s own argument and
/// print-mode handling — a fixture replays our arg shape and cannot catch
/// upstream changes to either rule.
///
/// - **Slash interception** (agy 1.1.9-1.1.12): a recognized slash command is
///   answered locally, spends no quota, and creates **no conversation** - which
///   the adapter cannot capture, so the turn fails. `--disable-slash-commands`
///   must keep the text a message.
/// - **Flag-token prompts** (agy 1.1.18): a `-p` value that is exactly a known
///   flag exits 2 before any model call. The transport space must clear it.
///
/// A regression on either shows up here as a failed turn, not a wrong answer,
/// so the assertions are about completion + a live response rather than the
/// model's exact words.
#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_slash_and_flag_shaped_prompts_reach_the_model() {
    for prompt in [
        // Recognized command: bare `-p "/model"` prints a TSV row and mints no
        // conversation. With the flag it must reach the model instead.
        "/model - ignore that leading text and reply with the single word 'ack'",
        // Exactly a known flag token, the shape agy 1.1.18 rejects outright.
        "--sandbox",
    ] {
        let tmp = tempfile::TempDir::new().unwrap();
        let adapter = AntigravityAdapter::new();
        let agent = live_antigravity_agent();
        let turn_id = Uuid::now_v7();

        let events = antigravity_live_turn(
            &adapter,
            &agent,
            tmp.path(),
            prompt,
            turn_id,
            RetryPolicy::RetryOnKnownTransient,
        )
        .await;

        let terminal = events
            .iter()
            .find(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
            .unwrap_or_else(|| panic!("no TurnEnd for {prompt:?}; got: {events:?}"));
        assert!(
            matches!(
                terminal,
                AdapterEvent::TurnEnd {
                    outcome: TurnOutcome::Completed,
                    ..
                }
            ),
            "{prompt:?} must reach the model and complete, got: {terminal:?}"
        );

        // A completed turn already requires a terminal transcript answer, but
        // assert non-empty text directly: a locally-answered slash command
        // produces stdout with no transcript answer, and this states that
        // distinction rather than leaning on the classifier for it.
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                AdapterEvent::ContentChunk { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(
            !text.trim().is_empty(),
            "{prompt:?} must produce a model response; got: {text:?}"
        );
    }
}

// --- Per-turn model/effort across a mid-conversation switch ---
//
// Each asserts on BOTH the emitted `TurnEnd` (the live carrier) AND a real-file
// hydration via `load_*_transcript` — because the live carrier and the hydrator
// read different sources for some harnesses (Claude live = stream
// `init`/`SessionMeta`; hydrate = on-disk per-record model), so only asserting
// the live path would let on-disk format drift ship undetected.

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_model_and_effort_change_across_turns() {
    // Turn 1 on `sonnet` is a *tool-use* turn (Bash echo → ≥2 model calls),
    // exercising per-turn model attribution on a multi-call turn. Turn 2 resumes
    // on `opus`. Claude exposes **no** per-turn effort, so `TurnEnd.effort` is
    // always `None` — we still pass `--effort` to confirm it doesn't break the
    // turn, but assert only the model switch (live + hydrate).
    let adapter = ClaudeCodeAdapter::new();
    let cwd = tempfile::TempDir::new().unwrap();
    let session_id = Uuid::now_v7();
    let agent_id = Uuid::now_v7();
    let mut agent = AgentRecord {
        session_home: None,
        model: Some("sonnet".to_owned()),
        effort: Some("low".to_owned()),
        profiles: switchboard_core::AgentProfiles::default(),
        forked_from_session: None,
        id: agent_id,
        project_id: Uuid::now_v7(),
        name: "m4-claude".to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_locator: Some(SessionLocator::Uuid(session_id)),
        created_at: chrono::Utc::now(),
    };

    let events1: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            cwd.path(),
            "Use the Bash tool to run `echo hi`, then reply with only the word: done.",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch 1")
        .collect()
        .await;
    // The multi-call assertion is only meaningful if a tool actually ran. If
    // this fails, the prompt no longer triggers a tool call — fix the prompt,
    // don't weaken the assertion.
    assert!(
        events1
            .iter()
            .any(|e| matches!(e, AdapterEvent::ToolStarted { .. })),
        "turn 1 must trigger a tool call (≥2 model calls); events: {events1:?}"
    );
    let (m1, _) = turn_end_model_effort(&events1).expect("turn 1 TurnEnd");
    assert!(
        m1.as_deref().is_some_and(|m| m.contains("sonnet")),
        "turn 1 per-turn model = sonnet; got {m1:?}"
    );

    agent.model = Some("opus".to_owned());
    agent.effort = Some("high".to_owned());
    let events2: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            cwd.path(),
            "Reply with only the word: ok.",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch 2")
        .collect()
        .await;
    let (m2, _) = turn_end_model_effort(&events2).expect("turn 2 TurnEnd");
    assert!(
        m2.as_deref().is_some_and(|m| m.contains("opus")),
        "turn 2 per-turn model = opus; got {m2:?}"
    );

    // Hydration (reopen) must reconstruct the same per-turn switch from disk.
    let hydrated =
        load_claude_transcript(&home_dir(), cwd.path(), session_id, agent_id).expect("hydrate");
    let models = hydrated_turn_models(&hydrated);
    assert_eq!(models.len(), 2, "two agent turns on disk; got {models:?}");
    assert!(
        models[0].as_deref().is_some_and(|m| m.contains("sonnet")),
        "hydrated turn 1 = sonnet; got {:?}",
        models[0]
    );
    assert!(
        models[1].as_deref().is_some_and(|m| m.contains("opus")),
        "hydrated turn 2 = opus; got {:?}",
        models[1]
    );
}

#[tokio::test]
#[ignore = "requires codex installed — run with: make test-live"]
async fn live_codex_model_and_effort_change_across_turns() {
    // Codex models are plan-gated, so we pin the cheapest current-generation
    // model (`gpt-5.6-luna`) and vary *effort* `medium`→`high`
    // (the readback field is `turn_context.effort`). Asserts the per-turn effort
    // switch on the emitted `TurnEnd` AND on a real-file hydration.
    let cwd = tempfile::TempDir::new().unwrap();
    let adapter = CodexAdapter::new();
    let agent_id = Uuid::now_v7();
    let mut agent = live_codex_agent();
    agent.id = agent_id;
    agent.model = Some("gpt-5.6-luna".to_owned());
    agent.effort = Some("medium".to_owned());

    let events1: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            cwd.path(),
            "Reply with the single word 'ack'.",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch 1")
        .collect()
        .await;
    let (_, e1) = turn_end_model_effort(&events1).expect("turn 1 TurnEnd");
    assert_eq!(
        e1.as_deref(),
        Some("medium"),
        "turn 1 per-turn effort; got {e1:?}"
    );
    let (thread_id, date) = codex_capture(&events1).expect("captured Codex locator");

    agent.session_locator = Some(SessionLocator::Codex {
        thread_id: thread_id.clone(),
        partition_date: date,
    });
    agent.effort = Some("high".to_owned());
    let events2: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            cwd.path(),
            "Reply with the single word 'ack'.",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch 2")
        .collect()
        .await;
    let (_, e2) = turn_end_model_effort(&events2).expect("turn 2 TurnEnd");
    assert_eq!(
        e2.as_deref(),
        Some("high"),
        "turn 2 per-turn effort; got {e2:?}"
    );

    let hydrated = load_codex_transcript(&home_dir(), cwd.path(), &thread_id, Some(date), agent_id)
        .expect("hydrate");
    let efforts: Vec<_> = hydrated
        .turns
        .iter()
        .filter_map(|t| match t {
            Turn::Agent { effort, .. } => Some(effort.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        efforts,
        vec![Some("medium".to_owned()), Some("high".to_owned())],
        "per-turn effort on disk; got {efforts:?}"
    );
}

/// Codex durable send↔turn key parity. The key on the live `TurnEnd`
/// (`first_message_id`, sourced from the enrichment re-read of
/// `turn_context.turn_id`) must equal the `hydration_key` the session-file parser
/// reconstructs for the *same* turn — that equality is what makes the M2 `TurnLink`
/// correlate the right turn. Because both come from the same on-disk field it should
/// hold by construction; this proves the per-turn mirroring (reset at `task_started`)
/// is implemented correctly and guards it against Codex CLI drift. Two turns so it
/// also proves the key **varies per turn** (no stale-key carryover).
#[tokio::test]
#[ignore = "requires codex installed — run with: make test-live-codex"]
async fn live_codex_hydration_key_matches_live_turn_end() {
    let cwd = tempfile::TempDir::new().unwrap();
    let adapter = CodexAdapter::new();
    let agent_id = Uuid::now_v7();
    let mut agent = live_codex_agent();
    agent.id = agent_id;

    let events1: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            cwd.path(),
            "Reply with the single word 'ack'.",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch 1")
        .collect()
        .await;
    let live_key_1 = turn_end_first_message_id(&events1).expect(
        "Codex TurnEnd must carry a first_message_id (turn_context.turn_id via enrichment)",
    );
    let (thread_id, date) = codex_capture(&events1).expect("captured Codex locator");

    agent.session_locator = Some(SessionLocator::Codex {
        thread_id: thread_id.clone(),
        partition_date: date,
    });
    let events2: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            cwd.path(),
            "Reply with the single word 'ack'.",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch 2")
        .collect()
        .await;
    let live_key_2 = turn_end_first_message_id(&events2).expect("turn 2 first_message_id");

    let hydrated = load_codex_transcript(&home_dir(), cwd.path(), &thread_id, Some(date), agent_id)
        .expect("hydrate");
    let disk_keys = agent_hydration_keys(&hydrated);

    assert_eq!(
        disk_keys,
        vec![live_key_1.clone(), live_key_2.clone()],
        "each turn's live TurnEnd key must equal the parser's hydration_key, in order"
    );
    assert_ne!(
        live_key_1, live_key_2,
        "the per-turn key must vary across turns — a constant key would mean stale carryover"
    );
}

/// Restores `settings.json` from a byte-for-byte backup and deletes the backup
/// on drop — covers normal completion AND assertion-panic unwinding. See the
/// config-mutation protocol in `live_antigravity_model_change_announced_on_resume`.
struct AntigravitySettingsGuard {
    path: std::path::PathBuf,
    backup: std::path::PathBuf,
}

impl Drop for AntigravitySettingsGuard {
    fn drop(&mut self) {
        if !self.backup.exists() {
            return;
        }
        // Delete the backup ONLY after the restore is verified to have
        // succeeded — otherwise a failed copy would leave settings.json mutated
        // *and* destroy the pristine copy. On failure, keep the backup and shout
        // both paths so the next run's self-heal (or the developer) can recover.
        match std::fs::copy(&self.backup, &self.path) {
            Ok(_) => {
                let _ = std::fs::remove_file(&self.backup);
            }
            Err(e) => eprintln!(
                "FAILED to restore {} from backup ({e}); backup KEPT at {} — \
                 restore it manually (cp {} {}).",
                self.path.display(),
                self.backup.display(),
                self.backup.display(),
                self.path.display()
            ),
        }
    }
}

#[tokio::test]
#[ignore = "requires antigravity installed — run with: make test-live"]
#[serial_test::serial]
async fn live_antigravity_model_change_announced_on_resume() {
    // Antigravity's per-turn model history is a *carry-forward* of the
    // `USER_SETTINGS_CHANGE` sentence, which `agy` emits only when the model
    // changes. We can't set the model via a flag, so we change the **global**
    // `~/.gemini/antigravity-cli/settings.json` `model` field between turns and
    // assert our adapter's emitted per-turn model reflects the switch.
    //
    // Config-mutation safety protocol (this edits the real, harness-owned
    // settings.json — the only workable home, since an isolated HOME re-prompts
    // agy for OAuth): (1) byte-for-byte backup to a stable path; (2) self-heal —
    // if the backup already exists from an interrupted run, restore from it and
    // fail loud; (3) a `Drop` guard restores + deletes the backup on normal exit
    // AND on assertion-panic unwind; (4) known gap — SIGKILL/crash bypasses Drop,
    // but step 2 repairs on the next run and the pristine value survives in the
    // backup; (5) `#[serial]` because the file is global. Mutating harness config
    // *in a test* doesn't violate the production rule that the app never writes
    // harness config — it's the only way to exercise the real contract.
    let home = home_dir();
    let settings = home.join(".gemini/antigravity-cli/settings.json");
    let backup = home.join(".gemini/antigravity-cli/settings.json.switchboard-test-backup");
    if !settings.exists() {
        eprintln!("skipping: {settings:?} not present (Antigravity not configured)");
        return;
    }
    // Self-heal from an interrupted prior run before doing anything else: if a
    // backup exists, the previous run died with settings.json possibly still
    // mutated — restore from the pristine backup, then fail loud.
    if backup.exists() {
        match std::fs::copy(&backup, &settings) {
            Ok(_) => {
                let _ = std::fs::remove_file(&backup);
                panic!(
                    "self-healed: a prior run was interrupted; restored {settings:?} from its \
                     backup and removed the backup. Re-run."
                );
            }
            Err(e) => panic!(
                "a prior run was interrupted AND the self-heal restore failed ({e}). \
                 The pristine value is in {backup:?}; restore it manually \
                 (cp {backup:?} {settings:?}) and delete the backup, then re-run."
            ),
        }
    }

    let original = std::fs::read(&settings).expect("read settings.json");
    std::fs::write(&backup, &original).expect("write backup");
    let _guard = AntigravitySettingsGuard {
        path: settings.clone(),
        backup: backup.clone(),
    };

    let set_model = |display_name: &str| {
        let mut v: serde_json::Value =
            serde_json::from_slice(&original).expect("settings.json is JSON");
        v["model"] = serde_json::Value::String(display_name.to_owned());
        std::fs::write(&settings, serde_json::to_vec_pretty(&v).unwrap()).expect("write settings");
    };

    let cwd = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::new();
    let agent_id = Uuid::now_v7();
    let mut agent = AgentRecord {
        session_home: None,
        model: None,
        effort: None,
        profiles: switchboard_core::AgentProfiles::default(),
        forked_from_session: None,
        id: agent_id,
        project_id: Uuid::now_v7(),
        name: "m4-agy".to_owned(),
        harness: HarnessKind::Antigravity,
        session_locator: None,
        created_at: chrono::Utc::now(),
    };

    // Turn 1 on model X.
    set_model("Gemini 3.1 Pro (High)");
    let events1 = antigravity_live_turn(
        &adapter,
        &agent,
        cwd.path(),
        "Reply with only the word ack.",
        Uuid::now_v7(),
        // Turn 1 opens a fresh conversation and calls no tools.
        RetryPolicy::RetryOnKnownTransient,
    )
    .await;
    let (m1, _) = turn_end_model_effort(&events1).expect("turn 1 TurnEnd");
    assert!(
        m1.as_deref().is_some_and(|m| m.contains("Gemini 3.1 Pro")),
        "turn 1 per-turn model = Gemini 3.1 Pro; got {m1:?}"
    );
    let conversation_id = antigravity_capture(&events1).expect("captured conversation id");

    // Turn 2: switch the global model to Y, resume.
    agent.session_locator = Some(SessionLocator::Uuid(conversation_id));
    set_model("Claude Sonnet 4.6 (Thinking)");
    let events2 = antigravity_live_turn(
        &adapter,
        &agent,
        cwd.path(),
        "Reply with only the word ack.",
        Uuid::now_v7(),
        // Turn 2 resumes, and this test hydrates the transcript to assert
        // per-turn model order — a duplicated turn would corrupt exactly what
        // it measures. So this dispatch stays exposed to the upstream
        // transient by design; a red run here is re-run, not retried.
        RetryPolicy::Never,
    )
    .await;
    let (m2, _) = turn_end_model_effort(&events2).expect("turn 2 TurnEnd");
    assert!(
        m2.as_deref()
            .is_some_and(|m| m.contains("Claude Sonnet 4.6")),
        "turn 2 per-turn model = Claude Sonnet 4.6; got {m2:?}"
    );

    // Hydration reconstructs the carry-forward from the transcript.
    let hydrated = load_antigravity_transcript(&home, cwd.path(), Some(conversation_id), agent_id)
        .expect("hydrate");
    let models = hydrated_turn_models(&hydrated);
    assert!(
        models
            .first()
            .and_then(Option::as_deref)
            .is_some_and(|m| m.contains("Gemini 3.1 Pro")),
        "hydrated turn 1 = Gemini 3.1 Pro; got {models:?}"
    );
    assert!(
        models
            .last()
            .and_then(Option::as_deref)
            .is_some_and(|m| m.contains("Claude Sonnet 4.6")),
        "hydrated last turn = Claude Sonnet 4.6; got {models:?}"
    );
}

// ---------------------------------------------------------------------------
// Tool-facet drift guards (harness-behavior §3.6). These are the tests that
// notice when a CLI vendor changes its tool wire shapes: the fixture-driven
// facet tests prove the classifiers handle the *recorded* shapes; these prove
// those shapes still arrive from the current CLI. Cost note: the edit-driving
// tests deliberately sit above the one-word-reply discipline (~cents each) —
// the model must genuinely edit files for the facet to exist.
// ---------------------------------------------------------------------------

/// `(tool_use_id, name, facet)` for every live `ToolStarted`.
fn tool_started_facets(events: &[AdapterEvent]) -> Vec<(String, String, ToolFacet)> {
    events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ToolStarted {
                tool_use_id,
                name,
                facet,
                ..
            } => Some((tool_use_id.clone(), name.clone(), facet.clone())),
            _ => None,
        })
        .collect()
}

/// `(tool_use_id, facet)` for every hydrated tool item.
fn hydrated_tool_facets(t: &switchboard_harness::LoadedTranscript) -> Vec<(String, ToolFacet)> {
    t.turns
        .iter()
        .filter_map(|turn| match turn {
            Turn::Agent { items, .. } => Some(items.iter().filter_map(|i| match i {
                TurnItem::Tool {
                    tool_use_id, facet, ..
                } => Some((tool_use_id.clone(), facet.clone())),
                _ => None,
            })),
            _ => None,
        })
        .flatten()
        .collect()
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_edit_emits_edit_facet() {
    // One turn exercises Read/Edit/Write/Bash so a single dispatch covers
    // four facet mappings; Grep is deliberately not requested (not every
    // environment exposes it in the default toolset).
    let cwd = tempfile::TempDir::new().unwrap();
    std::fs::write(cwd.path().join("alpha.txt"), "foo\n").unwrap();
    let adapter = ClaudeCodeAdapter::new();
    let agent = live_agent();
    let Some(switchboard_core::SessionLocator::Uuid(session_id)) = agent.session_locator else {
        panic!("live_agent carries a Uuid locator");
    };

    let events: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            cwd.path(),
            "In the current directory, do these steps using exactly the named tool for each: \
             1) Use the Read tool to read alpha.txt. \
             2) Use the Edit tool to change foo to bar in alpha.txt. \
             3) Use the Write tool to create epsilon.txt containing exactly: hello world \
             4) Use the Bash tool to run: ls \
             Then reply with the single word done.",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch")
        .collect()
        .await;

    let live = tool_started_facets(&events);
    let facet_of = |tool: &str| -> &ToolFacet {
        &live
            .iter()
            .find(|(_, n, _)| n == tool)
            .unwrap_or_else(|| {
                panic!(
                    "expected a {tool} call; got tools {:?}",
                    live.iter().map(|(_, n, _)| n).collect::<Vec<_>>()
                )
            })
            .2
    };
    let ToolFacet::Edit { files } = facet_of("Edit") else {
        panic!("Edit must classify as an Edit facet — Claude's Edit input shape drifted");
    };
    assert_eq!(files[0].edits[0].old, "foo");
    assert_eq!(files[0].edits[0].new, "bar");
    assert!(matches!(facet_of("Read"), ToolFacet::Read { .. }));
    assert!(matches!(facet_of("Write"), ToolFacet::Write { .. }));
    assert!(matches!(facet_of("Bash"), ToolFacet::Shell { .. }));

    // Two-call-site equivalence against the real session file: same
    // tool_use_id ⇒ same facet.
    let hydrated =
        load_claude_transcript(&home_dir(), cwd.path(), session_id, agent.id).expect("hydrate");
    let disk = hydrated_tool_facets(&hydrated);
    let mut compared = 0;
    for (id, name, live_facet) in &live {
        if let Some((_, disk_facet)) = disk.iter().find(|(did, _)| did == id) {
            assert_eq!(live_facet, disk_facet, "facet divergence for {name} ({id})");
            compared += 1;
        }
    }
    assert!(
        compared >= 4,
        "expected >=4 shared tool calls, compared {compared}"
    );
}

#[tokio::test]
#[ignore = "requires codex installed — run with: make test-live"]
async fn live_codex_apply_patch_emits_edit_facet() {
    // The headline drift guard: Codex's edit split (live `file_change`
    // paths-only → turn-end `ToolFacetUpdated` with patch content → disk
    // `apply_patch` custom_tool_call) is the most shape-dependent path in
    // the facet design. Also covers the Shell facet in the same turn.
    let cwd = tempfile::TempDir::new().unwrap();
    std::fs::write(cwd.path().join("alpha.txt"), "foo\n").unwrap();
    let adapter = CodexAdapter::new();
    let agent = live_codex_agent();

    let events: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            cwd.path(),
            "Edit the file alpha.txt in the current directory, changing the word foo to bar. \
             Then run the shell command: ls. Then reply with the single word done.",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch")
        .collect()
        .await;

    let live = tool_started_facets(&events);
    // Live edit announcement: paths + kind, no content.
    let (edit_id, _, live_edit) = live
        .iter()
        .find(|(_, n, _)| n == "file_change")
        .expect("Codex must announce the edit as a live file_change item — shape drifted");
    let ToolFacet::Edit { files: live_files } = live_edit else {
        panic!("file_change must classify as Edit");
    };
    assert!(
        live_files
            .iter()
            .any(|f| f.path.ends_with("alpha.txt") && matches!(f.change, EditChange::Modified)),
        "live edit facet must name alpha.txt as modified: {live_files:?}"
    );
    assert!(
        live.iter()
            .any(|(_, _, f)| matches!(f, ToolFacet::Shell { .. }))
    );

    // The turn-end upgrade: content-bearing facet for the same row, emitted
    // before TurnEnd (the dispatcher drops turn-scoped events post-terminal).
    let upgrade_idx = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::ToolFacetUpdated { tool_use_id, .. } if tool_use_id == edit_id))
        .expect("the live edit row must receive a ToolFacetUpdated from the enrichment read");
    let turn_end_idx = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("TurnEnd");
    assert!(upgrade_idx < turn_end_idx, "upgrade must precede TurnEnd");
    let AdapterEvent::ToolFacetUpdated {
        facet: upgraded, ..
    } = &events[upgrade_idx]
    else {
        unreachable!();
    };
    let ToolFacet::Edit {
        files: upgraded_files,
    } = upgraded
    else {
        panic!("upgrade must carry an Edit facet");
    };
    assert!(
        upgraded_files.iter().any(|f| !f.edits.is_empty()),
        "the upgraded facet must carry before/after content"
    );

    // Disk side: the reload parser reconstructs the same content-bearing
    // facet from the apply_patch record.
    let (thread_id, date) = codex_capture(&events).expect("captured Codex locator");
    let hydrated = load_codex_transcript(&home_dir(), cwd.path(), &thread_id, Some(date), agent.id)
        .expect("hydrate");
    let disk_edit = hydrated_tool_facets(&hydrated)
        .into_iter()
        .find_map(|(_, f)| match f {
            ToolFacet::Edit { files } if files.iter().any(|x| !x.edits.is_empty()) => Some(files),
            _ => None,
        })
        .expect("reloaded transcript must carry a content-bearing Edit facet (apply_patch)");
    assert_eq!(
        &disk_edit, upgraded_files,
        "the upgraded live facet must equal the reload parser's facet"
    );
}

#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
async fn live_antigravity_run_command_emits_shell_facet() {
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = AntigravityAdapter::new();
    let agent = live_antigravity_agent();

    let events = antigravity_live_turn(
        &adapter,
        &agent,
        tmp.path(),
        // `echo` (not `ls`) — the model satisfies "list the directory" with its
        // dedicated `list_dir` tool, which never exercises the Shell mapping.
        "Run the shell command: echo switchboard-facet-probe. Then reply with the single word done.",
        Uuid::now_v7(),
        // Fresh conversation. This prompt does run a tool, but `echo` is
        // idempotent and the workspace is a discarded tempdir, so a second
        // attempt cannot leave different state behind.
        RetryPolicy::RetryOnKnownTransient,
    )
    .await;

    let live = tool_started_facets(&events);
    let shell = live
        .iter()
        .find_map(|(_, n, f)| match f {
            ToolFacet::Shell { command, .. } if n == "run_command" => Some(command.clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "run_command must classify as a Shell facet with a decoded CommandLine; observed tool calls: {:?}",
                live.iter().map(|(_, n, f)| (n.clone(), f.clone())).collect::<Vec<_>>()
            )
        });
    assert!(
        !shell.is_empty() && !shell.starts_with('\"'),
        "CommandLine must decode transcript.jsonl's string-encoding, got {shell:?}"
    );
}
