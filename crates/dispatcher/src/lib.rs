//! Switchboard dispatcher — drives harness adapters via one actor task per agent.
//!
//! # Concurrency model & alternatives considered
//!
//! Each agent is served by a single long-lived **actor task** that owns all of
//! that agent's mutable state in its local scope — a FIFO backlog, the running
//! turn's cancellation handle, and an app-injected dispatch-context builder.
//! The actor is reached only through an `mpsc` **command channel**; the
//! `Dispatcher` itself holds nothing but a `Mutex<HashMap<AgentId, AgentSlot>>`
//! of those senders (an `AgentSlot` is `Active(sender)` or `Closing` — the
//! latter is the resurrection guard during teardown).
//!
//! Because a single task is the sole consumer of an agent's work, **"one turn
//! in flight per agent" is structural** — there is no status flag, no RAII
//! idle-guard, and no terminal→next-turn handoff to coordinate (a single
//! consumer that pops-or-parks cannot race itself). The queue is just the
//! actor's backlog. This was chosen over a shared per-agent state map (status
//! flag + guard + a hand-written atomic handoff) precisely because the handoff
//! was a TOCTOU against concurrent sends; the actor deletes that surface
//! rather than guarding it. The cost is that the actor must **multiplex control
//! commands against the running turn's event stream via `select!`** so that
//! cancel/remove/shutdown act promptly mid-turn rather than queuing behind the
//! turn — that multiplexing is the one load-bearing concurrency surface here.
//!
//! **Cancellation is out-of-band by necessity:** a running turn must be
//! interruptible, so cancel cannot sit in the FIFO behind it — it is a
//! `select!` arm that fires the running turn's `CancellationToken`. The adapter
//! (which owns the harness-specific kill) ends its stream without a terminal;
//! the actor synthesizes `TurnEnd { Cancelled { source } }` from the source it
//! recorded. A cancel with no turn live, or after the turn's terminal was
//! observed, is a no-op.
//!
//! **Wire contract:** `send_message` returns a `MessageId` immediately (the
//! send is *accepted*, not necessarily started). The `turn_id` is delivered on
//! the `TurnStart` event, which carries the originating `message_id` for
//! correlation. A send that fails before any turn starts surfaces as
//! `MessageFailed`. `AgentIdle` fires only when the actor parks with an empty
//! backlog (genuine idle) — never between chained queued turns.
//!
//! **Opt-in per-send completion signal.** A caller that needs to *await* a
//! specific send's outcome (the workflow runtime, the cross-agent forward
//! resolver) uses [`Dispatcher::send_message_awaiting_completion`], which hands
//! back a [`oneshot::Receiver<CompletionResult>`]. The signal fires **exactly
//! once**, when that send's turn reaches a terminal state, carrying the same
//! [`TurnOutcome`] the actor used for the terminal `TurnEnd` it emitted (or
//! synthesized) **plus the turn's captured text output**. A per-send `oneshot`
//! is the minimal primitive here: the caller holds the handle at send time and
//! there is exactly one awaiter per send, so — unlike a `turn_id`-keyed
//! broadcast/watch — there is no keying, no cleanup, and no missed-event window.
//! It fires at *every* terminal-synthesis point (normal terminal, synthesized
//! cancel, force-failed locator persist, journal/dispatch failure, and a stream
//! that ends with neither terminal nor cancel — which now synthesizes a `Failed`
//! terminal for awaited *and* non-awaited sends alike, one truncation semantics
//! for both), so an awaited send can never hang. A dropped `Receiver` (caller
//! stopped awaiting) is ignored, exactly as the actor ignores any other dropped
//! reply channel.
//!
//! **Per-agent current-turn wait.** A second, distinct await primitive serves
//! the *manual* cross-agent forward: [`Dispatcher::wait_for_current_turn`]
//! resolves when an agent's already-running turn reaches its terminal (or
//! immediately, [`CurrentTurnWait::Idle`], if the agent is idle). Unlike the
//! per-send signal above, the waiter never dispatched that turn — the manual
//! forward references an agent whose turn the user kicked off from an *earlier*
//! compose send (`completion = None`), so there is no per-send handle to it. It
//! still carries the turn's captured text on a `Completed` terminal — because the
//! actor captures every turn's text from its start (not only awaited sends), a
//! waiter that registers mid-turn gets the whole output without a disk read. Disk
//! is read only for an agent that was already *idle* at wait time (no live turn to
//! capture). Both primitives fire at the same terminal-synthesis points — a turn
//! resolves all its awaiters together via [`TurnAwaiters`].
//!
//! **The signal resolves at the turn's terminal, not when the agent is next
//! re-dispatchable** — it fires the instant the terminal is observed, so the
//! forwarded text is available with minimal latency, while the actor then keeps
//! draining post-terminal adapter events (for some harnesses a session-file read,
//! not microseconds) before it parks. To keep a back-to-back same-agent
//! `send`→await→`send` from spuriously failing in that window, the `FailFast`
//! enqueue path **accepts** a send that arrives once the current turn is terminal
//! and nothing else is queued (it runs when drain finishes); it returns `Busy`
//! only for genuine contention — a turn still mid-flight, or other work already
//! queued. So a caller may re-dispatch to a just-awaited agent immediately after
//! its completion without awaiting `AgentIdle`, and `Busy` still means a real
//! conflict (the workflow "contention = step failure" rule holds).
//!
//! **Why the payload carries captured text, not a turn id (decision #7).** The
//! awaited turn's text is accumulated by the actor from the live stream (the
//! `Text`-kind `ContentChunk`s it already drains) and delivered in the
//! `CompletionResult`, so a forward/aggregate consumer never reads it back from
//! disk. This sidesteps identity entirely: the dispatcher's `turn_id` is **not**
//! joinable to the harness session file's own turn ids (they are different id
//! spaces — `crates/app` correlates them only positionally), and the one stable
//! per-turn key that does exist (`hydration_key`) is absent for some harnesses.
//! "Hold a `turn_id`, then find the matching turn on disk" therefore cannot work
//! for the just-awaited turn; capturing the text at completion is the only
//! reliable source. Text is accumulated for **every** turn from its start (one
//! bounded `String` per in-flight turn): a manual forward's current-turn waiter
//! registers *mid-turn* and would otherwise miss the prefix, and the same capture
//! removes the disk-flush race the manual path would hit reading a just-finished
//! turn off disk. (An earlier design captured only for awaited sends; that left
//! the manual path reading disk, which could return a *stale* earlier turn when
//! the just-finished one had not flushed — so capture is now unconditional.)
//!
//! The `EventEmitter` trait keeps the dispatcher unit-testable without a Tauri
//! app — `RecordingEmitter` collects emissions (and offers an async wait) for
//! assertions; production wiring (in `crates/app`) provides a Tauri-backed
//! implementation.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use futures::StreamExt;
use switchboard_core::{
    AgentId, AgentRecord, Attachment, SendId, SessionLocator, render_prompt_with_attachments,
};
use switchboard_harness::{
    AdapterEvent, CancelSource, ContentKind, ContextWindowSource, DispatchOptions, EventStream,
    FailureKind, HarnessAdapter, MessageId, NormalizedEvent, RateLimitSource, TurnId, TurnOutcome,
    TurnSpend,
};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// A message accepted into an agent's queue (or run immediately). Carries only
/// what cannot be recomputed when the turn finally starts — the adapter, cwd,
/// emitter, options, and journal are rebuilt by the actor's
/// [`DispatchContextFactory`] at start time, so per-dispatch state is never
/// frozen at enqueue.
///
/// Not `Clone`: it owns the optional one-shot completion sender, which is
/// single-consumer by construction. The item is *moved* through the backlog and
/// into `run_turn`; nothing duplicates it.
#[derive(Debug)]
struct WorkItem {
    message_id: MessageId,
    send_id: SendId,
    /// The **clean** prompt (no attachment footer). The agent-facing footer is
    /// rendered from `attachments` at dispatch time, so the queued item stays
    /// the user's literal text.
    prompt: String,
    attachments: Vec<Attachment>,
    /// Set only for sends made via [`Dispatcher::send_message_awaiting_completion`].
    /// The actor fires it once when this send's turn reaches a terminal state.
    /// `None` for the compose-bar path — that path allocates no completion channel
    /// and accumulates no text. The awaitable path is `FailFast`, normally
    /// dispatched immediately; but the post-terminal-drain accept (see the
    /// `FailFast` enqueue arm) can briefly queue such an item, so the contract
    /// "an accepted awaitable always resolves" is upheld by **every** backlog-drop
    /// path firing this completion with a synthesized `Cancelled` terminal before
    /// discarding the item (see [`resolve_dropped_completion`]) — never a silent
    /// drop that would leave the awaiter on a `RecvError`.
    completion: Option<oneshot::Sender<CompletionResult>>,
    /// Emit a live [`NormalizedEvent::UserMessage`] for this send once it is
    /// durable (after `record_send` succeeds, before `TurnStart`). Set for
    /// backend-originated sends (a workflow `send`) that have no frontend
    /// optimistic user turn; `false` for the compose-bar path, which renders its
    /// user turn optimistically. Emitting at the journal boundary keeps the live
    /// user message identical to the reloaded journal view by construction.
    emit_user_message: bool,
}

/// Delivered once, when an awaited send's turn reaches a terminal state, over
/// the channel returned by [`Dispatcher::send_message_awaiting_completion`].
///
/// `text` is the turn's accumulated `Text`-kind output (no `Thinking`, no tool
/// output) for a `Completed` turn; it is empty for `Failed`/`Cancelled` turns,
/// which produce no forwardable output. See the module doc for why this is
/// captured from the live stream rather than re-derived from disk (decision #7).
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionResult {
    pub outcome: TurnOutcome,
    pub text: String,
}

