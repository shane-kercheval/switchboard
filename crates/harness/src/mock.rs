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
    /// test double for the M1.4 dispatcher.
    Streaming,

    /// Intentionally violates the stream contract — panics mid-stream before
    /// `TurnEnd`. The **only** legitimate use is testing the M1.4 dispatcher's
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
}

/// A `HarnessAdapter` that produces canned events without spawning any subprocess.
/// Selected at runtime via `SWITCHBOARD_HARNESS=mock` (see M1.3 step 9).
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
        _agent: &AgentRecord,
        _cwd: &Path,
        prompt: &str,
        turn_id: TurnId,
    ) -> Result<EventStream, DispatchError> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

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
        }

        Ok(Box::pin(UnboundedReceiverStream::new(rx)))
    }
}
