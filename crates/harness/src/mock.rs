use std::path::Path;

use async_trait::async_trait;
use chrono::Utc;
use switchboard_core::{AgentRecord, SessionLocator};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::adapter::{DispatchError, EventStream, HarnessAdapter};
use crate::events::{
    AdapterEvent, ContentKind, ContextWindowSource, TurnId, TurnOutcome, TurnUsage,
};

/// Controls the behaviour of `MockHarnessAdapter`.
///
/// Fault-injection scenarios (`Panic`, `TruncatedStream`) intentionally
/// violate the stream contract — in production, a missing terminal event
/// is an adapter bug, not an acceptable outcome. They exist solely to
/// exercise the dispatcher's state-recovery path.
pub enum MockScenario {
    /// Emits three `ContentChunk`s followed by `TurnEnd(Completed)`. Used for
    /// dev-time UI iteration (no real `claude` needed) and as the standard
    /// test double for the dispatcher.
    Streaming,

    /// Intentionally violates the stream contract — panics mid-stream before
    /// `TurnEnd`. The **only** legitimate use is testing the dispatcher's
    /// `AgentIdleGuard` Drop path under producer-task panic. Never use in
    /// production code paths.
    Panic,

    /// Intentionally violates the stream contract — emits two `ContentChunk`s
    /// and then drops the sender without a terminal event. Distinct from
    /// `Panic` in that the producer exits cleanly; only the contract is
    /// violated. Used to validate the dispatcher's drain loop on truncated
    /// streams *without* relying on a panic side-effect. Never use in
    /// production code paths.
    TruncatedStream,

    /// Returns `Err(DispatchError::BinaryNotFound)` from `dispatch()` before
    /// any stream is established. Used to exercise the dispatcher's
    /// pre-stream failure path: the `AgentIdleGuard` must drop on early
    /// return so agent state restores to `Idle`, and no `TurnStart` is
    /// emitted (the wire stays clean — consumers see the `DispatcherError`
    /// from `send_message`, never a half-stream).
    DispatchFails,

    /// Emits one `ContentChunk` then a terminal `TurnEnd { Failed }`
    /// (`AdapterFailure`). Used to exercise the dispatcher's handling of an
    /// adapter-emitted *failed* terminal — including journaling the failed
    /// outcome and clearing the cancellation token — which the other
    /// scenarios (completed / cancelled) don't cover.
    Fails,

    /// Emits one `ContentChunk`, then **awaits the cancellation token** and,
    /// once fired, ends the stream **without** a terminal event — mirroring a
    /// real adapter's cancel path (kill the subprocess, drop the stream, let
    /// the dispatcher synthesize `TurnEnd { Cancelled }`). The deterministic
    /// vehicle for the dispatcher's cancellation tests: the producer parks
    /// until the test fires the token, so there is no timing race.
    AwaitCancellation,

    /// Emits one `ContentChunk`, **awaits the cancellation token**, then emits
    /// a *second* `ContentChunk` and a real `TurnEnd { Completed }`, and ends —
    /// simulating a harness whose buffered output and result event lost the
    /// race with cancellation. Exercises two invariants: (1) the cancellation
    /// *latch* drops the late real terminal so the synthesized `Cancelled`
    /// wins, and (2) buffered content the agent already produced is still
    /// emitted (partial output stays visible past a cancel, per system-design
    /// §7) — only the terminal is suppressed, not the content.
    TerminalAfterCancel,

    /// Emits a Codex-shaped post-terminal enrichment sequence:
    /// `ContentChunk → TurnEnd(Completed) → RateLimitEvent → SessionMeta`.
    /// Used in the dispatcher's `agent_idle_is_last_after_codex_post_terminal_enrichment_sequence`
    /// test to pin that the dispatcher preserves adapter event order and
    /// emits `AgentIdle` strictly after all post-terminal events. Real
    /// Codex emits this shape via `emit_terminal_with_enrichment` in
    /// `crates/harness/src/codex/mod.rs`; this scenario stands in without
    /// requiring a subprocess.
    CodexPostTerminalEnrichment,

