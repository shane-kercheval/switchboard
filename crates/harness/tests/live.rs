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
    DispatchOptions, EditChange, GeminiAdapter, HarnessAdapter, RateLimitSource, ToolFacet, Turn,
    TurnItem, TurnOutcome, UserPromptSource, claude_session_file_path, load_antigravity_transcript,
    load_claude_transcript, load_codex_transcript, load_gemini_transcript,
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
        model: None,
        effort: None,
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
    // effect end-to-end), and dispatching with an effort set completes without
    // error. Per-turn *effort* exposure is asserted elsewhere — here the effort
    // contract is the build_args unit test plus "dispatch succeeds."
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
        model: None,
        effort: None,
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
        model: None,
        effort: None,
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
        model: None,
        effort: None,
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

// --- Codex live tests ---

fn live_codex_agent() -> AgentRecord {
    AgentRecord {
        model: None,
        effort: None,
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
    // `-m <model>` is plan-gated (only the account's entitled model is
    // accepted), so we pin the entitled `gpt-5.5` rather than switching models;
    // the across-turns *effort* assertion lives elsewhere. Here we prove the flags
    // are accepted end-to-end (dispatch completes, model surfaces in
    // SessionMeta) — a rejected `-m`/`-c` would 400 and fail the turn.
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = CodexAdapter::new();
    let mut agent = live_codex_agent();
    agent.model = Some("gpt-5.5".to_owned());
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
        model.contains("gpt-5"),
        "selected `-m gpt-5.5` must surface in SessionMeta.model; got {model:?}"
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

// --- Gemini live tests ---

fn live_gemini_agent() -> AgentRecord {
    AgentRecord {
        model: None,
        effort: None,
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
async fn live_gemini_model_dispatch() {
    // The selected model surfaces in `SessionMeta` (from `init.model`), proving
    // `--model` took effect. Gemini has no effort axis, so there is nothing
    // effort-shaped to assert.
    let tmp = tempfile::TempDir::new().unwrap();
    let adapter = GeminiAdapter::new();
    let mut agent = live_gemini_agent();
    agent.model = Some("gemini-2.5-flash".to_owned());
    let turn_id = Uuid::now_v7();

    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "Reply with only the number 4 and nothing else.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real gemini");
    let events: Vec<AdapterEvent> = stream.collect().await;

    let model = session_meta_model(&events).expect("Gemini emits SessionMeta with model");
    assert!(
        model.contains("flash"),
        "selected `--model gemini-2.5-flash` must surface in SessionMeta.model; got {model:?}"
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
        "dispatch with model must complete; got {terminal:?}"
    );
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
        model: None,
        effort: None,
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
        model: None,
        effort: None,
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

// --- Per-turn model/effort across a mid-conversation switch ---
//
// Each asserts on BOTH the emitted `TurnEnd` (the live carrier) AND a real-file
// hydration via `load_*_transcript` — because the live carrier and the hydrator
// read different sources for some harnesses (Gemini/Claude live = stream
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
        model: Some("sonnet".to_owned()),
        effort: Some("low".to_owned()),
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
    // Codex model is plan-gated to `gpt-5.5`, so we vary *effort* `medium`→`high`
    // (the readback field is `turn_context.effort`). Asserts the per-turn effort
    // switch on the emitted `TurnEnd` AND on a real-file hydration.
    let cwd = tempfile::TempDir::new().unwrap();
    let adapter = CodexAdapter::new();
    let agent_id = Uuid::now_v7();
    let mut agent = live_codex_agent();
    agent.id = agent_id;
    agent.model = Some("gpt-5.5".to_owned());
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

#[tokio::test]
#[ignore = "requires gemini installed — run with: make test-live"]
async fn live_gemini_model_changes_across_turns() {
    // `gemini-2.5-flash` → `-pro` per turn; no effort axis. Asserts the per-turn
    // model switch on the emitted `TurnEnd` AND on a real-file hydration (each
    // `gemini` record carries its own model — the reopen guard).
    let adapter = GeminiAdapter::new();
    let cwd = tempfile::TempDir::new().unwrap();
    // Gemini uses UUID v4 (session-file naming embeds the first 8 hex chars).
    let session_id = Uuid::new_v4();
    let agent_id = Uuid::now_v7();
    let mut agent = AgentRecord {
        model: Some("gemini-2.5-flash".to_owned()),
        effort: None,
        id: agent_id,
        project_id: Uuid::now_v7(),
        name: "m4-gemini".to_owned(),
        harness: HarnessKind::Gemini,
        session_locator: Some(SessionLocator::Uuid(session_id)),
        created_at: chrono::Utc::now(),
    };

    let events1: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            cwd.path(),
            "Reply with only the number 4.",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch 1")
        .collect()
        .await;
    let (m1, _) = turn_end_model_effort(&events1).expect("turn 1 TurnEnd");
    assert!(
        m1.as_deref().is_some_and(|m| m.contains("flash")),
        "turn 1 per-turn model = flash; got {m1:?}"
    );

    agent.model = Some("gemini-2.5-pro".to_owned());
    let events2: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            cwd.path(),
            "Reply with only the number 5.",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch 2")
        .collect()
        .await;
    let (m2, _) = turn_end_model_effort(&events2).expect("turn 2 TurnEnd");
    assert!(
        m2.as_deref().is_some_and(|m| m.contains("pro")),
        "turn 2 per-turn model = pro; got {m2:?}"
    );

    let hydrated =
        load_gemini_transcript(&home_dir(), cwd.path(), session_id, agent_id).expect("hydrate");
    let models = hydrated_turn_models(&hydrated);
    assert_eq!(models.len(), 2, "two agent turns on disk; got {models:?}");
    assert!(
        models[0].as_deref().is_some_and(|m| m.contains("flash")),
        "hydrated turn 1 = flash; got {:?}",
        models[0]
    );
    assert!(
        models[1].as_deref().is_some_and(|m| m.contains("pro")),
        "hydrated turn 2 = pro; got {:?}",
        models[1]
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
        model: None,
        effort: None,
        id: agent_id,
        project_id: Uuid::now_v7(),
        name: "m4-agy".to_owned(),
        harness: HarnessKind::Antigravity,
        session_locator: None,
        created_at: chrono::Utc::now(),
    };

    // Turn 1 on model X.
    set_model("Gemini 3.1 Pro (High)");
    let events1: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            cwd.path(),
            "Reply with only the word ack.",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch 1")
        .collect()
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
    let events2: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            cwd.path(),
            "Reply with only the word ack.",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch 2")
        .collect()
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

    let events: Vec<AdapterEvent> = adapter
        .dispatch(
            &agent,
            tmp.path(),
            // `echo` (not `ls`) — the model satisfies "list the directory" with its
            // dedicated `list_dir` tool, which never exercises the Shell mapping.
            "Run the shell command: echo switchboard-facet-probe. Then reply with the single word done.",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch")
        .collect()
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