/// The set of awaiters a turn must resolve **exactly once** when it reaches a
/// terminal state, bundled so every terminal-synthesis path fires all of them
/// together (the centralization that keeps a new terminal path from
/// emit-or-journal-but-forget-to-fire). Two kinds ride the same terminal:
///
/// - `completion` — the per-send handle (outcome + captured text). At most
///   one, set at send time only for `send_message_awaiting_completion`.
/// - `current_turn` — per-agent [`Command::WaitForCurrentTurn`] waiters
///   registered *mid-turn* (the manual forward's source wait). Carry the same
///   captured text as the completion handle. Any number can register against
///   one running turn.
#[derive(Default)]
struct TurnAwaiters {
    completion: Option<oneshot::Sender<CompletionResult>>,
    current_turn: Vec<oneshot::Sender<CurrentTurnWait>>,
}

impl TurnAwaiters {
    /// Fire every awaiter exactly once with this terminal `outcome` and captured
    /// `text` (empty for a non-completed turn). A single turn can carry **both** a
    /// completion handle (a workflow dispatched this agent) and current-turn
    /// waiters (a manual forward references it), so both kinds get the text — the
    /// completion side must not consume it by move and leave the waiters textless.
    /// Senders are taken / drained, so a second call is a no-op; a dropped
    /// receiver makes `send` return `Err`, which is ignored — consistent with how
    /// the actor treats every other dropped reply channel.
    fn fire(&mut self, outcome: &TurnOutcome, text: &str) {
        if let Some(tx) = self.completion.take() {
            let _ = tx.send(CompletionResult {
                outcome: outcome.clone(),
                text: text.to_owned(),
            });
        }
        for tx in self.current_turn.drain(..) {
            let _ = tx.send(CurrentTurnWait::Terminal {
                outcome: outcome.clone(),
                text: text.to_owned(),
            });
        }
    }
}

/// Result of [`Dispatcher::send_message_awaiting_completion`]. Modelled as an
/// enum (rather than `(SendOutcome, Option<Receiver>)`) so a caller cannot
/// confuse "the agent was busy, nothing was queued" with "queued, awaiting
/// completion": the completion handle exists **only** on the `Accepted` arm,
/// minted only once acceptance is certain.
#[must_use]
pub enum AwaitableSendOutcome {
    /// The send was accepted. `completion` resolves when the turn terminates.
    Accepted {
        message_id: MessageId,
        completion: oneshot::Receiver<CompletionResult>,
    },
    /// `FailFast` was requested and the agent was busy; nothing was enqueued and
    /// there is no completion handle to await.
    Busy,
}

/// Result of [`Dispatcher::wait_for_current_turn`] — the per-agent
/// "await this agent's current in-flight turn's terminal" capability used by the
/// manual cross-agent forward.
///
/// Like the per-send [`CompletionResult`], this carries the turn's captured
/// text — but for a turn the waiter never dispatched (the manual forward
/// references an agent whose turn the user kicked off from an *earlier* compose
/// send, so there is no per-send handle to it). Because the waiter registers
/// *mid-turn*, the dispatcher accumulates every turn's text from its start (not
/// only awaited sends), so a `Terminal { Completed }` carries the **whole**
/// turn's output live — no disk read, no flush race. Disk is read only for an
/// `Idle` source (no live turn to capture; its file is already settled).
#[derive(Debug, Clone, PartialEq)]
pub enum CurrentTurnWait {
    /// The agent had no in-flight turn — it was idle (or between queued turns,
    /// or never dispatched). The caller reads the source's latest completed
    /// output from disk (settled, since nothing is running).
    Idle,
    /// The agent had an in-flight turn, which reached this terminal outcome.
    /// `text` is the turn's captured `Text`-kind output for a `Completed`
    /// outcome (empty for `Failed` / `Cancelled`, which the manual forward
    /// invalidates on) — the same live-stream capture as [`CompletionResult`].
    Terminal { outcome: TurnOutcome, text: String },
}

/// What `send_message` should do when the agent already has a turn in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnBusy {
    /// Compose-bar default: append to the FIFO backlog; it dispatches when the
    /// in-flight turn (and any earlier queued turns) finish.
    Enqueue,
    /// Workflow-step default (system-design §7): refuse with `Busy` so an
    /// autonomous collision surfaces instead of silently serializing.
    FailFast,
}

/// Result of `send_message`. `Busy` is only possible under [`OnBusy::FailFast`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    /// The send was accepted (queued or started immediately). The `turn_id`
    /// arrives later on the correlated `TurnStart`.
    Accepted(MessageId),
    /// `FailFast` was requested and the agent was busy; nothing was enqueued.
    Busy,
}

/// Outcome of a [`Dispatcher::cancel`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CancelOutcome {
    /// A cancel was delivered to the agent's actor. The actor no-ops if no turn
    /// is live (idle or already past its terminal) — token-presence is the
    /// in-actor cancellability signal, so "delivered" does not guarantee a turn
    /// was actually running.
    Requested,
    /// No actor exists for the agent (never dispatched, or already shut down).
    NothingToCancel,
}

/// The payload of a removed queued message, returned so the UI can restore the
/// composer text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedQueuedMessage {
    pub agent_id: AgentId,
    pub send_id: SendId,
    pub prompt: String,
    /// Attachments of the removed send, returned so the compose bar can restore
    /// the chips alongside the text (consistent with restoring the prompt).
    pub attachments: Vec<Attachment>,
}

/// `remove_queued_message` found no such queued message — already
/// dequeued/started, already removed, never existed, or the agent has no actor.
/// Typed so the frontend doesn't fake-restore composer text for a message that
/// is already running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no queued message with that id")]
pub struct NotQueued;

/// Commands sent to an agent's actor over its `mpsc` channel.
enum Command {
    /// Append work (or, under `FailFast`, reject if the agent is busy). The
    /// actor — the single authority — decides; `reply` is `Some` only for
    /// `FailFast` (the `Enqueue` path already returned its `MessageId`).
    Enqueue {
        item: WorkItem,
        on_busy: OnBusy,
        reply: Option<oneshot::Sender<SendOutcome>>,
    },
    /// Remove a queued message by id; reply with its payload or `NotQueued`.
    Remove {
        message_id: MessageId,
        reply: oneshot::Sender<Result<RemovedQueuedMessage, NotQueued>>,
    },
    /// Await the agent's **current in-flight turn's** terminal (the per-agent
    /// forward-source wait). Replies [`CurrentTurnWait::Idle`] at once if no turn
    /// is running; otherwise the reply is held and fired with
    /// [`CurrentTurnWait::Terminal`] when the running turn reaches its terminal —
    /// the same instant the completion signal fires, since both ride the
    /// turn's terminal-synthesis points.
    WaitForCurrentTurn {
        reply: oneshot::Sender<CurrentTurnWait>,
    },
    /// **Non-blocking** peek: reply `true` iff a turn is *actively running* right
    /// now (not idle, not already past its terminal). Unlike `WaitForCurrentTurn`
    /// it never holds the reply — used by completed-only forwarding to reject a
    /// still-streaming source without waiting on it.
    PeekCurrentTurn { reply: oneshot::Sender<bool> },
    /// Cancel the running turn (out-of-band; no-op if none is live).
    Cancel(CancelSource),
    /// Cancel a whole *send* on this agent: fire the running turn's cancel
    /// token **iff** the running item's `send_id` matches, and drop any queued
    /// items of that send. No-op otherwise (already past this send, or never
    /// had it). Out-of-band, no reply — effects surface as the turn's
    /// synthesized `Cancelled` terminal.
    CancelSend {
        send_id: SendId,
        source: CancelSource,
    },
    /// Stop the whole agent: fire the running turn's cancel token (if any) **and**
    /// clear the entire backlog, **without** shutting the actor down — the agent
    /// stays loaded and idle, ready for new work. Out-of-band, no reply; the
    /// running turn's effect surfaces as its synthesized `Cancelled` terminal, and
    /// the dropped queued items are discarded silently (never journaled, so the
    /// frontend's optimistic cleanup is the only place they need resolving — same
    /// as `CancelSend`'s queued drop, just unscoped).
    CancelAgent { source: CancelSource },
    /// Close the actor: reject further work, clear the backlog, cancel any
    /// running turn, drain it, then signal `reply` and exit.
    Shutdown {
        source: CancelSource,
        reply: oneshot::Sender<()>,
    },
}

/// The dispatcher's per-agent map slot.
///
/// `Active` holds the actor's command sender (no `busy` watch — contention is
/// decided inside the actor for `FailFast`, and teardown completion comes from
/// the `Shutdown` reply, so a watch would be dead state inviting a
/// check-then-act race).
///
/// `Closing` is the **resurrection guard**: while an agent is being torn down
/// (`shutdown_agent`), its slot is `Closing` so a racing `send_message` is
/// rejected rather than spawning a *fresh* actor that would drive the same
/// harness session concurrently with the draining turn (the double-drive the
/// project lock exists to prevent). The dispatcher upholds its own
/// one-driver-per-agent invariant here rather than relying on an app-layer
/// guard. The entry is removed only after the actor's `Shutdown` reply, so a
/// *later* send (e.g. project re-open) creates a new actor normally.
enum AgentSlot {
    Active(mpsc::UnboundedSender<Command>),
    Closing,
}

/// Errors returned by the conversation-journal write contract. See
/// [`ConversationJournal`].
#[derive(Debug, thiserror::Error)]
#[error("conversation journal write failed: {0}")]
pub struct JournalError(pub Box<dyn std::error::Error + Send + Sync + 'static>);

/// A sink for outbound events. The dispatcher emits one event per
/// `NormalizedEvent` on the channel `agent:<agent_id>` — the same channel for
/// the lifetime of the agent (per-agent, not per-turn).
pub trait EventEmitter: Send + Sync {
    fn emit(&self, name: &str, payload: serde_json::Value);
}

/// Test double — records emissions for assertion, and offers an async wait so
/// tests can deterministically block until the agent's event stream reaches a
/// state (e.g. `AgentIdle` observed) without a per-send join handle.
#[derive(Default)]
pub struct RecordingEmitter {
    events: Mutex<Vec<(String, serde_json::Value)>>,
    notify: Notify,
}

impl RecordingEmitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of all emissions since construction.
    pub fn snapshot(&self) -> Vec<(String, serde_json::Value)> {
        lock(&self.events).clone()
    }

    pub fn len(&self) -> usize {
        lock(&self.events).len()
    }

    pub fn is_empty(&self) -> bool {
        lock(&self.events).is_empty()
    }

    /// Resolve once a recorded event satisfies `pred`. Uses the
    /// register-before-recheck pattern so an `emit` racing between the check
    /// and the await is not lost.
    pub async fn wait_for(&self, mut pred: impl FnMut(&[(String, serde_json::Value)]) -> bool) {
        loop {
            let fut = self.notify.notified();
            tokio::pin!(fut);
            fut.as_mut().enable();
            if pred(&lock(&self.events)) {
                return;
            }
            fut.await;
        }
    }

    /// Convenience: resolve once an event of the given `type` tag for `agent_id`
    /// (or any agent, if the event is turn-scoped) has been recorded. Matches on
    /// the wire `type` field; counts occurrences ≥ `count`.
    pub async fn wait_for_type(&self, event_type: &str, count: usize) {
        self.wait_for(|events| {
            events
                .iter()
                .filter(|(_, v)| v["type"] == event_type)
                .count()
                >= count
        })
        .await;
    }
}

