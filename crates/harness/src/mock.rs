use std::path::Path;

use async_trait::async_trait;
use chrono::Utc;
use switchboard_core::AgentRecord;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::adapter::{DispatchError, EventStream, HarnessAdapter};
use crate::events::{AdapterEvent, TurnId, TurnOutcome};

/// Controls the behaviour of `MockHarnessAdapter`. Two scenarios cover M1.3's
/// needs; add more as the test suite requires them.
pub enum MockScenario {
    /// Emits three `ContentChunk`s followed by `TurnEnd(Completed)`. Used for
    /// dev-time UI iteration (no real `claude` needed) and as the standard
    /// test double for the M1.4 dispatcher.
    Streaming,

    /// Intentionally violates the stream contract — panics mid-stream before
    /// `TurnEnd`. The **only** legitimate use is testing the M1.4 dispatcher's
    /// `AgentIdleGuard` Drop path (verifies the agent returns to `Idle` even
    /// when the stream task panics). Never use in production code paths.
    Panic,
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
    async fn dispatch(
        &self,
        _agent: &AgentRecord,
        _project_root: &Path,
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
                            text: chunk,
                        });
                    }
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                    });
                });
            }
            MockScenario::Panic => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        text: "partial".to_owned(),
                    });
                    panic!("MockScenario::Panic — intentional, for AgentIdleGuard drop test");
                });
            }
        }

        Ok(Box::pin(UnboundedReceiverStream::new(rx)))
    }
}
