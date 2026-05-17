use std::path::Path;

use async_trait::async_trait;
use chrono::Utc;
use switchboard_core::AgentRecord;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::adapter::{DispatchError, EventStream, HarnessAdapter};
use crate::events::{AdapterEvent, ContentKind, TurnId, TurnOutcome};

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

    /// Emits a Codex-shaped post-terminal enrichment sequence:
    /// `ContentChunk → TurnEnd(Completed) → RateLimitEvent → SessionMeta`.
    /// Used in the dispatcher's `agent_idle_is_last_after_codex_post_terminal_enrichment_sequence`
    /// test to pin that the dispatcher preserves adapter event order and
    /// emits `AgentIdle` strictly after all post-terminal events. Real
    /// Codex emits this shape via `emit_terminal_with_enrichment` in
    /// `crates/harness/src/codex/mod.rs`; this scenario stands in without
    /// requiring a subprocess.
    CodexPostTerminalEnrichment,
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

    async fn dispatch(
        &self,
        agent: &AgentRecord,
        _cwd: &Path,
        prompt: &str,
        turn_id: TurnId,
        _options: crate::DispatchOptions,
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
                    });
                    let _ = tx.send(AdapterEvent::RateLimitEvent {
                        agent_id,
                        info: serde_json::json!({"primary": {"used_percent": 12.5}}),
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
            MockScenario::DispatchFails => {
                // Handled by the early return above.
                unreachable!()
            }
        }

        Ok(Box::pin(UnboundedReceiverStream::new(rx)))
    }
}