impl EventEmitter for RecordingEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        lock(&self.events).push((name.to_owned(), payload));
        self.notify.notify_waiters();
    }
}

/// Switchboard's user-side conversation persistence, injected like
/// [`EventEmitter`] so the dispatcher stays app-agnostic. The dispatcher (via
/// the actor) owns the *timing*; the app-side implementation owns the *path*
/// (which project's `journal.jsonl`) and the `send_id`; core owns the *format*.
///
/// This refines the "Switchboard stores no transcript" invariant: Switchboard
/// owns the user-send journal + non-completed-turn outcomes; harness session
/// files own completed-turn agent content. The two partition cleanly.
///
/// **The two methods have deliberately different failure contracts:**
///
/// - `record_send` is **fail-closed**: written *before* the turn's subprocess
///   is spawned, returns `Result`. If it fails, the actor does **not** start
///   the turn and surfaces a `MessageFailed` (the async analogue of the
///   pre-actor synchronous fail-closed `Err`). The unified transcript sources
///   the user's messages from the journal, so a silently-lost send record would
///   become an assistant reply with no user message above it after restart.
/// - `record_outcome` is **best-effort**: by the time it fires, the turn has
///   already run. It returns `()`; the implementation logs on failure. The
///   degradation if lost is a mislabel, not data loss.
pub trait ConversationJournal: Send + Sync {
    /// Written when a turn *starts* (immediately before dispatch), one per
    /// recipient. **Fail-closed** — `Err` aborts the turn-start.
    fn record_send(
        &self,
        turn_id: TurnId,
        agent_id: AgentId,
        prompt: &str,
        attachments: &[Attachment],
        at: DateTime<Utc>,
    ) -> Result<(), JournalError>;

    /// Written on a **non-completed** terminal (failed or cancelled) — never
    /// for a completed turn (whose content lives in the harness session file).
    /// **Best-effort.**
    fn record_outcome(
        &self,
        turn_id: TurnId,
        agent_id: AgentId,
        outcome: &TurnOutcome,
        started_at: DateTime<Utc>,
        ended_at: DateTime<Utc>,
    );
}

/// Sink for **stream-only** (class-C) metadata that has no harness-side
/// on-disk equivalent, so Switchboard persists it to survive an app restart.
/// Parallel to [`ConversationJournal`]: the dispatcher owns *when* a snapshot
/// is recorded (on a `RateLimitEvent` whose `source` is `StreamOnly`); the
/// app-side impl owns *where* (the per-agent metadata sidecar).
///
/// **Best-effort, not load-bearing.** Returns `()`; the implementation logs
/// and swallows write errors. The cache is a UX improvement (restart
/// continuity), not a correctness dependency — a failed write degrades to
/// "the agent bar is empty until the next event," exactly the pre-cache
/// behavior, never a failed turn.
pub trait MetadataCache: Send + Sync {
    /// Persist the latest rate-limit snapshot for `agent_id`. Last-write-wins.
    fn record_rate_limit(
        &self,
        agent_id: AgentId,
        info: serde_json::Value,
        captured_at: DateTime<Utc>,
    );

    /// Persist the latest context-window size for `agent_id`. Last-write-wins.
    /// Stream-only for Claude (the window lives in `result.modelUsage`, never
    /// in the session file), so it must be cached to let the context bar
    /// render on reopen instead of blanking until the next turn.
    fn record_context_window(
        &self,
        agent_id: AgentId,
        context_window: u32,
        captured_at: DateTime<Utc>,
    );

    /// Append one real-spend turn's cost + overage telemetry, keyed on the
    /// turn's per-message id. Stream-only for Claude (cost/overage arrive on the
    /// `result` record, never in the session file), so it must be persisted to
    /// re-attach the inline cost + "using credits" marker to the right message
    /// on reopen. Unlike the snapshots above this is an append-log (one record
    /// per turn), not last-write-wins. `message_id` is the join key the
    /// hydrated turn carries; the dispatcher gates on its presence + real-spend,
    /// not on harness identity.
    fn record_turn_spend(
        &self,
        agent_id: AgentId,
        message_id: String,
        total_cost_usd: Option<f64>,
        spend: TurnSpend,
        captured_at: DateTime<Utc>,
    );
}

/// No-op metadata cache for tests and any caller that doesn't persist
/// stream-only metadata (e.g. the live end-to-end harness).
pub struct NoopMetadataCache;

impl MetadataCache for NoopMetadataCache {
    fn record_rate_limit(&self, _: AgentId, _: serde_json::Value, _: DateTime<Utc>) {}
    fn record_context_window(&self, _: AgentId, _: u32, _: DateTime<Utc>) {}
    fn record_turn_spend(
        &self,
        _: AgentId,
        _: String,
        _: Option<f64>,
        _: TurnSpend,
        _: DateTime<Utc>,
    ) {
    }
}

