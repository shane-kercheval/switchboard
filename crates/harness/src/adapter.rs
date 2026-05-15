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
    /// Sidecar-or-equivalent pre-stream persistence read failed. Used by the
    /// Codex adapter when it can't read the session-link sidecar before
    /// deciding first-turn vs resume — a corrupt or unreadable sidecar
    /// is fail-loud (per the AGENTS.md cross-cutting invariant on Switchboard-owned
    /// JSONL corruption), not silently treated as "no prior session."
    #[error("adapter pre-stream read failed: {0}")]
    PreStreamRead(String),
}

/// Implemented by each harness (`ClaudeCode`, Codex, ...). Returns a stream of
/// `AdapterEvent`s for a single user-initiated turn.
///
/// Stream contract: consumers always receive exactly one terminal `TurnEnd` per turn.
/// The adapter owns this guarantee — if the subprocess dies without a terminal event,
/// the adapter must synthesize `TurnEnd(Failed { kind: AdapterFailure })`.
#[async_trait]
pub trait HarnessAdapter: Send + Sync {
    /// Dispatch a single turn. `cwd` is the working directory the
    /// subprocess is spawned in — for `ClaudeCodeAdapter` this is the
    /// user's bound working directory (so claude can see the user's repo
    /// files via its Read/Glob/Bash tools), **not** the per-project
    /// metadata directory inside `.switchboard/projects/<uuid>/`.
    async fn dispatch(
        &self,
        agent: &AgentRecord,
        cwd: &Path,
        prompt: &str,
        turn_id: TurnId,
    ) -> Result<EventStream, DispatchError>;

    /// Pre-flight check that the harness can be invoked. Returns
    /// `BinaryNotFound` if the binary is missing; `Ok(())` if the adapter
    /// is ready to dispatch. In-process adapters (e.g., the mock) return
    /// `Ok(())` unconditionally.
    fn probe(&self) -> Result<(), DispatchError>;
}
