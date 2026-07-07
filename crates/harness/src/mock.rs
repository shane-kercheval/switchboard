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

    /// Emits a `Text` chunk, a `Thinking` chunk, another `Text` chunk, then
    /// `TurnEnd(Completed)`. The vehicle for the completion-signal test that the
    /// captured text excludes reasoning: the awaited `CompletionResult.text`
    /// must concatenate only the two `Text` chunks, never the `Thinking` one.
    StreamingWithThinking,

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

    /// Emits `ContentChunk("real answer") → TurnEnd(Completed) →
    /// ContentChunk("stray") → TurnEnd(Completed)` — a **contract-violating**
    /// adapter that emits two terminals (the Claude multi-`result`
    /// background-agent regression was exactly this bug class). The vehicle
    /// for the dispatcher's duplicate-terminal guard: the second `TurnEnd`
    /// must be dropped — not forwarded, not re-firing waiters, not
    /// overwriting the post-terminal stash that `WaitForCurrentTurn` answers
    /// from.
    DuplicateTerminal,

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

    /// Emits `ContentChunk → TurnEnd(Completed)` whose `usage.total_cost_usd`,
    /// `spend`, and `stable_message_id` are the given values. The vehicle for
    /// the dispatcher's per-turn cost/overage persistence gate: run with a
    /// real-spend `spend` + `Some(message_id)` (must persist) and with
    /// `real_spend == false` or `stable_message_id == None` (must not).
    CompletesWithSpend {
        total_cost_usd: Option<f64>,
        spend: Option<crate::events::TurnSpend>,
        stable_message_id: Option<String>,
    },

    /// Emits `ContentChunk → TurnEnd` carrying the given `first_message_id`, with
    /// the outcome `Completed` (`fail == false`) or `Failed` (`fail == true`). The
    /// vehicle for the dispatcher's durable send↔turn link gate: a terminal with a
    /// key must write a `TurnLink` for **both** `Completed` and a
    /// crash-truncated-with-content `Failed`; a terminal with `first_message_id ==
    /// None` must write none.
    TerminatesWithKey {
        fail: bool,
        first_message_id: Option<String>,
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

    /// Emits one `ContentChunk` (`"fresh-live-output"`), then **awaits an external
    /// [`tokio::sync::Notify`]** the test controls, then emits
    /// `TurnEnd(Completed)` and ends. The deterministic vehicle for proving an
    /// in-flight turn's text is captured live and handed to a current-turn waiter:
    /// the producer parks after emitting content (so a `wait_for_current_turn`
    /// registers mid-turn), and completes only when the test releases it — unlike
    /// the cancellation scenarios, whose synthesized terminal is `Cancelled`.
    CompletesOnSignal(std::sync::Arc<tokio::sync::Notify>),

    /// Emits `ContentChunk → TurnEnd(Completed)` immediately, then **holds the
    /// stream open** (awaiting an external [`tokio::sync::Notify`]) before ending.
    /// The terminal has fired, so the actor is parked *inside* `drain_turn`'s
    /// post-terminal enrichment-drain window until the test releases it — the
    /// deterministic vehicle for exercising the `FailFast` post-terminal-drain
    /// accept (a back-to-back same-agent re-send accepted while the agent's own
    /// turn is terminal but still draining) and the backlog-drop-fires-completion
    /// paths during that window. Distinct from [`Self::CompletesOnSignal`], which
    /// parks *before* the terminal (mid-turn).
    CompletesThenHolds(std::sync::Arc<tokio::sync::Notify>),
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
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                });
            }
            MockScenario::StreamingWithThinking => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "visible-one ".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Thinking,
                        text: "secret reasoning".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "visible-two".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
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
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
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
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                });
            }
            MockScenario::DuplicateTerminal => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "real answer".to_owned(),
                    });
                    let completed = AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    };
                    let _ = tx.send(completed.clone());
                    // Contract violation from here on: content after the
                    // terminal, then a second terminal.
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "stray".to_owned(),
                    });
                    let _ = tx.send(completed);
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
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
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
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
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
                        spend: None,
                        model: None,
                        effort: None,
                        stable_message_id: None,
                        first_message_id: None,
                    });
                });
            }
            MockScenario::CompletesWithSpend {
                total_cost_usd,
                ref spend,
                ref stable_message_id,
            } => {
                let spend = spend.clone();
                let stable_message_id = stable_message_id.clone();
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
                            context_window: None,
                            total_cost_usd,
                        }),
                        context_window_source: None,
                        spend,
                        model: None,
                        effort: None,
                        stable_message_id,
                        first_message_id: None,
                    });
                });
            }
            MockScenario::TerminatesWithKey {
                fail,
                ref first_message_id,
            } => {
                let first_message_id = first_message_id.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let outcome = if fail {
                        TurnOutcome::Failed {
                            kind: crate::events::FailureKind::AdapterFailure,
                            message: "mock failure with partial content".to_owned(),
                        }
                    } else {
                        TurnOutcome::Completed
                    };
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        spend: None,
                        model: None,
                        effort: None,
                        stable_message_id: None,
                        first_message_id,
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
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
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
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                });
            }
            MockScenario::CompletesOnSignal(ref signal) => {
                let signal = std::sync::Arc::clone(signal);
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "fresh-live-output".to_owned(),
                    });
                    // Park mid-turn until the test releases us, then complete.
                    signal.notified().await;
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                });
            }
            MockScenario::CompletesThenHolds(ref signal) => {
                let signal = std::sync::Arc::clone(signal);
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "held-output".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                    // Hold the stream open *after* the terminal so the actor stays
                    // inside drain_turn's post-terminal window; ending happens when
                    // the test releases us (dropping `tx` closes the stream).
                    signal.notified().await;
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