/// Error returned when persisting a captured session locator fails. See
/// [`SessionLocatorSink`].
#[derive(Debug, thiserror::Error)]
#[error("session locator persist failed: {0}")]
pub struct SessionLocatorError(pub Box<dyn std::error::Error + Send + Sync + 'static>);

/// Sink for a runtime-captured [`SessionLocator`], injected like
/// [`MetadataCache`]/[`ConversationJournal`] so the dispatcher stays
/// app-agnostic. The dispatcher calls this on an
/// [`AdapterEvent::SessionLocatorCaptured`] (emitted when a Codex/Antigravity
/// adapter first learns — or, on an Antigravity fork-and-heal, re-learns — its
/// locator); the app-side impl persists it to the agent's registry record.
///
/// **Load-bearing, unlike `MetadataCache`.** A persist failure means the next
/// turn would start a fresh session and silently drop context, so the
/// dispatcher fails the turn (synthesizes a `Failed { AdapterFailure }` terminal
/// and tears the child down via the turn's cancel token), matching the old
/// sidecar-write-failure semantics. The metadata cache, by contrast, swallows
/// write errors.
pub trait SessionLocatorSink: Send + Sync {
    /// Persist `locator` as `agent_id`'s session identity. Returns `Err` only on
    /// a genuine persistence failure (registry write / lookup error).
    fn persist(
        &self,
        agent_id: AgentId,
        locator: SessionLocator,
    ) -> Result<(), SessionLocatorError>;
}

/// No-op locator sink for tests and callers with no registry (e.g. the live
/// end-to-end harness, which dispatches against the real CLI without persisting
/// the locator). Always succeeds, so a capture event never fails the turn.
pub struct NoopSessionLocatorSink;

impl SessionLocatorSink for NoopSessionLocatorSink {
    fn persist(&self, _: AgentId, _: SessionLocator) -> Result<(), SessionLocatorError> {
        Ok(())
    }
}

/// No-op journal for tests and any caller that doesn't persist the user side.
pub struct NoopJournal;

impl ConversationJournal for NoopJournal {
    fn record_send(
        &self,
        _: TurnId,
        _: AgentId,
        _: &str,
        _: &[Attachment],
        _: DateTime<Utc>,
    ) -> Result<(), JournalError> {
        Ok(())
    }
    fn record_outcome(
        &self,
        _: TurnId,
        _: AgentId,
        _: &TurnOutcome,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) {
    }
}

/// Everything needed to run one turn, produced fresh by a
/// [`DispatchContextFactory`] at the moment a turn starts. The immutable fields
/// (`adapter`, `cwd`, `agent`) are stable for an agent's lifetime; `emitter`,
/// `options`, and `journal` are rebuilt per dispatch so per-dispatch state is
/// never frozen at enqueue.
pub struct DispatchContext {
    pub adapter: Arc<dyn HarnessAdapter>,
    pub cwd: PathBuf,
    pub agent: AgentRecord,
    pub emitter: Arc<dyn EventEmitter>,
    pub options: DispatchOptions,
    pub journal: Arc<dyn ConversationJournal>,
    pub metadata: Arc<dyn MetadataCache>,
    pub locator_sink: Arc<dyn SessionLocatorSink>,
}

/// Builds a [`DispatchContext`] for an agent's next turn. Injected by the app
/// (like [`EventEmitter`] / [`ConversationJournal`]) and **owned by the agent's
/// actor** for its lifetime — the actor calls `build` at the moment each turn
/// starts, so any per-dispatch state it reads (e.g. `needs_session_meta`) is
/// current, never frozen at enqueue. `send_id` is the only per-message input;
/// the prompt rides on the work item separately.
///
/// The app impl must capture only agent-lifetime-stable data plus live `Arc`
/// handles — never `Arc<Dispatcher>` (which it does not need) — so there is no
/// reference cycle.
pub trait DispatchContextFactory: Send + Sync {
    fn build(&self, send_id: SendId) -> DispatchContext;

    /// The plain per-agent event sink, used to emit `AgentIdle` after the
    /// backlog drains — at which point no per-turn context is in hand.
    /// `AgentIdle` carries no session metadata, so the per-dispatch emitter
    /// wrapping is irrelevant; returning the base emitter is sufficient.
    fn idle_emitter(&self) -> Arc<dyn EventEmitter>;
}

/// The single chokepoint for sending turns to agents. Globally keyed by
/// `AgentId` (UUID v7, globally unique), so no project context is needed here.
/// Holds only the per-agent command-channel senders; all per-agent mutable
/// state lives inside each agent's actor task.
pub struct Dispatcher {
    agents: Mutex<HashMap<AgentId, AgentSlot>>,
}

impl Dispatcher {
    pub fn new() -> Self {
        Self {
            agents: Mutex::new(HashMap::new()),
        }
    }

    /// Number of live agent slots — `Active` actors plus any mid-teardown
    /// `Closing` slots. Zero means no actor (and therefore no harness
    /// subprocess) is being kept alive. Used to assert that teardown leaves no
    /// orphan actor.
    #[must_use]
    pub fn agent_slot_count(&self) -> usize {
        lock(&self.agents).len()
    }

    /// Accept a send for `agent_id`, spawning the agent's actor on first use.
    /// Returns `SendOutcome::Accepted(message_id)` immediately for the
    /// `Enqueue` (compose-bar) path; under `FailFast` it awaits the actor's
    /// authoritative `Accepted`/`Busy` decision. The `factory` is used only
    /// when the actor is first created (it owns the builder thereafter); later
    /// sends to an existing actor ignore the passed factory.
    ///
    /// This is the compose-bar path: it carries **no** completion handle, so no
    /// caller is awaiting the turn's terminal here. (Per-turn text is still
    /// captured always-on for current-turn waiters — see the `captured_text`
    /// declaration in the actor loop.)
    pub async fn send_message(
        &self,
        agent_id: AgentId,
        prompt: &str,
        attachments: Vec<Attachment>,
        send_id: SendId,
        factory: Arc<dyn DispatchContextFactory>,
        on_busy: OnBusy,
    ) -> SendOutcome {
        let item = WorkItem {
            message_id: Uuid::now_v7(),
            send_id,
            prompt: prompt.to_owned(),
            attachments,
            completion: None,
            // Compose-bar sends render their user turn optimistically on the
            // frontend; no backend-emitted user message.
            emit_user_message: false,
        };
        self.accept(agent_id, item, factory, on_busy).await
    }

    /// Like [`send_message`](Self::send_message), but hands back a one-shot
    /// channel that resolves with the turn's [`CompletionResult`] (outcome +
    /// captured text) when this send's turn terminates. Used by the workflow
    /// runtime and the cross-agent forward resolver, which must *await* a
    /// specific send. The completion handle is delivered only on the `Accepted`
    /// arm — a `Busy` outcome means no turn started, so there is nothing to await.
    ///
    /// **Fail-fast only, by construction.** This path is always `OnBusy::FailFast`
    /// — it never enqueues. That is both the spec's rule (a workflow step *fails
    /// fast* on contention rather than queuing — system-design §7) and what makes
    /// the completion contract airtight: a `FailFast`-accepted send is dispatched
    /// by the actor as its very next action (an idle agent pops it before
    /// servicing any other command; a busy agent returns `Busy` and enqueues
    /// nothing), so an awaited item can never sit in the backlog to be queue-
    /// dropped. The handle therefore resolves with a `CompletionResult` for every
    /// accepted send; the only way it resolves `Err` is the caller dropping its
    /// own receiver. (An awaitable *enqueue* variant would re-introduce the
    /// queue-drop path — if a future milestone needs it, it must teach the queue-
    /// drop paths to fire the completion first.)
    pub async fn send_message_awaiting_completion(
        &self,
        agent_id: AgentId,
        prompt: &str,
        attachments: Vec<Attachment>,
        send_id: SendId,
        factory: Arc<dyn DispatchContextFactory>,
    ) -> AwaitableSendOutcome {
        self.send_awaitable(agent_id, prompt, attachments, send_id, factory, false)
            .await
    }

    /// Like [`send_message_awaiting_completion`](Self::send_message_awaiting_completion),
    /// but also emits a live [`NormalizedEvent::UserMessage`] for this send once it
    /// is durable. Used by **backend-originated workflow sends**, which have no
    /// frontend optimistic user turn: the workflow's dispatched message must still
    /// appear as a user message live (and group a fan-out into columns), exactly
    /// as it does after a reload from the journal. Emitting at the journal boundary
    /// (in the actor, after `record_send` succeeds) keeps live == reload by
    /// construction — a send whose journal write fails shows no live user message.
    pub async fn send_workflow_message_awaiting_completion(
        &self,
        agent_id: AgentId,
        prompt: &str,
        attachments: Vec<Attachment>,
        send_id: SendId,
        factory: Arc<dyn DispatchContextFactory>,
    ) -> AwaitableSendOutcome {
        self.send_awaitable(agent_id, prompt, attachments, send_id, factory, true)
            .await
    }

    async fn send_awaitable(
        &self,
        agent_id: AgentId,
        prompt: &str,
        attachments: Vec<Attachment>,
        send_id: SendId,
        factory: Arc<dyn DispatchContextFactory>,
        emit_user_message: bool,
    ) -> AwaitableSendOutcome {
        let (completion_tx, completion_rx) = oneshot::channel();
        let item = WorkItem {
            message_id: Uuid::now_v7(),
            send_id,
            prompt: prompt.to_owned(),
            attachments,
            completion: Some(completion_tx),
            emit_user_message,
        };
        match self.accept(agent_id, item, factory, OnBusy::FailFast).await {
            SendOutcome::Accepted(message_id) => AwaitableSendOutcome::Accepted {
                message_id,
                completion: completion_rx,
            },
            // `completion_rx` is dropped here — Busy means no turn, no signal.
            SendOutcome::Busy => AwaitableSendOutcome::Busy,
        }
    }

    /// Shared acceptance path for both send entry points. Spawns the actor on
    /// first use, then routes the prebuilt [`WorkItem`] through the
    /// `Enqueue`/`FailFast` decision. The item's `completion` sender (if any)
    /// rides along untouched; a path that drops the item without starting a turn
    /// drops the sender, which the awaiter observes as a failed `recv`.
    async fn accept(
        &self,
        agent_id: AgentId,
        item: WorkItem,
        factory: Arc<dyn DispatchContextFactory>,
        on_busy: OnBusy,
    ) -> SendOutcome {
        let message_id = item.message_id;
        // `None` ⇒ the agent is `Closing` (mid-teardown): reject rather than
        // resurrect it with a fresh actor.
        let Some(commands) = self.ensure_actor(agent_id, Arc::clone(&factory)) else {
            return reject_send(
                message_id,
                agent_id,
                on_busy,
                factory.as_ref(),
                "agent is shutting down",
            );
        };
        match on_busy {
            OnBusy::Enqueue => {
                // The message_id is the receipt; turn lifecycle flows over the
                // event channel. A send error means the actor task is gone
                // (e.g. panicked) while a stale handle lingered — never report a
                // silently-dropped send as cleanly accepted; surface it as a
                // MessageFailed so the optimistic bubble doesn't spin forever.
                if commands
                    .send(Command::Enqueue {
                        item,
                        on_busy,
                        reply: None,
                    })
                    .is_err()
                {
                    emit_message_failed(
                        factory.idle_emitter().as_ref(),
                        &channel_name(agent_id),
                        message_id,
                        // Pre-`record_send`: the actor never received the item.
                        None,
                        agent_id,
                        "agent worker is unavailable",
                    );
                }
                SendOutcome::Accepted(message_id)
            }
            OnBusy::FailFast => {
                let (tx, rx) = oneshot::channel();
                if commands
                    .send(Command::Enqueue {
                        item,
                        on_busy,
                        reply: Some(tx),
                    })
                    .is_err()
                {
                    // Actor gone: fail-fast must not falsely report acceptance.
                    return SendOutcome::Busy;
                }
                rx.await.unwrap_or(SendOutcome::Busy)
            }
        }
    }

    /// Request cancellation of `agent_id`'s in-flight turn, stamping `source`.
    /// Delivered to the actor, which no-ops if no turn is live (idle or already
    /// past its terminal). `NothingToCancel` only when the agent has no actor.
    pub fn cancel(&self, agent_id: AgentId, source: CancelSource) -> CancelOutcome {
        let agents = lock(&self.agents);
        match agents.get(&agent_id) {
            Some(AgentSlot::Active(tx)) if tx.send(Command::Cancel(source)).is_ok() => {
                CancelOutcome::Requested
            }
            _ => CancelOutcome::NothingToCancel,
        }
    }

    /// Cancel a whole send across its `recipients` (system-design §7 "Cancel a
    /// send"). Delivers a send-scoped command to each recipient's actor; each
    /// actor — the single authority over its own running turn + backlog —
    /// cancels its in-flight turn **iff** that turn belongs to `send_id` and
    /// drops any still-queued item of the send, never touching a *later,
    /// unrelated* turn (the TOCTOU a frontend per-agent `cancel_turn` loop
    /// would hit). Fire-and-forget: per-recipient effects arrive as that turn's
    /// synthesized `Cancelled` terminal on the event stream; there is no
    /// aggregate send outcome.
    pub fn cancel_send(&self, send_id: SendId, recipients: &[AgentId], source: CancelSource) {
        let agents = lock(&self.agents);
        for &agent_id in recipients {
            if let Some(AgentSlot::Active(tx)) = agents.get(&agent_id) {
                let _ = tx.send(Command::CancelSend { send_id, source });
            }
        }
    }

    /// Stop an agent: cancel its in-flight turn (if any) **and** drop its entire
    /// queued backlog, leaving the actor alive and idle. Fire-and-forget — the
    /// running turn's effect arrives as its synthesized `Cancelled` terminal, and
    /// dropped queued items are discarded silently (never journaled). `NothingToCancel`
    /// only when the agent has no actor.
    pub fn cancel_agent(&self, agent_id: AgentId, source: CancelSource) -> CancelOutcome {
        let agents = lock(&self.agents);
        match agents.get(&agent_id) {
            Some(AgentSlot::Active(tx)) if tx.send(Command::CancelAgent { source }).is_ok() => {
                CancelOutcome::Requested
            }
            _ => CancelOutcome::NothingToCancel,
        }
    }

    /// Remove a not-yet-dispatched queued message by id, returning its payload
    /// so the UI can restore the composer text. **Race-safe** (the actor is the
    /// single authority): if the id is no longer enqueued — already
    /// dequeued/started, removed, or the agent has no actor — returns
    /// `Err(NotQueued)`.
    pub async fn remove_queued_message(
        &self,
        agent_id: AgentId,
        message_id: MessageId,
    ) -> Result<RemovedQueuedMessage, NotQueued> {
        let commands = {
            let agents = lock(&self.agents);
            match agents.get(&agent_id) {
                Some(AgentSlot::Active(tx)) => tx.clone(),
                // No actor, or shutting down — nothing to remove.
                _ => return Err(NotQueued),
            }
        };
        let (tx, rx) = oneshot::channel();
        if commands
            .send(Command::Remove {
                message_id,
                reply: tx,
            })
            .is_err()
        {
            return Err(NotQueued);
        }
        rx.await.unwrap_or(Err(NotQueued))
    }

    /// Await `agent_id`'s **current in-flight turn** reaching a terminal state —
    /// the per-agent forward-source wait. Resolves [`CurrentTurnWait::Idle`]
    /// immediately when the agent has no actor (never dispatched) or is idle /
    /// between queued turns; otherwise resolves [`CurrentTurnWait::Terminal`]
    /// with the running turn's outcome when it terminates.
    ///
    /// Unlike [`send_message_awaiting_completion`](Self::send_message_awaiting_completion)
    /// this awaits a turn the caller did **not** dispatch (so there is no
    /// completion handle to it), yet still returns the turn's captured text on
    /// `Terminal { Completed }` — the actor captures every turn's text from its
    /// start, so a mid-turn waiter gets the whole output without a disk read. A
    /// lost actor (`Err` on `recv`) is treated as `Idle`.
    ///
    /// **Binding semantics:** resolves for whichever turn is current when the
    /// actor *processes* this request, not when it was sent. An agent with a
    /// queued backlog can therefore bind to a later turn if its visible turn ends
    /// in the window before the actor services the request — a narrow edge
    /// accepted for v1 (forwarding the agent's next reply, not the intended one;
    /// no data loss). Pinning the exact turn would require threading the intended
    /// `turn_id` from the caller, which has no handle to thread.
    pub async fn wait_for_current_turn(&self, agent_id: AgentId) -> CurrentTurnWait {
        let commands = {
            let agents = lock(&self.agents);
            match agents.get(&agent_id) {
                Some(AgentSlot::Active(tx)) => tx.clone(),
                // No actor (never dispatched) or shutting down: no in-flight turn.
                _ => return CurrentTurnWait::Idle,
            }
        };
        let (tx, rx) = oneshot::channel();
        if commands
            .send(Command::WaitForCurrentTurn { reply: tx })
            .is_err()
        {
            return CurrentTurnWait::Idle;
        }
        rx.await.unwrap_or(CurrentTurnWait::Idle)
    }

    /// Whether `agent_id` has a turn **actively running** right now. Non-blocking:
    /// the actor answers immediately (never holds the reply for a running turn),
    /// so completed-only forwarding can reject a still-streaming source without
    /// waiting on it. `false` for an idle/never-dispatched/shutting-down agent.
    pub async fn is_turn_running(&self, agent_id: AgentId) -> bool {
        let commands = {
            let agents = lock(&self.agents);
            match agents.get(&agent_id) {
                Some(AgentSlot::Active(tx)) => tx.clone(),
                _ => return false,
            }
        };
        let (tx, rx) = oneshot::channel();
        if commands
            .send(Command::PeekCurrentTurn { reply: tx })
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Close `agent_id`'s actor atomically: mark the slot `Closing` (so a racing
    /// send is rejected, not resurrected with a fresh actor), tell the actor to
    /// abandon its backlog + cancel any running turn + drain, await its reply,
    /// then drop the slot. The teardown primitive —
    /// `drain_agents_then_release_locks` calls this per agent before releasing
    /// project locks, so a lock is never released while a turn is still driving
    /// the harness session, and no *fresh* turn starts mid-teardown. Dropping
    /// the slot only after the reply lets a *later* send (e.g. project re-open)
    /// create a new actor normally.
    pub async fn shutdown_agent(&self, agent_id: AgentId, source: CancelSource) {
        let sender = {
            let mut agents = lock(&self.agents);
            match agents.get_mut(&agent_id) {
                Some(slot) if matches!(slot, AgentSlot::Active(_)) => {
                    let AgentSlot::Active(tx) = std::mem::replace(slot, AgentSlot::Closing) else {
                        unreachable!("matched Active above")
                    };
                    tx
                }
                // No actor, or already `Closing` (a concurrent shutdown owns the
                // drain + removal) — nothing for this call to do.
                _ => return,
            }
        };
        let (tx, rx) = oneshot::channel();
        if sender.send(Command::Shutdown { source, reply: tx }).is_ok() {
            let _ = rx.await;
        }
        // Actor has stopped — drop the `Closing` slot so the agent can be
        // re-created by a later send.
        lock(&self.agents).remove(&agent_id);
    }

    /// Get the command sender for an agent's actor, spawning it on first use.
    /// Returns `None` if the agent is `Closing` (mid-teardown) — the caller must
    /// reject the send rather than resurrect a torn-down agent.
    fn ensure_actor(
        &self,
        agent_id: AgentId,
        factory: Arc<dyn DispatchContextFactory>,
    ) -> Option<mpsc::UnboundedSender<Command>> {
        let mut agents = lock(&self.agents);
        match agents.get(&agent_id) {
            Some(AgentSlot::Active(tx)) => Some(tx.clone()),
            Some(AgentSlot::Closing) => None,
            None => {
                let (tx, rx) = mpsc::unbounded_channel();
                agents.insert(agent_id, AgentSlot::Active(tx.clone()));
                tokio::spawn(agent_actor(agent_id, factory, rx));
                Some(tx)
            }
        }
    }
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}

/// One agent's actor: the sole owner of its backlog, running-turn cancel state,
/// and dispatch-context builder. Parks on `recv` when idle; drains its backlog
/// one turn at a time; emits `AgentIdle` only when the backlog empties.
async fn agent_actor(
    agent_id: AgentId,
    factory: Arc<dyn DispatchContextFactory>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let channel = channel_name(agent_id);
    let idle_emitter = factory.idle_emitter();
    let mut backlog: VecDeque<WorkItem> = VecDeque::new();
    'main: loop {
        // Idle: park until a command arrives (or all senders drop → exit).
        let Some(cmd) = commands.recv().await else {
            break 'main;
        };
        if let IdleAfter::Shutdown(reply) =
            apply_idle_command(cmd, agent_id, &mut backlog, idle_emitter.as_ref(), &channel)
        {
            abandon_backlog(&mut backlog, idle_emitter.as_ref(), &channel, agent_id);
            let _ = reply.send(());
            break 'main;
        }
        // Busy: drain the backlog one turn at a time. New work / removals /
        // cancels arriving *mid-turn* are handled inside `run_turn` via
        // `select!`. Between turns, absorb any already-delivered commands
        // non-blockingly *before* deciding the agent is idle — so a message
        // enqueued while the prior turn was finishing chains deterministically,
        // with no `AgentIdle` flickering out between the two turns.
        'busy: loop {
            if let Some(item) = backlog.pop_front() {
                match run_turn(
                    agent_id,
                    &channel,
                    &factory,
                    item,
                    &mut commands,
                    &mut backlog,
                )
                .await
                {
                    TurnAfter::Continue => continue 'busy,
                    TurnAfter::Shutdown(reply) => {
                        abandon_backlog(&mut backlog, idle_emitter.as_ref(), &channel, agent_id);
                        let _ = reply.send(());
                        break 'main;
                    }
                    TurnAfter::ChannelClosed => {
                        // Process exit (all senders dropped mid-turn). No
                        // frontend to notify — just abandon and stop.
                        backlog.clear();
                        break 'main;
                    }
                }
            }
            match commands.try_recv() {
                Ok(cmd) => {
                    if let IdleAfter::Shutdown(reply) = apply_idle_command(
                        cmd,
                        agent_id,
                        &mut backlog,
                        idle_emitter.as_ref(),
                        &channel,
                    ) {
                        abandon_backlog(&mut backlog, idle_emitter.as_ref(), &channel, agent_id);
                        let _ = reply.send(());
                        break 'main;
                    }
                    // Continue 'busy — the command may have pushed to the backlog.
                }
                // Backlog empty and no command waiting → genuinely idle.
                Err(mpsc::error::TryRecvError::Empty) => break 'busy,
                // All senders dropped → process exit.
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    backlog.clear();
                    break 'main;
                }
            }
        }
        emit_event(
            idle_emitter.as_ref(),
            &channel,
            &NormalizedEvent::AgentIdle { agent_id },
            agent_id,
        );
    }
}