    /// Emits `ContentChunk → TurnEnd(Completed) → RateLimitEvent` with the
    /// given [`RateLimitSource`]. The vehicle for the dispatcher's
    /// metadata-persistence durability-gate test: run once with `StreamOnly`
    /// (must persist) and once with `SessionFileBacked` (must not), asserting
    /// the injected `MetadataCache`.
    RateLimitWithSource(crate::events::RateLimitSource),

    /// Emits `ContentChunk → TurnEnd(Completed)` whose `usage` carries the given
    /// `context_window`, tagged with the given [`ContextWindowSource`]. The
    /// vehicle for the dispatcher's context-window persistence gate: run with
    /// `StreamOnly` (must persist) and `SessionFileBacked` (must not).
    CompletesWithContextWindow {
        context_window: u32,
        source: ContextWindowSource,
    },

    /// Emits `ContentChunk → SessionLocatorCaptured(locator) → TurnEnd(Completed)`.
    /// The vehicle for the dispatcher's runtime-capture tests: drives the
    /// internal capture event so the dispatcher's injected `SessionLocatorSink`
    /// fires (and, with a failing sink, the turn fails). Stands in for a
    /// Codex/Antigravity adapter without a subprocess.
    CapturesLocator(SessionLocator),

    /// Emits `SessionLocatorCaptured(locator) → ContentChunk → TurnEnd(Completed)`
    /// — content and a terminal **after** the capture. Models Antigravity's
    /// post-exit drain (capture, then more transcript content + terminal). The
    /// vehicle for the persist-failure suppression test: with a failing sink the
    /// dispatcher force-fails on the capture, and nothing after it may forward.
    CapturesLocatorThenContent(SessionLocator),
}

/// A `HarnessAdapter` that produces canned events without spawning any subprocess.
/// Selected at runtime via `SWITCHBOARD_HARNESS=mock`.
pub struct MockHarnessAdapter {
    scenario: MockScenario,
}

impl MockHarnessAdapter {
    pub fn new() -> Self {
        Self::with_scenario(MockScenario::Streaming)
    }

    pub fn with_scenario(scenario: MockScenario) -> Self {
        Self { scenario }
    }
}

impl Default for MockHarnessAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HarnessAdapter for MockHarnessAdapter {
    fn probe(&self) -> Result<(), DispatchError> {
        Ok(())
    }

    fn version(&self) -> Option<String> {
        None
    }

