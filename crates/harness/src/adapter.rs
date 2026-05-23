use std::path::Path;
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use switchboard_core::AgentRecord;
use tokio_util::sync::CancellationToken;

use crate::events::{AdapterEvent, TurnId};

// `CancellationToken` is carried on `DispatchOptions` (below) rather than as a
// `dispatch` parameter — extending the options struct is backwards-compatible
// with every call site, while a new trait parameter would ripple to all of
// them (the same rationale the struct's own doc gives for existing fields).

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
    /// Caller-input validation failed before spawn — the supplied prompt is
    /// not dispatch-shaped (e.g., empty/whitespace-only). The Gemini CLI
    /// rejects empty prompts with exit 42; the adapter pre-validates so the
    /// failure surfaces as a recognizable input error rather than a
    /// subprocess crash.
    #[error("invalid prompt: {0}")]
    InvalidPrompt(String),
}

/// Per-dispatch options. Plumbed through `HarnessAdapter::dispatch` so
/// adapters can react to caller-side conditions without growing the trait
/// signature for every new flag. Adapters that don't care about a given
/// field ignore it.
///
/// **Struct, not a parameter list**: extending the trait signature ripples
/// to every adapter impl and every call site; extending this struct (which
/// derives `Default`) is backwards-compatible — existing call sites that
/// pass `DispatchOptions::default()` keep compiling unchanged.
#[derive(Debug, Default, Clone)]
pub struct DispatchOptions {
    /// `true` when this dispatch is the first one Switchboard drives on
    /// an agent attached to an existing harness session (the attach-flow
    /// pre-writes a sidecar at attach time, so the adapter's normal
    /// "first turn" heuristic — `prior.is_none()` — would otherwise
    /// misclassify the dispatch as a resume).
    ///
    /// Adapters that need to re-emit per-session metadata react to this:
    /// the Codex adapter forces `SessionMeta` emission, ensuring the
    /// sidebar's MCP/skills/model registry populates on the first
    /// post-attach turn instead of staying empty until some other code
    /// path fires.
    ///
    /// Adapters with no first-dispatch-conditional behavior (Claude Code)
    /// ignore this field — Claude emits `SessionMeta` from its
    /// `system/init` stream event on every dispatch regardless.
    pub is_first_dispatch_after_attach: bool,

    /// Fired by the dispatcher to request cancellation of this turn. The
    /// adapter watches it (via `select!`) and, on cancel, does only the
    /// harness-specific teardown — kill the subprocess group — then **ends
    /// its stream without emitting a terminal event**. The dispatcher
    /// synthesizes the `TurnEnd { Cancelled }` from the token, stamping the
    /// cancel *source* it recorded (a binary token can't carry intent, and
    /// only the dispatcher knows why it fired). The dispatcher always
    /// overwrites this with the turn's token before calling `dispatch`; the
    /// `Default` (a fresh, never-fired token) is just a harmless placeholder
    /// for direct callers. The real subprocess kill lands in M4.3; until then
    /// the four real adapters ignore the token and the mock honors it.
    pub cancel_token: CancellationToken,
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
    ///
    /// `options` carries caller-side conditions (see [`DispatchOptions`]),
    /// including `options.cancel_token`, which the dispatcher fires to request
    /// cancellation of this turn. Normal sends pass `DispatchOptions::default()`;
    /// the attach-existing-session flow sets `is_first_dispatch_after_attach`.
    async fn dispatch(
        &self,
        agent: &AgentRecord,
        cwd: &Path,
        prompt: &str,
        turn_id: TurnId,
        options: DispatchOptions,
    ) -> Result<EventStream, DispatchError>;

    /// Pre-flight check that the harness can be invoked. Returns
    /// `BinaryNotFound` if the binary is missing; `Ok(())` if the adapter
    /// is ready to dispatch. In-process adapters (e.g., the mock) return
    /// `Ok(())` unconditionally.
    fn probe(&self) -> Result<(), DispatchError>;
}