/// Emit a `MessageFailed` for every still-queued message and clear the backlog —
/// used when an agent is torn down (`Shutdown`) so a queued-but-unstarted
/// message doesn't leave its optimistic UI bubble spinning forever. (These
/// messages are in-memory and abandoned on teardown by design; this just makes
/// the abandonment visible rather than silent.)
fn abandon_backlog(
    backlog: &mut VecDeque<WorkItem>,
    emitter: &dyn EventEmitter,
    channel: &str,
    agent_id: AgentId,
) {
    for mut item in backlog.drain(..) {
        resolve_dropped_completion(
            &mut item,
            TurnOutcome::Cancelled {
                source: CancelSource::Shutdown,
            },
        );
        emit_message_failed(
            emitter,
            channel,
            item.message_id,
            // A never-run (dropped) item — no durable record.
            None,
            agent_id,
            "agent is shutting down",
        );
    }
}

/// What the actor's *idle* command handler decided.
enum IdleAfter {
    Continue,
    Shutdown(oneshot::Sender<()>),
}

/// Handle a command received while no turn is running.
fn apply_idle_command(
    cmd: Command,
    agent_id: AgentId,
    backlog: &mut VecDeque<WorkItem>,
    emitter: &dyn EventEmitter,
    channel: &str,
) -> IdleAfter {
    match cmd {
        Command::Enqueue {
            item,
            on_busy: _,
            reply,
        } => {
            // Idle ⇒ nothing is running and the backlog drains fully before we
            // park, so FailFast is satisfiable: accept.
            let message_id = item.message_id;
            backlog.push_back(item);
            if let Some(reply) = reply {
                let _ = reply.send(SendOutcome::Accepted(message_id));
            }
            IdleAfter::Continue
        }
        Command::Remove { message_id, reply } => {
            let _ = reply.send(remove_from_backlog(backlog, agent_id, message_id));
            IdleAfter::Continue
        }
        // No turn live (idle, or between queued turns) ⇒ nothing to wait on; the
        // source's latest completed output, if any, is already on disk.
        Command::WaitForCurrentTurn { reply } => {
            let _ = reply.send(CurrentTurnWait::Idle);
            IdleAfter::Continue
        }
        // No turn live ⇒ not running.
        Command::PeekCurrentTurn { reply } => {
            let _ = reply.send(false);
            IdleAfter::Continue
        }
        // No turn live ⇒ cancel is a no-op.
        Command::Cancel(_) => IdleAfter::Continue,
        // No turn live ⇒ only this send's *queued* items can exist; drop them
        // and emit MessageCancelled for each.
        Command::CancelSend { send_id, source } => {
            drop_queued_send(backlog, send_id, source, emitter, channel, agent_id);
            IdleAfter::Continue
        }
        // No turn live ⇒ drop the whole backlog (the actor stays alive),
        // emitting MessageCancelled for each dropped send.
        Command::CancelAgent { source } => {
            drop_all_queued(backlog, source, emitter, channel, agent_id);
            IdleAfter::Continue
        }
        // The caller (`agent_actor`) abandons the backlog (emitting
        // `MessageFailed` for each queued message) before replying.
        Command::Shutdown { source: _, reply } => IdleAfter::Shutdown(reply),
    }
}