    // One arm per `MockScenario`; the body is a flat match where each arm
    // spawns a small canned producer. It reads top-to-bottom as a catalog of
    // scenarios — splitting it into per-scenario helpers would scatter that
    // catalog for no real gain — so the length lint is allowed here.
    #[allow(clippy::too_many_lines)]
    async fn dispatch(
        &self,
        agent: &AgentRecord,
        _cwd: &Path,
        prompt: &str,
        turn_id: TurnId,
        options: crate::DispatchOptions,
    ) -> Result<EventStream, DispatchError> {
        if matches!(self.scenario, MockScenario::DispatchFails) {
            return Err(DispatchError::BinaryNotFound);
        }

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let agent_id = agent.id;

        match self.scenario {
            MockScenario::Streaming => {
                let prompt = prompt.to_owned();
                tokio::spawn(async move {
                    let chunks: [String; 3] = [
                        "Mock response to: ".to_owned(),
                        prompt,
                        " — replied by mock harness.".to_owned(),
                    ];
                    for chunk in chunks {
                        let _ = tx.send(AdapterEvent::ContentChunk {
                            turn_id,
                            kind: ContentKind::Text,
                            text: chunk,
                        });
                    }
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                    });
                });
            }
            MockScenario::Panic => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "partial".to_owned(),
                    });
                    panic!("MockScenario::Panic — intentional, for AgentIdleGuard drop test");
                });
            }
            MockScenario::TruncatedStream => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "partial-one".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "partial-two".to_owned(),
                    });
                    // Drop tx without emitting TurnEnd — stream closes silently.
                });
            }
            MockScenario::Fails => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "partial-before-failure".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Failed {
                            kind: crate::events::FailureKind::AdapterFailure,
                            message: "mock failure".to_owned(),
                        },
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                    });
                });
            }
            MockScenario::AwaitCancellation => {
                let cancel_token = options.cancel_token.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "partial-before-cancel".to_owned(),
                    });
                    // Park until cancelled, then end the stream with no
                    // terminal event — the dispatcher synthesizes Cancelled.
                    cancel_token.cancelled().await;
                });
            }
            MockScenario::TerminalAfterCancel => {
                let cancel_token = options.cancel_token.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "before-cancel".to_owned(),
                    });
                    cancel_token.cancelled().await;
                    // Buffered content the agent produced before the kill — it
                    // should still be emitted (partial output stays visible).
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "after-cancel".to_owned(),
                    });
                    // Agent-scoped enrichment buffered behind the cancel — it
                    // reflects real agent state and is forwarded as-is (only the
                    // terminal is the dispatcher's to synthesize).
                    let _ = tx.send(AdapterEvent::RateLimitEvent {
                        agent_id,
                        info: serde_json::json!({"primary": {"used_percent": 50.0}}),
                        // Codex-shaped enrichment → session-file-backed.
                        source: crate::events::RateLimitSource::SessionFileBacked,
                    });
                    // A real terminal that lost the race with cancellation —
                    // the dispatcher must drop this in favor of Cancelled.
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                    });
                });
            }
            MockScenario::CodexPostTerminalEnrichment => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                    });
                    let _ = tx.send(AdapterEvent::RateLimitEvent {
                        agent_id,
                        info: serde_json::json!({"primary": {"used_percent": 12.5}}),
                        // Codex-shaped enrichment → session-file-backed.
                        source: crate::events::RateLimitSource::SessionFileBacked,
                    });
                    let _ = tx.send(AdapterEvent::SessionMeta {
                        agent_id,
                        model: "gpt-test".to_owned(),
                        harness_version: "0.130.0".to_owned(),
                        tools: vec![],
                        mcp_servers: vec![crate::events::McpServerStatus {
                            name: "fs".to_owned(),
                            status: "connected".to_owned(),
                        }],
                        skills: vec![],
                        raw: serde_json::Value::Null,
                    });
                });
            }
            MockScenario::RateLimitWithSource(source) => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                    });
                    let _ = tx.send(AdapterEvent::RateLimitEvent {
                        agent_id,
                        info: serde_json::json!({"primary": {"used_percent": 42.0}}),
                        source,
                    });
                });
            }
            MockScenario::CompletesWithContextWindow {
                context_window,
                source,
            } => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: Some(TurnUsage {
                            input_tokens: 100,
                            output_tokens: 25,
                            cached_input_tokens: None,
                            cache_creation_input_tokens: None,
                            context_input_tokens: Some(100),
                            reasoning_output_tokens: None,
                            context_window: Some(context_window),
                            total_cost_usd: None,
                        }),
                        context_window_source: Some(source),
                    });
                });
            }
            MockScenario::CapturesLocator(ref locator) => {
                let locator = locator.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::SessionLocatorCaptured { locator });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                    });
                });
            }
            MockScenario::CapturesLocatorThenContent(ref locator) => {
                let locator = locator.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::SessionLocatorCaptured { locator });
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "post-capture content".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                    });
                });
            }
            MockScenario::DispatchFails => {
                // Handled by the early return above.
                unreachable!()
            }
        }

        Ok(Box::pin(UnboundedReceiverStream::new(rx)))
    }
}
