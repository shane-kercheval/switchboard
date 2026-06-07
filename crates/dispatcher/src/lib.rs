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
    AdapterEvent, CancelSource, ContextWindowSource, DispatchOptions, EventStream, FailureKind,
    HarnessAdapter, MessageId, NormalizedEvent, RateLimitSource, TurnId, TurnOutcome, TurnSpend,
};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// A message accepted into an agent's queue (or run immediately). Carries only
/// what cannot be recomputed when the turn finally starts — the adapter, cwd,
/// emitter, options, and journal are rebuilt by the actor's
/// [`DispatchContextFactory`] at start time, so per-dispatch state is never
/// frozen at enqueue.
#[derive(Debug, Clone)]
struct WorkItem {
    message_id: MessageId,
    send_id: SendId,
    /// The **clean** prompt (no attachment footer). The agent-facing footer is
    /// rendered from `attachments` at dispatch time, so the queued item stays
    /// the user's literal text.
    prompt: String,
    attachments: Vec<Attachment>,
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
    pub async fn send_message(
        &self,
        agent_id: AgentId,
        prompt: &str,
        attachments: Vec<Attachment>,
        send_id: SendId,
        factory: Arc<dyn DispatchContextFactory>,
        on_busy: OnBusy,
    ) -> SendOutcome {
        let message_id = Uuid::now_v7();
        let item = WorkItem {
            message_id,
            send_id,
            prompt: prompt.to_owned(),
            attachments,
        };
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
    for item in backlog.drain(..) {
        emit_message_failed(
            emitter,
            channel,
            item.message_id,
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
        // No turn live ⇒ cancel is a no-op.
        Command::Cancel(_) => IdleAfter::Continue,
        // No turn live ⇒ only this send's *queued* items can exist; drop them
        // and emit MessageCancelled for each.
        Command::CancelSend { send_id, source: _ } => {
            drop_queued_send(backlog, send_id, emitter, channel, agent_id);
            IdleAfter::Continue
        }
        // No turn live ⇒ drop the whole backlog (the actor stays alive),
        // emitting MessageCancelled for each dropped send.
        Command::CancelAgent { source: _ } => {
            drop_all_queued(backlog, emitter, channel, agent_id);
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
            agent_id,
            &e.to_string(),
        );
        return TurnAfter::Continue;
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
            journal.record_outcome(
                turn_id,
                agent_id,
                &TurnOutcome::Failed {
                    kind: FailureKind::AdapterFailure,
                    message: e.to_string(),
                },
                started_at,
                Utc::now(),
            );
            emit_message_failed(
                emitter.as_ref(),
                channel,
                item.message_id,
                agent_id,
                &e.to_string(),
            );
            return TurnAfter::Continue;
        }
    };

    emit_event(
        emitter.as_ref(),
        channel,
        &NormalizedEvent::TurnStart {
            turn_id,
            message_id: item.message_id,
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
    mut stream: EventStream,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    backlog: &mut VecDeque<WorkItem>,
) -> TurnAfter {
    let mut terminal_seen = false;
    let mut cancel_source: Option<CancelSource> = None;
    let mut shutdown_reply: Option<oneshot::Sender<()>> = None;
    let mut channel_closed = false;
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
                            let ended_at = Utc::now();
                            let outcome = TurnOutcome::Failed {
                                kind: FailureKind::AdapterFailure,
                                message: format!("failed to persist session locator: {e}"),
                            };
                            emit_event(
                                emitter.as_ref(),
                                channel,
                                &NormalizedEvent::TurnEnd {
                                    turn_id,
                                    outcome: outcome.clone(),
                                    ended_at,
                                    usage: None,
                                    spend: None,
                                    model: None,
                                    effort: None,
                                    // Synthesized failure — no harness turn id.
                                    hydration_key: None,
                                },
                                agent_id,
                            );
                            journal.record_outcome(turn_id, agent_id, &outcome, started_at, ended_at);
                            terminal_seen = true;
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
                                // A turn is running ⇒ busy.
                                if let Some(reply) = reply { let _ = reply.send(SendOutcome::Busy); }
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
                        drop_queued_send(backlog, send_id, emitter.as_ref(), channel, agent_id);
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
                        drop_all_queued(backlog, emitter.as_ref(), channel, agent_id);
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
        let ended_at = Utc::now();
        let outcome = TurnOutcome::Cancelled { source };
        emit_event(
            emitter.as_ref(),
            channel,
            &NormalizedEvent::TurnEnd {
                turn_id,
                outcome: outcome.clone(),
                ended_at,
                usage: None,
                // A cancelled turn never completed — no cost/overage to attribute.
                spend: None,
                model: None,
                effort: None,
                // Synthesized cancelled terminal — no harness turn id.
                hydration_key: None,
            },
            agent_id,
        );
        journal.record_outcome(turn_id, agent_id, &outcome, started_at, ended_at);
        terminal_seen = true;
    }

    if !terminal_seen {
        tracing::warn!(
            agent_id = %agent_id,
            channel = %channel,
            "turn stream ended without a terminal TurnEnd and without a fired cancel token — adapter contract violation; backlog advances"
        );
    }

    match shutdown_reply {
        Some(reply) => TurnAfter::Shutdown(reply),
        None if channel_closed => TurnAfter::ChannelClosed,
        None => TurnAfter::Continue,
    }
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
    let item = backlog
        .remove(pos)
        .expect("position from iter is in bounds");
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
                agent_id,
                reason,
            );
            SendOutcome::Accepted(message_id)
        }
        OnBusy::FailFast => SendOutcome::Busy,
    }
}

/// Emit a `MessageFailed` for a send that could not start a turn.
fn emit_message_failed(
    emitter: &dyn EventEmitter,
    channel: &str,
    message_id: MessageId,
    agent_id: AgentId,
    error: &str,
) {
    emit_event(
        emitter,
        channel,
        &NormalizedEvent::MessageFailed {
            message_id,
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
    emitter: &dyn EventEmitter,
    channel: &str,
    agent_id: AgentId,
) {
    backlog.retain(|m| {
        if m.send_id == send_id {
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
    emitter: &dyn EventEmitter,
    channel: &str,
    agent_id: AgentId,
) {
    for item in backlog.drain(..) {
        emit_message_cancelled(emitter, channel, item.message_id, agent_id);
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