/// What `run_turn` decided after the turn settled.
enum TurnAfter {
    /// Keep draining the backlog (turn completed/failed/cancelled normally).
    Continue,
    /// A `Shutdown` was received mid-turn; the turn was cancelled and drained.
    /// The actor should abandon the backlog, reply, and exit.
    Shutdown(oneshot::Sender<()>),
    /// All command senders dropped mid-turn (process exit). The turn was
    /// cancelled and drained; the actor should stop with no reply target.
    ChannelClosed,
}

/// Run one turn to completion, multiplexing the adapter event stream against
/// incoming commands. Builds the dispatch context (reading per-dispatch state
/// live), journals the send fail-closed, dispatches, emits `TurnStart`, then
/// drains — handling `Cancel`/`Enqueue`/`Remove`/`Shutdown` promptly via
/// `select!`.
// A single linear turn lifecycle (build → journal → user-message → dispatch →
// emit → drain); splitting it would scatter the fail-closed ordering it must
// keep in one place.
#[allow(clippy::too_many_lines)]
async fn run_turn(
    agent_id: AgentId,
    channel: &str,
    factory: &Arc<dyn DispatchContextFactory>,
    item: WorkItem,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    backlog: &mut VecDeque<WorkItem>,
) -> TurnAfter {
    let DispatchContext {
        adapter,
        cwd,
        agent,
        emitter,
        mut options,
        journal,
        metadata,
        locator_sink,
    } = factory.build(item.send_id);
    // Take the completion sender out of the item so the failure paths below can
    // fire it on early return; on success it is handed to `drain_turn`, which
    // fires it at the turn's terminal. Partial move — the remaining `item`
    // fields stay accessible.
    let mut completion = item.completion;
    let turn_id: TurnId = Uuid::now_v7();
    let started_at = Utc::now();

    // Fail-closed: journal the send before spawning. On failure, no turn starts,
    // no outcome marker (the journal is what's broken — a marker would orphan),
    // and we surface MessageFailed. Advance the backlog regardless.
    if let Err(e) = journal.record_send(
        turn_id,
        agent_id,
        &item.prompt,
        &item.attachments,
        started_at,
    ) {
        emit_message_failed(
            emitter.as_ref(),
            channel,
            item.message_id,
            // The journal write itself failed — there is no durable send for reload
            // to reconstruct, so the frontend must not invent a row.
            None,
            agent_id,
            &e.to_string(),
        );
        // An awaited send must not hang because its turn never started.
        fire_completion(
            &mut completion,
            TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message: e.to_string(),
            },
            String::new(),
        );
        return TurnAfter::Continue;
    }

    // The send is now durable (journaled above). For a backend-originated send (a
    // workflow `send`, which has no frontend optimistic user turn), surface its
    // message as a live user message before the turn starts — so the live view
    // matches the reloaded journal view. Emitting *past* `record_send` means a
    // journal-write failure (handled above) shows no live user message either.
    if item.emit_user_message {
        emit_user_message(
            emitter.as_ref(),
            channel,
            item.send_id,
            &item.prompt,
            started_at,
            agent_id,
        );
    }

    let token = CancellationToken::new();
    options.cancel_token = token.clone();
    // Clean prompt is journaled (above) and queued; the agent-facing footer of
    // `label: <absolute path>` lines is appended only here, at the dispatch
    // boundary, so adapters stay attachment-unaware. Empty attachments → the
    // prompt is returned unchanged.
    let dispatch_prompt = render_prompt_with_attachments(&item.prompt, &item.attachments);
    let stream = match adapter
        .dispatch(&agent, &cwd, &dispatch_prompt, turn_id, options)
        .await
    {
        Ok(stream) => stream,
        Err(e) => {
            // Send is journaled but the turn never started: record a Failed
            // marker against the minted turn_id (intentional — restart shows a
            // failed turn, not an orphan user message) and surface MessageFailed.
            let message = e.to_string();
            let outcome = TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message: message.clone(),
            };
            journal.record_outcome(turn_id, agent_id, &outcome, started_at, Utc::now());
            emit_message_failed(
                emitter.as_ref(),
                channel,
                item.message_id,
                // Post-`record_send`: the send is durable (and a `Failed` outcome
                // was just recorded), so reload reconstructs user message + marker
                // — carry the send_id so live attaches the marker identically.
                Some(item.send_id),
                agent_id,
                &message,
            );
            fire_completion(&mut completion, outcome, String::new());
            return TurnAfter::Continue;
        }
    };

    emit_event(
        emitter.as_ref(),
        channel,
        &NormalizedEvent::TurnStart {
            turn_id,
            message_id: item.message_id,
            send_id: item.send_id,
            started_at,
        },
        agent_id,
    );

    drain_turn(
        agent_id,
        channel,
        turn_id,
        item.send_id,
        started_at,
        &emitter,
        &journal,
        &metadata,
        &locator_sink,
        &token,
        completion,
        stream,
        commands,
        backlog,
    )
    .await
}

