use std::path::Path;
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use switchboard_core::AgentRecord;

use crate::events::{AdapterEvent, TurnId};

/// A stream of `AdapterEvent`s emitted by a running harness turn.
pub type EventStream = Pin<Box<dyn Stream<Item = AdapterEvent> + Send>>;

/// Errors that prevent establishing the event stream. Once the stream is returned,
/// mid-turn failures surface as `AdapterEvent::TurnEnd { outcome: Failed }` — never
/// as a `DispatchError`. This keeps the two failure paths distinct at the type level.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DispatchError {
    #[error("harness binary not found")]
    BinaryNotFound,
    #[error("failed to spawn harness subprocess: {0}")]
    SpawnFailed(#[from] std::io::Error),
}

/// Implemented by each harness (`ClaudeCode`, Codex, ...). Returns a stream of
/// `AdapterEvent`s for a single user-initiated turn.
///
/// Stream contract: consumers always receive exactly one terminal `TurnEnd` per turn.
/// The adapter owns this guarantee — if the subprocess dies without a terminal event,
/// the adapter must synthesize `TurnEnd(Failed { kind: AdapterFailure })`.
#[async_trait]
pub trait HarnessAdapter: Send + Sync {
    async fn dispatch(
        &self,
        agent: &AgentRecord,
        project_root: &Path,
        prompt: &str,
        turn_id: TurnId,
    ) -> Result<EventStream, DispatchError>;
}