/// Drain a single turn's event stream while multiplexing commands. Returns
/// whether the actor should keep draining its backlog or shut down.
// The command-multiplexing select! loop is one cohesive state machine; splitting
// it to satisfy the line/arg pedantic lints would scatter the cancel/enqueue/
// shutdown handling that must be read together.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn drain_turn(
    agent_id: AgentId,
    channel: &str,
    turn_id: TurnId,
    running_send_id: SendId,
    started_at: DateTime<Utc>,
    emitter: &Arc<dyn EventEmitter>,
    journal: &Arc<dyn ConversationJournal>,
    metadata: &Arc<dyn MetadataCache>,
    locator_sink: &Arc<dyn SessionLocatorSink>,
    token: &CancellationToken,
    completion: Option<oneshot::Sender<CompletionResult>>,
    mut stream: EventStream,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    backlog: &mut VecDeque<WorkItem>,
) -> TurnAfter {
    // Everything that must be fired at this turn's terminal: the per-send
    // completion handle (if any) plus any per-agent current-turn waiters that
    // register mid-turn. Both fire together at every terminal-synthesis point.
    let mut awaiters = TurnAwaiters {
        completion,
        current_turn: Vec::new(),
    };
    let mut terminal_seen = false;
    // The terminal outcome + captured text, stashed once observed so a
    // `WaitForCurrentTurn` arriving *after* the terminal but before the stream
    // ends (the post-terminal enrichment-drain window) is answered immediately
    // with the same payload rather than registered against a turn that already
    // fired its awaiters.
    let mut terminal: Option<(TurnOutcome, String)> = None;
    let mut cancel_source: Option<CancelSource> = None;
    let mut shutdown_reply: Option<oneshot::Sender<()>> = None;
    let mut channel_closed = false;
    // Accumulated `Text`-kind output for the turn — the text a forward/aggregate
    // consumer reads, delivered to both the completion handle and the
    // current-turn waiters. Captured for **every** turn from its start, not only
    // awaited ones: a current-turn waiter (manual forward) registers *mid-turn*,
    // so the prefix it would otherwise miss must already be buffered. The cost is
    // one bounded `String` per in-flight turn — negligible for a desktop app, and
    // it removes the disk-flush race the manual path would otherwise hit.
    // `Thinking` text and tool output are deliberately excluded.
    let mut captured_text = String::new();
    // Set when the dispatcher itself synthesizes a `Failed` terminal (a
    // load-bearing locator-persist failure). Distinct from the cancel latch:
    // cancellation deliberately still forwards buffered content (system-design
    // §7) and a normal terminal still forwards Codex post-terminal enrichment —
    // but a force-failed turn is authoritatively over, so **every** subsequent
    // adapter event is dropped (the adapter may still be mid-stream, e.g.
    // Antigravity's post-exit drain emits content + `SessionMeta` after the
    // capture). We keep looping only to service commands and let the child exit.
    let mut force_failed = false;

    loop {
        tokio::select! {
            // Bias the stream so buffered events drain promptly; commands are
            // rare. (Not required for correctness — both arms are cancel-safe.)
            biased;

            maybe_event = stream.next() => {
                let Some(event) = maybe_event else { break };
                // A force-failed turn is authoritatively over — drop every
                // further adapter event (content, meta, terminal) rather than
                // forward output for a turn the UI already saw fail.
                if force_failed {
                    continue;
                }
                // Accumulate the turn's text output (see the `captured_text`
                // declaration for why this is always-on). Only `Text` kind, no
                // `Thinking`; tool output never arrives as a `ContentChunk`.
                if let AdapterEvent::ContentChunk { kind: ContentKind::Text, text, .. } = &event {
                    captured_text.push_str(text);
                }
                // Internal adapter→dispatcher event: persist the runtime-captured
                // session locator to the **running turn's** agent (never an
                // event-supplied id — see `SessionLocatorCaptured`). Load-bearing:
                // a lost locator silently starts a fresh session next turn, so a
                // persist failure fails the turn and tears the child down via the
                // cancel token. Never forwarded to the frontend.
                if let AdapterEvent::SessionLocatorCaptured { locator } = &event {
                    if let Err(e) = locator_sink.persist(agent_id, locator.clone()) {
                        if terminal_seen {
                            // Can't fail an already-terminal turn; the locator is
                            // lost but the turn outcome stands. Surface loudly.
                            tracing::error!(
                                agent_id = %agent_id, %turn_id, error = %e,
                                "session locator persist failed after the turn's terminal — locator lost"
                            );
                        } else {
                            // A force-failed turn produced no forwardable output.
                            let outcome = TurnOutcome::Failed {
                                kind: FailureKind::AdapterFailure,
                                message: format!("failed to persist session locator: {e}"),
                            };
                            synthesize_terminal(
                                emitter.as_ref(),
                                channel,
                                turn_id,
                                agent_id,
                                started_at,
                                journal.as_ref(),
                                &mut awaiters,
                                &outcome,
                                "",
                            );
                            terminal_seen = true;
                            terminal = Some((outcome, String::new()));
                            force_failed = true;
                            // Tear down the child; the adapter may still be
                            // mid-stream (Antigravity's post-exit drain), and
                            // `force_failed` drops anything it emits from here on.
                            token.cancel();
                        }
                    }
                    continue;
                }
                if let AdapterEvent::TurnEnd { outcome, ended_at, .. } = &event {
                    // Cancellation latch: once cancel fired, the actor owns the
                    // terminal — drop a real terminal that races in afterwards
                    // so the synthesized Cancelled wins.
                    if token.is_cancelled() {
                        continue;
                    }
                    terminal_seen = true;
                    if !matches!(outcome, TurnOutcome::Completed) {
                        journal.record_outcome(turn_id, agent_id, outcome, started_at, *ended_at);
                    }
                    // Fire the awaited-send + current-turn waiters with the real
                    // outcome. Only a completed turn carries forwardable text; a
                    // failed terminal has none.
                    let text = if matches!(outcome, TurnOutcome::Completed) {
                        std::mem::take(&mut captured_text)
                    } else {
                        String::new()
                    };
                    awaiters.fire(outcome, &text);
                    terminal = Some((outcome.clone(), text));
                }
                // Persist stream-only (class-C) rate-limit snapshots so they
                // survive an app restart. The gate is on the event's `source`,
                // not the harness — keeping the dispatcher harness-agnostic.
                // Session-file-backed payloads (Codex) are already durable on
                // disk and are not re-persisted. Best-effort: the cache logs
                // and swallows write errors, so this never blocks the turn.
                if let AdapterEvent::RateLimitEvent { agent_id: a, info, source } = &event
                    && *source == RateLimitSource::StreamOnly
                {
                    metadata.record_rate_limit(*a, info.clone(), Utc::now());
                }
                // Persist the stream-only context window so the context bar
                // survives restart. Same source-gated, harness-agnostic posture
                // as the rate-limit snapshot above: only `StreamOnly` (Claude's
                // `result.modelUsage`, class C) is persisted; `SessionFileBacked`
                // (Codex, class B) is already durable in the harness file and is
                // NOT shadow-cached. Keyed on the running turn's `agent_id` since
                // `TurnEnd` carries none. A cancelled terminal already `continue`d
                // above, so this only fires for a real completed/failed turn.
                if let AdapterEvent::TurnEnd {
                    usage: Some(usage),
                    context_window_source: Some(ContextWindowSource::StreamOnly),
                    ..
                } = &event
                    && let Some(context_window) = usage.context_window
                {
                    metadata.record_context_window(agent_id, context_window, Utc::now());
                }
                // Persist the turn's stream-only cost + overage so the inline
                // cost / "using credits" marker re-attaches to the right message
                // on reopen. Gate on a join key being present AND the turn being
                // real-spend — not on harness identity (Claude is the only
                // harness that derives a key + real-spend today, so the gate is
                // self-selecting and the dispatcher stays harness-agnostic). A
                // cancelled terminal already `continue`d above, so this only
                // fires for a real completed/failed turn. Best-effort: the cache
                // logs and swallows write errors.
                if let AdapterEvent::TurnEnd {
                    usage,
                    spend: Some(spend),
                    stable_message_id: Some(message_id),
                    ..
                } = &event
                    && spend.real_spend
                {
                    metadata.record_turn_spend(
                        agent_id,
                        message_id.clone(),
                        usage.as_ref().and_then(|u| u.total_cost_usd),
                        spend.clone(),
                        Utc::now(),
                    );
                }
                // Capture events are handled above and never reach here, so this
                // always yields `Some`; the `if let` keeps the contract explicit.
                if let Some(normalized) = event.into_normalized() {
                    emit_event(emitter.as_ref(), channel, &normalized, agent_id);
                }
            }

            maybe_cmd = commands.recv() => {
                match maybe_cmd {
                    None => {
                        // All senders dropped while a turn ran (process exit) —
                        // cancel and drain, then signal ChannelClosed so the
                        // actor stops (no reply target, backlog abandoned).
                        channel_closed = true;
                        if !terminal_seen && cancel_source.is_none() {
                            cancel_source = Some(CancelSource::Shutdown);
                            token.cancel();
                        }
                    }
                    Some(Command::Cancel(src)) => {
                        // Post-terminal cancel is a no-op (token-presence guard).
                        if !terminal_seen {
                            cancel_source.get_or_insert(src);
                            token.cancel();
                        }
                    }
                    Some(Command::Enqueue { item, on_busy, reply }) => {
                        match on_busy {
                            OnBusy::FailFast => {
                                // FailFast normally refuses while a turn runs. But once
                                // our own turn has reached terminal and we're only
                                // draining post-terminal enrichment events with nothing
                                // else queued, the agent is effectively free — accept and
                                // let the new turn start when drain finishes, rather than
                                // spuriously failing a back-to-back same-agent send
                                // (the completion signal fires at terminal, before the
                                // actor parks idle). Genuine contention — a turn still
                                // mid-flight (terminal not seen) or other work already
                                // queued — still returns Busy, preserving the
                                // workflow "contention = step failure" rule.
                                if terminal_seen && backlog.is_empty() {
                                    let message_id = item.message_id;
                                    backlog.push_back(item);
                                    if let Some(reply) = reply {
                                        let _ = reply.send(SendOutcome::Accepted(message_id));
                                    }
                                } else if let Some(reply) = reply {
                                    let _ = reply.send(SendOutcome::Busy);
                                }
                            }
                            OnBusy::Enqueue => {
                                let message_id = item.message_id;
                                backlog.push_back(item);
                                if let Some(reply) = reply {
                                    let _ = reply.send(SendOutcome::Accepted(message_id));
                                }
                            }
                        }
                    }
                    Some(Command::Remove { message_id, reply }) => {
                        let _ = reply.send(remove_from_backlog(backlog, agent_id, message_id));
                    }
                    Some(Command::WaitForCurrentTurn { reply }) => {
                        // Mid-turn: register to fire at this turn's terminal. If
                        // the terminal already passed (we're draining post-terminal
                        // enrichment), answer immediately with the stashed outcome
                        // + text — registering would strand the caller.
                        match &terminal {
                            Some((outcome, text)) => {
                                let _ = reply.send(CurrentTurnWait::Terminal {
                                    outcome: outcome.clone(),
                                    text: text.clone(),
                                });
                            }
                            None => awaiters.current_turn.push(reply),
                        }
                    }
                    // Mid-turn peek: running iff the terminal hasn't passed yet.
                    Some(Command::PeekCurrentTurn { reply }) => {
                        let _ = reply.send(terminal.is_none());
                    }
                    Some(Command::CancelSend { send_id, source }) => {
                        // Fire the running turn's cancel token only if this turn
                        // belongs to the send (post-terminal cancel is a no-op
                        // via the token-presence guard); either way drop any
                        // still-queued items of the same send. A send maps to at
                        // most one item per agent, so these are mutually
                        // exclusive in practice.
                        if running_send_id == send_id && !terminal_seen {
                            cancel_source.get_or_insert(source);
                            token.cancel();
                        }
                        drop_queued_send(backlog, send_id, source, emitter.as_ref(), channel, agent_id);
                    }
                    Some(Command::CancelAgent { source }) => {
                        // Stop the agent: cancel the running turn (post-terminal
                        // cancel no-ops via the token-presence guard) and drop the
                        // whole backlog. Unlike Shutdown, the actor keeps running —
                        // it drains this turn to its terminal then parks idle.
                        if !terminal_seen {
                            cancel_source.get_or_insert(source);
                            token.cancel();
                        }
                        drop_all_queued(backlog, source, emitter.as_ref(), channel, agent_id);
                    }
                    Some(Command::Shutdown { source, reply }) => {
                        // Cancel the running turn and keep draining to its
                        // terminal; the actor abandons the backlog (emitting
                        // MessageFailed per queued message) and replies once this
                        // returns.
                        if !terminal_seen {
                            cancel_source.get_or_insert(source);
                            token.cancel();
                        }
                        shutdown_reply = Some(reply);
                    }
                }
            }
        }
    }

    if !terminal_seen && token.is_cancelled() {
        // Adapter ended the stream on cancel without a terminal — the actor owns
        // the cancelled terminal. `cancel_source` is set whenever the token was
        // fired here, so `unwrap_or` covers only the structurally-unreachable
        // case.
        let source = cancel_source.unwrap_or(CancelSource::User);
        // A cancelled turn never completed — no forwardable output.
        synthesize_terminal(
            emitter.as_ref(),
            channel,
            turn_id,
            agent_id,
            started_at,
            journal.as_ref(),
            &mut awaiters,
            &TurnOutcome::Cancelled { source },
            "",
        );
        terminal_seen = true;
    }

    if !terminal_seen {
        // The stream ended with neither a terminal nor a fired cancel — an
        // adapter-contract violation. Synthesize a `Failed { AdapterFailure }`
        // terminal (consistent with the dispatch-failure path) so there is one
        // truncation semantics for every send: the awaited path's caller can't
        // be stranded, and the compose-bar path renders a failed turn rather
        // than silently returning to idle (which read as a successful empty
        // turn). With no awaiters, `synthesize_terminal`'s firing has nothing to
        // send, so the non-awaited path effectively pays only the emit + journal.
        tracing::warn!(
            agent_id = %agent_id,
            channel = %channel,
            "turn stream ended without a terminal TurnEnd and without a fired cancel token — adapter contract violation; synthesizing a failed terminal"
        );
        synthesize_terminal(
            emitter.as_ref(),
            channel,
            turn_id,
            agent_id,
            started_at,
            journal.as_ref(),
            &mut awaiters,
            &TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message: "turn stream ended without a terminal event".to_owned(),
            },
            "",
        );
    }

    match shutdown_reply {
        Some(reply) => TurnAfter::Shutdown(reply),
        None if channel_closed => TurnAfter::ChannelClosed,
        None => TurnAfter::Continue,
    }
}

/// Fire an awaited send's completion signal exactly once. Takes the sender so a
/// second call is a no-op; a dropped receiver (caller stopped awaiting) makes
/// `send` return `Err`, which is ignored — consistent with how the actor treats
/// every other dropped reply channel.
fn fire_completion(
    completion: &mut Option<oneshot::Sender<CompletionResult>>,
    outcome: TurnOutcome,
    text: String,
) {
    if let Some(tx) = completion.take() {
        let _ = tx.send(CompletionResult { outcome, text });
    }
}

/// Emit a **dispatcher-synthesized** terminal `TurnEnd`, journal its outcome, and
/// fire the turn's awaiters — the three things every synthesized terminal must do
/// together. Centralized so a new synthesized-terminal path (a future milestone)
/// can't emit-or-journal-but-forget-to-fire: calling this does all three by
/// construction. A synthesized terminal carries no harness usage/cost/model/effort
/// and no `hydration_key` (there is no harness turn id behind it).
///
/// This is **only** for terminals the dispatcher invents (cancel, force-fail,
/// stream-truncation). A real adapter `TurnEnd` is forwarded verbatim through
/// `into_normalized` and fires its awaiters inline — it must not be re-emitted
/// here. With no awaiters present the firing has nothing to send, so the
/// non-awaited compose-bar path effectively pays only the emit + journal.
#[allow(clippy::too_many_arguments)]
fn synthesize_terminal(
    emitter: &dyn EventEmitter,
    channel: &str,
    turn_id: TurnId,
    agent_id: AgentId,
    started_at: DateTime<Utc>,
    journal: &dyn ConversationJournal,
    awaiters: &mut TurnAwaiters,
    outcome: &TurnOutcome,
    text: &str,
) {
    let ended_at = Utc::now();
    emit_event(
        emitter,
        channel,
        &NormalizedEvent::TurnEnd {
            turn_id,
            outcome: outcome.clone(),
            ended_at,
            usage: None,
            spend: None,
            model: None,
            effort: None,
            hydration_key: None,
        },
        agent_id,
    );
    journal.record_outcome(turn_id, agent_id, outcome, started_at, ended_at);
    awaiters.fire(outcome, text);
}

/// Remove a queued message by id from an agent's backlog, returning its payload.
fn remove_from_backlog(
    backlog: &mut VecDeque<WorkItem>,
    agent_id: AgentId,
    message_id: MessageId,
) -> Result<RemovedQueuedMessage, NotQueued> {
    let pos = backlog
        .iter()
        .position(|m| m.message_id == message_id)
        .ok_or(NotQueued)?;
    let mut item = backlog
        .remove(pos)
        .expect("position from iter is in bounds");
    // A removed queued message is a user action; resolve any awaitable completion
    // so an accepted awaitable that was removed before running doesn't strand its
    // awaiter (compose-bar items carry no completion, so this is usually a no-op).
    resolve_dropped_completion(
        &mut item,
        TurnOutcome::Cancelled {
            source: CancelSource::User,
        },
    );
    Ok(RemovedQueuedMessage {
        agent_id,
        send_id: item.send_id,
        prompt: item.prompt,
        attachments: item.attachments,
    })
}

fn channel_name(agent_id: AgentId) -> String {
    format!("agent:{agent_id}")
}

/// Serialize and emit one `NormalizedEvent`. Log-and-skip on the cosmic
/// serialization failure rather than panic the actor — well-formed events are
/// pure data and never fail to serialize.
fn emit_event(
    emitter: &dyn EventEmitter,
    channel: &str,
    event: &NormalizedEvent,
    agent_id: AgentId,
) {
    match serde_json::to_value(event) {
        Ok(payload) => emitter.emit(channel, payload),
        Err(e) => tracing::error!(
            agent_id = %agent_id,
            error = %e,
            "failed to serialize event — skipping emit (should be unreachable)"
        ),
    }
}

/// Reject a send for an agent that is shutting down (or otherwise can't accept
/// it). On the `Enqueue` path the receipt `message_id` is still returned, but a
/// `MessageFailed` is emitted so the optimistic UI bubble fails rather than
/// spinning; on `FailFast` the caller is told `Busy` (never falsely `Accepted`).
fn reject_send(
    message_id: MessageId,
    agent_id: AgentId,
    on_busy: OnBusy,
    factory: &dyn DispatchContextFactory,
    reason: &str,
) -> SendOutcome {
    match on_busy {
        OnBusy::Enqueue => {
            emit_message_failed(
                factory.idle_emitter().as_ref(),
                &channel_name(agent_id),
                message_id,
                // Rejected before the actor ran it — no durable record.
                None,
                agent_id,
                reason,
            );
            SendOutcome::Accepted(message_id)
        }
        OnBusy::FailFast => SendOutcome::Busy,
    }
}

/// Emit a `MessageFailed` for a send that could not start a turn.
/// Emit a live [`NormalizedEvent::UserMessage`] for a backend-originated send,
/// once it is durable. The frontend renders it as a user turn (grouping a fan-out
/// by `send_id`), matching a manual send and the reloaded journal view.
fn emit_user_message(
    emitter: &dyn EventEmitter,
    channel: &str,
    send_id: SendId,
    text: &str,
    at: DateTime<Utc>,
    agent_id: AgentId,
) {
    emit_event(
        emitter,
        channel,
        &NormalizedEvent::UserMessage {
            send_id,
            text: text.to_owned(),
            at,
        },
        agent_id,
    );
}

fn emit_message_failed(
    emitter: &dyn EventEmitter,
    channel: &str,
    message_id: MessageId,
    // `Some` only when the send was durably recorded (the post-`record_send`
    // adapter-launch-failure path); `None` for every pre-durable failure, so the
    // frontend invents no transcript row for a send reload can't reconstruct.
    send_id: Option<SendId>,
    agent_id: AgentId,
    error: &str,
) {
    emit_event(
        emitter,
        channel,
        &NormalizedEvent::MessageFailed {
            message_id,
            send_id,
            agent_id,
            error: error.to_owned(),
            at: Utc::now(),
        },
        agent_id,
    );
}

fn emit_message_cancelled(
    emitter: &dyn EventEmitter,
    channel: &str,
    message_id: MessageId,
    agent_id: AgentId,
) {
    emit_event(
        emitter,
        channel,
        &NormalizedEvent::MessageCancelled {
            message_id,
            agent_id,
            at: Utc::now(),
        },
        agent_id,
    );
}

/// Drop the queued backlog items of `send_id`, emitting `MessageCancelled` for
/// each — the authoritative signal that a not-yet-started send is gone, so the
/// frontend renders its cancellation instead of optimistically guessing.
fn drop_queued_send(
    backlog: &mut VecDeque<WorkItem>,
    send_id: SendId,
    source: CancelSource,
    emitter: &dyn EventEmitter,
    channel: &str,
    agent_id: AgentId,
) {
    backlog.retain_mut(|m| {
        if m.send_id == send_id {
            resolve_dropped_completion(m, TurnOutcome::Cancelled { source });
            emit_message_cancelled(emitter, channel, m.message_id, agent_id);
            false
        } else {
            true
        }
    });
}

/// Drop *all* queued backlog items (the `CancelAgent` "stop agent" path),
/// emitting `MessageCancelled` for each.
fn drop_all_queued(
    backlog: &mut VecDeque<WorkItem>,
    source: CancelSource,
    emitter: &dyn EventEmitter,
    channel: &str,
    agent_id: AgentId,
) {
    for mut item in backlog.drain(..) {
        resolve_dropped_completion(&mut item, TurnOutcome::Cancelled { source });
        emit_message_cancelled(emitter, channel, item.message_id, agent_id);
    }
}

/// Resolve a queued item's awaitable completion (if any) with `outcome` before
/// the item is dropped, so a `send_message_awaiting_completion` caller whose item
/// was accepted into the backlog and then cancelled / abandoned receives a real
/// [`CompletionResult`] rather than a `RecvError`. The post-terminal-drain accept
/// in the `FailFast` enqueue path can briefly queue an awaitable item, so every
/// drop path must honor this — it keeps "an accepted awaitable always resolves"
/// universally true. A no-op for the compose-bar path (no completion sender).
fn resolve_dropped_completion(item: &mut WorkItem, outcome: TurnOutcome) {
    if let Some(tx) = item.completion.take() {
        let _ = tx.send(CompletionResult {
            outcome,
            text: String::new(),
        });
    }
}

/// Recover from `Mutex` poisoning. The only holders are short O(1) map edits
/// that don't panic, so this is defensive only.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// Behavior is exercised from `crates/dispatcher/tests/dispatcher_with_mock.rs`
// (a Cargo integration test compiled against this crate as an external
// consumer) — driving the public command API and asserting on the recorded
// event stream, the same surface the real frontend observes.
