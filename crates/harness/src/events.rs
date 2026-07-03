use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use switchboard_core::{AgentId, SendId, SessionLocator};
use uuid::Uuid;

/// UUID v7 turn identifier — consistent with `AgentId` and `ProjectId`.
pub type TurnId = Uuid;

/// UUID v7 identifier minted by the dispatcher for every accepted send (idle or
/// queued), returned synchronously to the caller. A turn later started for that
/// message carries the same `message_id` on its `TurnStart`, letting the
/// frontend correlate "the message I sent/queued" with "the turn now running."
/// A message that fails before any turn starts is reported via
/// [`NormalizedEvent::MessageFailed`] keyed by this id.
pub type MessageId = Uuid;

/// Tells the reducer / UI which content rendering applies to a chunk.
///
/// `Thinking` carries model reasoning text, rendered distinct from (and
/// subordinate to) the answer (the frontend's `ThinkingWidget`). Whether a
/// harness exposes reasoning text varies: Antigravity emits it (planner
/// records, live + on reopen); Gemini writes it only to disk, so we deliberately
/// drop it (reopened-only reasoning is stale UX); Claude's redaction is
/// **per-model** — Sonnet 4.6 returns non-empty reasoning (live `thinking_delta`
/// and on disk, reconstructed on hydrate), while Opus 4.8 redacts the text to
/// empty so it surfaces only as a non-rendering [`AdapterEvent::Liveness`]
/// signal; Codex encrypts it (unavailable). Per-harness reality lives in
/// `docs/harness-behavior.md` §3.2.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContentKind {
    /// User-facing assistant text.
    Text,
    /// Model reasoning blocks — live reasoning text where the harness provides
    /// it (e.g. Antigravity's planner thinking).
    Thinking,
}

/// Discriminates tool origin so the UI can label calls (built-in tool vs MCP
/// vs plugin) without scraping the name. `Plugin` and `Other` are reserved
/// but not currently emitted — a forward-compat pattern so future tool
/// origins land without a wire-format break.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolKind {
    Builtin,
    Mcp,
    Plugin,
    Other,
}

/// One MCP server entry from a `SessionMeta` event. `status` is intentionally
/// an opaque string ("connected" / "disconnected" / "failed" / "needs-auth" /
/// future values) so a new harness-side status doesn't break deserialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerStatus {
    pub name: String,
    pub status: String,
}

/// Where a `RateLimitEvent`'s payload is durable — the dispatcher's gate for
/// whether to persist it to the per-agent metadata sidecar.
///
/// This is an **internal adapter→dispatcher** discriminator: it rides on
/// [`AdapterEvent::RateLimitEvent`] but is deliberately dropped at the
/// [`NormalizedEvent`] boundary (the frontend doesn't need it). Keeping the
/// persistence rule in the type system — rather than a `match harness {…}` in
/// the dispatcher — is what lets the dispatcher stay harness-agnostic while
/// still persisting only the class-C (stream-only) payloads.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RateLimitSource {
    /// Lives only on the live stream; no session-file equivalent (class C, per
    /// `docs/harness-behavior.md` §3.1). Claude's `rate_limit_event`.
    /// The dispatcher persists these to the metadata sidecar so they survive
    /// restart.
    StreamOnly,
    /// Already persisted by the harness in its own session file (class B); the
    /// harness file is canonical and durable, so Switchboard does **not**
    /// re-persist it. Codex's session-file-enriched rate-limit.
    SessionFileBacked,
}

/// Where a `TurnEnd`'s `context_window` is durable — the dispatcher's gate for
/// whether to persist it to the per-agent metadata sidecar. The exact analogue
/// of [`RateLimitSource`] for the window: an **internal adapter→dispatcher**
/// discriminator carried on [`AdapterEvent::TurnEnd`] and dropped at the
/// [`NormalizedEvent`] boundary, so the dispatcher persists only the class-C
/// (stream-only) window without a `match harness {…}`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContextWindowSource {
    /// Stream-only (class C): Claude's `result.modelUsage.<model>.contextWindow`,
    /// absent from the session file. The dispatcher persists it to the metadata
    /// sidecar so the context bar survives restart.
    StreamOnly,
    /// Already durable in the harness's own session file (class B): Codex's
    /// post-terminal session-file enrichment fills the window. Not re-persisted.
    SessionFileBacked,
}

/// Per-turn usage and cost. Carried on `TurnEnd.usage`.
///
/// `total_cost_usd` is Claude Code only (subscription auth has no dollar
/// number for Codex). `context_window` for Claude comes from
/// `result.modelUsage.<model>.contextWindow`; for Codex it's populated by
/// post-terminal session-file enrichment. The remaining `*_tokens` fields
/// mirror exactly what the harness reported — do not normalize them; the
/// reconciled occupancy value lives in `context_input_tokens` instead.
///
/// `context_input_tokens` is **derived, not raw**: the total input-side
/// tokens occupying the context window after this turn, reconciled by the
/// emitting adapter because harnesses count cached tokens *differently*.
/// For Claude, `cached_input_tokens` and `cache_creation_input_tokens` are
/// disjoint additions to `input_tokens`, so the input side is their sum.
/// For Codex, `cached_input_tokens` is a subset already inside
/// `input_tokens`, so the input side is just `input_tokens` — adding the
/// cached count would double-count. Keeping this reconciliation here (not in
/// the frontend) lets the context-utilization formula stay harness-agnostic:
/// `(context_input_tokens + output_tokens) / context_window`. `None` where a
/// harness doesn't compute it (e.g. Gemini, which exposes no window anyway).
///
/// Populated when the harness reports usage on its terminal event. Claude
/// carries it on both `Completed` and `Failed` turns; Codex carries it on
/// `Completed` only (Codex's `turn.failed` doesn't include `usage` in
/// observed fixtures). `usage: None` means "telemetry was unparseable /
/// absent," distinct from a real `Some` carrying zero values (which
/// Claude's synthetic auth-failure responses legitimately emit).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub context_input_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub context_window: Option<u32>,
    pub total_cost_usd: Option<f64>,
}

/// Per-turn real-spend attribution — the gate for showing a turn's cost and an
/// overage marker inline on the message. Distinct from [`TurnUsage`]: usage is
/// raw telemetry, this answers "did this turn cost real money, and was it
/// overage." Stamped per turn by the **adapter** (the harness owns the rule),
/// so the frontend renders on `real_spend` without a `match harness`.
///
/// The frontend reads the two fields through **two distinct gates**: the
/// **cost** renders on `real_spend`, the **"using credits" marker** on
/// `is_overage`. They coincide for Claude, but the split is load-bearing for a
/// future pay-per-use harness (cost shows, marker stays hidden), which is why
/// they're separate fields rather than one.
///
/// - `real_spend` — the harness-agnostic gate for showing cost: set when the
///   turn cost real money. For subscription Claude, `total_cost_usd` is a
///   *notional* API-equivalent figure that isn't charged unless the agent is in
///   overage, so `real_spend == is_overage`. A future pay-per-use harness would
///   set `real_spend` true whenever cost is present, regardless of overage.
/// - `is_overage` — the Claude-derived overage flag (`isUsingOverage`); gates
///   the "using credits" marker specifically.
/// - `overage_resets_at` — the credit/overage window reset, for the marker's
///   tooltip, when the harness reports it (`overageResetsAt`).
///
/// `None` on a `TurnEnd` means "no real-spend info" (non-Claude harness, or a
/// turn with no rate-limit signal) → the message shows neither cost nor marker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnSpend {
    pub real_spend: bool,
    pub is_overage: bool,
    pub overage_resets_at: Option<DateTime<Utc>>,
}

/// Events emitted by harness adapters. `TurnStart` is deliberately absent — it is
/// dispatcher-owned and synthesized before the stream is established. Excluding
/// it here makes the invariant type-enforced: no adapter author can accidentally emit it.
///
/// Variant scope: `ContentChunk`, `ToolStarted`, `ToolCompleted`, `TurnEnd` are
/// turn-scoped (the `turn_id` self-discriminates the agent via the transcript
/// map). `SessionMeta`, `RateLimitEvent` are agent-scoped and carry `agent_id`
/// in the payload because they reach the frontend, which keys them by agent.
///
/// `SessionLocatorCaptured` is **internal** (adapter → dispatcher only): the
/// dispatcher persists the locator to the registry and does not forward it to
/// the frontend, so it has no `NormalizedEvent` counterpart (see
/// [`AdapterEvent::into_normalized`]) and carries no `agent_id` — its sole
/// consumer binds it to the running turn's agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdapterEvent {
    ContentChunk {
        turn_id: TurnId,
        kind: ContentKind,
        text: String,
    },
    /// A content-free sign that the harness is still alive mid-turn. Emitted
    /// when the stream carries activity that produces no renderable content —
    /// e.g. Claude's `signature_delta`, or a `thinking_delta` whose text is
    /// empty (Opus 4.8 redacts reasoning to `""`; a non-empty delta becomes a
    /// `Thinking` `ContentChunk` instead). It re-arms the frontend liveness
    /// timer so a long silent-but-thinking turn is not falsely declared dead.
    /// Never becomes a transcript item.
    Liveness { turn_id: TurnId },
    /// The turn's dedup identity, announced **early** — at the first non-subagent
    /// assistant message, not at turn end. Carries the first assistant
    /// `message.id`, the same value `TurnEnd.first_message_id` carries, so the
    /// frontend can stamp a live turn's `hydration_key` *while it is still
    /// streaming*. That lets a concurrent disk re-read (switch-back refresh)
    /// recognize the live turn as the same turn and collapse against it, instead
    /// of rendering a second copy. Emitted once per turn (Claude only); keyless
    /// harnesses never emit it. Content-free.
    TurnIdentity { turn_id: TurnId, message_id: String },
    ToolStarted {
        turn_id: TurnId,
        tool_use_id: String,
        kind: ToolKind,
        name: String,
        input: serde_json::Value,
    },
    ToolCompleted {
        turn_id: TurnId,
        tool_use_id: String,
        output: String,
        is_error: bool,
    },
    TurnEnd {
        turn_id: TurnId,
        outcome: TurnOutcome,
        ended_at: DateTime<Utc>,
        usage: Option<TurnUsage>,
        /// Where `usage.context_window` is durable — gates whether the
        /// dispatcher persists it to the metadata sidecar. `None` when the turn
        /// carries no window. Internal: dropped at the `NormalizedEvent`
        /// boundary (the frontend reads the window off `usage`, not the source).
        context_window_source: Option<ContextWindowSource>,
        /// Per-turn real-spend attribution (cost/overage gate). Stamped by the
        /// adapter; carried through to the frontend (unlike
        /// `context_window_source`, this *is* rendered). `None` = no real-spend
        /// info for this turn.
        spend: Option<TurnSpend>,
        /// The model this turn ran on — the live per-turn carrier, mirroring
        /// `spend` (stamped by the adapter, carried through to the frontend).
        /// Per harness: Claude/Gemini the most-recent `SessionMeta`/`init`; Codex
        /// the current-turn `turn_context.model` (enrichment re-read); Antigravity
        /// the carry-forward state. `None` when the harness exposes no model.
        model: Option<String>,
        /// The reasoning effort this turn ran at — **Codex only**
        /// (`turn_context.effort`), `None` elsewhere. Live per-turn carrier; the
        /// reopen counterpart is reconstructed from the session file.
        effort: Option<String>,
        /// Cost-join key: the **final** non-subagent assistant message's
        /// Anthropic `message.id`, which appears identically in the live stream
        /// and the on-disk session file (verified). The dispatcher keys the
        /// durable per-turn metadata (`turnmeta` sidecar) on it so cost/overage
        /// can re-attach to the right message on reopen. Internal: dropped at
        /// the `NormalizedEvent` boundary (the frontend never sees it). `None`
        /// when the harness has no such id (non-Claude) or no assistant message.
        stable_message_id: Option<String>,
        /// Live↔disk dedup identity: the **first** non-subagent assistant
        /// message's Anthropic `message.id`. Distinct from `stable_message_id`
        /// on purpose — the first id is *stable from the turn's first assistant message*
        /// (a turn spans multiple assistant messages; the last one is a moving
        /// target until the turn ends, so anchoring dedup on it makes a
        /// mid-flight re-read mint a different key than the completed turn). The
        /// first id is identical between a partial and a complete parse, so it
        /// is what the hydrate merge dedups on. Routed to
        /// `NormalizedEvent::TurnEnd.hydration_key`. `None` for non-Claude
        /// (their live↔disk parity is unprobed; they stay `turn_id`-keyed) or
        /// when the turn produced no assistant message.
        first_message_id: Option<String>,
    },
    RateLimitEvent {
        agent_id: AgentId,
        info: serde_json::Value,
        /// Where this payload is durable — gates Switchboard-side persistence.
        /// Not carried to the frontend (dropped in the `NormalizedEvent`
        /// conversion below).
        source: RateLimitSource,
    },
    SessionMeta {
        agent_id: AgentId,
        model: String,
        harness_version: String,
        tools: Vec<String>,
        mcp_servers: Vec<McpServerStatus>,
        skills: Vec<String>,
        raw: serde_json::Value,
    },
    /// A runtime-assigned session locator the adapter just learned (Codex's
    /// `thread_id`+date on first dispatch; Antigravity's conversation UUID on
    /// first dispatch or a fork-and-heal). The dispatcher persists it to the
    /// **running turn's** agent registry record via its injected
    /// `SessionLocatorSink`; it is **not** forwarded to the frontend. Emitted
    /// only when the locator is newly learned or changes — never on a plain
    /// resume where it's unchanged.
    ///
    /// Deliberately carries **no `agent_id`**: unlike the other agent-scoped
    /// events, this one never reaches the frontend and its sole consumer
    /// (`drain_turn`) always knows the running turn's agent. Binding the locator
    /// to the turn's agent rather than an event-supplied id makes "persist to
    /// the wrong agent" unrepresentable.
    SessionLocatorCaptured { locator: SessionLocator },
}

/// Wire format across the IPC boundary to the frontend.
///
/// **Variant sources.** `TurnStart` and `AgentIdle` are dispatcher-owned —
/// neither exists on [`AdapterEvent`]. The dispatcher synthesizes them at
/// well-defined lifecycle points: `TurnStart` before the adapter stream is
/// established, `AgentIdle` after the stream drains and immediately before
/// the dispatcher's `AgentIdleGuard` drops. Adapter events lift into the
/// remaining variants via `From<AdapterEvent>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum NormalizedEvent {
    TurnStart {
        turn_id: TurnId,
        /// Correlates this turn to the accepted send (idle or dequeued) that
        /// produced it — see [`MessageId`]. The frontend flips its optimistic
        /// message bubble (keyed by `message_id`) from queued/sending to running.
        message_id: MessageId,
        /// The `send_id` of the originating send — shared across a fan-out's
        /// recipients. Lets the live UI group concurrent turns of one send into a
        /// side-by-side row even when the frontend didn't originate the send (a
        /// workflow dispatches backend-side, so it has no local `pending_sends`
        /// entry to derive the grouping from).
        send_id: SendId,
        started_at: DateTime<Utc>,
    },
    /// A **user-side** message dispatched to this agent, surfaced on the agent
    /// channel so the live transcript renders it like any other send's user
    /// message. Emitted by a workflow `send` step (which dispatches backend-side,
    /// so the frontend has no optimistic user turn for it); a fan-out's recipients
    /// share one `send_id`, letting the live UI group them into one user row +
    /// per-recipient columns, identical to a manual send and to the journal-backed
    /// view after reload. Manual sends do **not** use this (the frontend creates
    /// their user turn optimistically).
    UserMessage {
        send_id: SendId,
        text: String,
        at: DateTime<Utc>,
    },
    ContentChunk {
        turn_id: TurnId,
        kind: ContentKind,
        text: String,
    },
    /// Content-free liveness signal (see [`AdapterEvent::Liveness`]). The
    /// frontend re-arms its per-turn heartbeat on this and renders nothing.
    Liveness { turn_id: TurnId },
    /// Early dedup identity for the live turn (see [`AdapterEvent::TurnIdentity`]).
    /// The frontend stamps `hydration_key` onto the in-flight turn so a
    /// concurrent disk re-read collapses against it instead of duplicating. The
    /// internal first `message.id` is surfaced here as `hydration_key` — the same
    /// frontend-facing key `TurnEnd` carries.
    TurnIdentity {
        turn_id: TurnId,
        hydration_key: String,
    },
    ToolStarted {
        turn_id: TurnId,
        tool_use_id: String,
        kind: ToolKind,
        name: String,
        input: serde_json::Value,
    },
    ToolCompleted {
        turn_id: TurnId,
        tool_use_id: String,
        output: String,
        is_error: bool,
    },
    TurnEnd {
        turn_id: TurnId,
        outcome: TurnOutcome,
        ended_at: DateTime<Utc>,
        usage: Option<TurnUsage>,
        /// Per-turn real-spend attribution — gates the inline cost + overage
        /// marker on the message. `None` = show neither. See [`TurnSpend`].
        spend: Option<TurnSpend>,
        /// The model this turn ran on, rendered per-turn in the transcript
        /// footer. `None` = render nothing. See [`AdapterEvent::TurnEnd`].
        model: Option<String>,
        /// The reasoning effort this turn ran at (Codex only), rendered per-turn
        /// in the footer. `None` = render nothing.
        effort: Option<String>,
        /// **Live-matched** hydration key — the same per-turn id this turn will
        /// carry on disk, so a turn that streamed live *and* is later re-read
        /// from the session file is recognized as one turn (the merge dedups on
        /// it). This is the **first** non-subagent assistant `message.id`:
        /// stable from the turn's first assistant message and identical between a partial
        /// and a complete parse (the final id moves until the turn ends, so it
        /// can't anchor a mid-flight re-read). Populated only for harnesses
        /// whose live stream carries a disk-matching id (Claude). `None`
        /// elsewhere (Codex/Gemini unprobed; Antigravity has none) — those turns
        /// dedup by `turn_id` until a probe confirms parity. Frontend-facing;
        /// the internal cost-join `stable_message_id` stays dropped at this
        /// boundary.
        hydration_key: Option<String>,
    },
    RateLimitEvent {
        agent_id: AgentId,
        info: serde_json::Value,
    },
    SessionMeta {
        agent_id: AgentId,
        model: String,
        harness_version: String,
        tools: Vec<String>,
        mcp_servers: Vec<McpServerStatus>,
        skills: Vec<String>,
        raw: serde_json::Value,
    },
    /// A send **failed before any turn started**: either the journal write of
    /// the user's send failed (no durable record, no outcome marker), or the
    /// adapter failed to launch before `TurnStart` (the send was journaled and
    /// a `Failed` outcome marker references the minted turn, but no turn went
    /// live on the wire). Keyed by `message_id` (there is no live `turn_id`);
    /// carries no prompt — the frontend still holds the optimistically-rendered
    /// text and just marks that bubble failed. The async analogue of the
    /// pre-actor synchronous fail-closed `Err`.
    MessageFailed {
        message_id: MessageId,
        /// The **durably recorded** send this failure belongs to, or `None` if the
        /// send never reached the journal (it failed before `record_send`). When
        /// present it lets a backend-originated send (a workflow `send`, which has
        /// no frontend `pending_sends` entry) attach the failed marker under its
        /// live user row; when `None` the frontend renders no row for a
        /// backend-originated send, so live matches the (empty) reload — a send
        /// with no durable record can't be reconstructed. Manual sends ignore this
        /// and resolve via `pending_sends`. (Set `Some` only on the post-`record_send`
        /// adapter-launch-failure path; the invariant is exact up to the
        /// best-effort `record_outcome` write that path performs.)
        send_id: Option<SendId>,
        agent_id: AgentId,
        error: String,
        at: DateTime<Utc>,
    },
    /// A **queued** send was cancelled before it started (its backlog item was
    /// dropped by a `cancel_send` / `cancel_agent` while it was still waiting,
    /// so it never produced a `TurnStart`). Keyed by `message_id` — there is no
    /// `turn_id`. The dispatcher's authoritative signal that a not-yet-started
    /// send is gone, so the frontend renders its cancellation from this event
    /// rather than guessing (a *running* turn's cancellation still arrives as a
    /// `TurnEnd { Cancelled }`). Like `MessageFailed`, this is a non-turn,
    /// message-keyed event and carries no durable journal record (a
    /// queued-but-unstarted send is live-UI-only).
    MessageCancelled {
        message_id: MessageId,
        agent_id: AgentId,
        at: DateTime<Utc>,
    },
    /// Emitted by the dispatcher as the **last event on the per-agent
    /// channel** for a dispatch — immediately before `AgentIdleGuard`
    /// drops.
    ///
    /// Frontend consumers may rely on two contracts:
    ///
    /// 1. **Channel-ordering**: no further events arrive on this channel
    ///    for this dispatch.
    /// 2. **Sendability**: by the time the frontend processes `AgentIdle`,
    ///    the dispatcher accepts a new send to this agent without
    ///    returning `Busy`.
    ///
    /// Distinct from `TurnEnd`: `TurnEnd` is terminal for a turn,
    /// `AgentIdle` is terminal for the per-agent channel for this dispatch.
    /// For Codex agents, post-`TurnEnd` enrichment events
    /// (`RateLimitEvent`, `SessionMeta`) flow between `TurnEnd` and
    /// `AgentIdle`.
    AgentIdle { agent_id: AgentId },
}

impl AdapterEvent {
    /// Whether this event belongs to the **turn** lifecycle (content, tools,
    /// liveness, identity, the terminal) as opposed to the **agent** scope
    /// (rate limits, session metadata, the internal locator capture).
    ///
    /// The dispatcher uses this to enforce the adapter contract at its
    /// boundary: after a turn's terminal, further turn-scoped events are a
    /// contract violation and are dropped, while agent-scoped enrichment
    /// legitimately flows post-terminal (Codex and Antigravity both emit
    /// `RateLimitEvent`/`SessionMeta` after `TurnEnd` by design).
    ///
    /// Exhaustive match on purpose — no `_` arm — so adding a variant forces
    /// an explicit lifecycle classification at compile time instead of
    /// silently inheriting a default.
    #[must_use]
    pub fn is_turn_scoped(&self) -> bool {
        match self {
            AdapterEvent::ContentChunk { .. }
            | AdapterEvent::Liveness { .. }
            | AdapterEvent::TurnIdentity { .. }
            | AdapterEvent::ToolStarted { .. }
            | AdapterEvent::ToolCompleted { .. }
            | AdapterEvent::TurnEnd { .. } => true,
            AdapterEvent::RateLimitEvent { .. }
            | AdapterEvent::SessionMeta { .. }
            | AdapterEvent::SessionLocatorCaptured { .. } => false,
        }
    }

    /// Lift an adapter event into its frontend-facing [`NormalizedEvent`], or
    /// `None` for an **internal** event that the dispatcher consumes without
    /// forwarding. Not every adapter event has a wire representation:
    /// `SessionLocatorCaptured` is persisted to the registry and dropped, so it
    /// returns `None`. (Replaces a total `From` impl, which couldn't honestly
    /// represent the no-wire-form case without a panicking arm.)
    #[must_use]
    pub fn into_normalized(self) -> Option<NormalizedEvent> {
        Some(match self {
            AdapterEvent::ContentChunk {
                turn_id,
                kind,
                text,
            } => NormalizedEvent::ContentChunk {
                turn_id,
                kind,
                text,
            },
            AdapterEvent::Liveness { turn_id } => NormalizedEvent::Liveness { turn_id },
            AdapterEvent::TurnIdentity {
                turn_id,
                message_id,
            } => NormalizedEvent::TurnIdentity {
                turn_id,
                hydration_key: message_id,
            },
            AdapterEvent::ToolStarted {
                turn_id,
                tool_use_id,
                kind,
                name,
                input,
            } => NormalizedEvent::ToolStarted {
                turn_id,
                tool_use_id,
                kind,
                name,
                input,
            },
            AdapterEvent::ToolCompleted {
                turn_id,
                tool_use_id,
                output,
                is_error,
            } => NormalizedEvent::ToolCompleted {
                turn_id,
                tool_use_id,
                output,
                is_error,
            },
            // `context_window_source` and `stable_message_id` are intentionally
            // dropped (the `..`) — internal plumbing the frontend doesn't need
            // (`stable_message_id`, the cost-join key, is consumed by the
            // dispatcher off the `AdapterEvent` before this conversion). `spend`
            // *is* carried through — it's rendered on the message. The
            // frontend-facing `hydration_key` is populated from the **first**
            // assistant `message.id` (`first_message_id`), the parse-invariant
            // dedup identity — deliberately *not* `stable_message_id` (the final
            // id), which moves until the turn ends and so can't anchor a
            // mid-flight re-read.
            AdapterEvent::TurnEnd {
                turn_id,
                outcome,
                ended_at,
                usage,
                spend,
                model,
                effort,
                first_message_id,
                ..
            } => NormalizedEvent::TurnEnd {
                turn_id,
                outcome,
                ended_at,
                usage,
                spend,
                model,
                effort,
                // The dedup key is the *first* assistant message id (parse-
                // invariant), not `stable_message_id` (the final id, consumed by
                // the dispatcher for the cost-join before this conversion).
                hydration_key: first_message_id,
            },
            // `source` is intentionally dropped — it's an internal persistence
            // discriminator the frontend doesn't need (see `RateLimitSource`).
            AdapterEvent::RateLimitEvent { agent_id, info, .. } => {
                NormalizedEvent::RateLimitEvent { agent_id, info }
            }
            AdapterEvent::SessionMeta {
                agent_id,
                model,
                harness_version,
                tools,
                mcp_servers,
                skills,
                raw,
            } => NormalizedEvent::SessionMeta {
                agent_id,
                model,
                harness_version,
                tools,
                mcp_servers,
                skills,
                raw,
            },
            // Internal adapter → dispatcher event; persisted to the registry,
            // never shown to the frontend.
            AdapterEvent::SessionLocatorCaptured { .. } => return None,
        })
    }
}

/// Outcome of a completed turn. The `kind` field on `Failed` is load-bearing
/// for retry policy: `HarnessError` (model/API issue) vs `AdapterFailure`
/// (subprocess crash, parse error, infrastructure) have different retry
/// semantics.
///
/// `Cancelled` is distinct from `Failed` because cancellation is
/// **intent-bearing, not an error** — the user (or a workflow, or shutdown)
/// deliberately stopped the turn, and the frontend renders it differently
/// from a harness failure. The cancelled terminal is **dispatcher-stamped,
/// not adapter-emitted**: a binary cancellation token can't carry intent, and
/// the dispatcher is the only layer that knows *why* it fired the token, so it
/// synthesizes this variant with the `source` it recorded. `source` also lets
/// workflow cancel and shutdown reuse the same mechanism.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnOutcome {
    Completed,
    Failed { kind: FailureKind, message: String },
    Cancelled { source: CancelSource },
}

/// Who initiated a cancellation. Carried on `TurnOutcome::Cancelled` so the
/// UI (and persisted journal) can distinguish a user pressing stop from a
/// workflow aborting a step or the app shutting down.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CancelSource {
    /// The user cancelled an in-flight turn (compose-bar / context-menu stop).
    User,
    /// A workflow aborted the turn.
    Workflow,
    /// App shutdown or working-directory removal drained the turn.
    Shutdown,
}

/// Discriminates the cause of a failed turn for retry-policy decisions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FailureKind {
    /// Harness reported `is_error: true` in its terminal `result` event.
    /// Caused by model/API issues (bad model name, rate limit, invalid prompt).
    HarnessError,
    /// Adapter synthesized this: subprocess died, parser hit malformed JSON, or
    /// stdout EOF arrived without a terminal `result` event. Typically transient.
    AdapterFailure,
    /// Subscription / tier auth is missing or expired. Detected per-adapter
    /// from stream signals (Claude: `assistant.error == "authentication_failed"`;
    /// Codex: `turn.failed.error.message` containing `"401 Unauthorized"`;
    /// Gemini: 401 in `result.status:"error"`, exit 41 + "Please set an Auth
    /// method", or exit 42 + 401; Antigravity: `Authentication required` /
    /// `authentication timed out` on stdout). Each adapter authors a uniform
    /// actionable message naming the harness + recovery command — the user
    /// sees one consistent failure line in the transcript regardless of which
    /// harness's auth surface fired. There is no proactive auth banner;
    /// reactive auth means "discovered on send, fixed by signing in, then
    /// sending again."
    AuthFailure,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fresh_turn_id() -> TurnId {
        Uuid::now_v7()
    }

    fn fresh_agent_id() -> AgentId {
        Uuid::now_v7()
    }

    fn fresh_time() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn turn_start_wire_shape() {
        let turn_id = fresh_turn_id();
        let message_id = Uuid::now_v7();
        let send_id = Uuid::now_v7();
        let started_at = fresh_time();
        let event = NormalizedEvent::TurnStart {
            turn_id,
            message_id,
            send_id,
            started_at,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "turn_start");
        assert_eq!(value["turn_id"], turn_id.to_string());
        assert_eq!(value["message_id"], message_id.to_string());
        assert_eq!(value["send_id"], send_id.to_string());
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn message_failed_wire_shape() {
        let message_id = Uuid::now_v7();
        let send_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let at = fresh_time();
        let event = NormalizedEvent::MessageFailed {
            message_id,
            send_id: Some(send_id),
            agent_id,
            error: "journal write failed".to_owned(),
            at,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "message_failed");
        assert_eq!(value["message_id"], message_id.to_string());
        assert_eq!(value["send_id"], send_id.to_string());
        // A pre-durable failure carries no send_id (serializes as null).
        let no_send = NormalizedEvent::MessageFailed {
            message_id,
            send_id: None,
            agent_id,
            error: "rejected".to_owned(),
            at,
        };
        assert!(serde_json::to_value(&no_send).unwrap()["send_id"].is_null());
        assert_eq!(value["agent_id"], agent_id.to_string());
        assert_eq!(value["error"], "journal write failed");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn content_chunk_wire_shape_with_kind_text() {
        let turn_id = fresh_turn_id();
        let event = NormalizedEvent::ContentChunk {
            turn_id,
            kind: ContentKind::Text,
            text: "hello".to_owned(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "content_chunk");
        assert_eq!(value["kind"], "text");
        assert_eq!(value["text"], "hello");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn content_chunk_wire_shape_preserves_thinking_kind() {
        let event = NormalizedEvent::ContentChunk {
            turn_id: fresh_turn_id(),
            kind: ContentKind::Thinking,
            text: "deliberating".to_owned(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["kind"], "thinking");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn turn_end_completed_wire_shape_with_no_usage() {
        let turn_id = fresh_turn_id();
        let event = NormalizedEvent::TurnEnd {
            turn_id,
            outcome: TurnOutcome::Completed,
            ended_at: fresh_time(),
            usage: None,
            spend: None,
            model: None,
            effort: None,
            hydration_key: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "turn_end");
        assert_eq!(value["outcome"]["status"], "completed");
        assert!(value["usage"].is_null());
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn turn_end_wire_shape_with_full_usage() {
        let event = NormalizedEvent::TurnEnd {
            turn_id: fresh_turn_id(),
            outcome: TurnOutcome::Completed,
            ended_at: fresh_time(),
            usage: Some(TurnUsage {
                input_tokens: 100,
                output_tokens: 25,
                cached_input_tokens: Some(50),
                cache_creation_input_tokens: Some(30),
                context_input_tokens: Some(180),
                reasoning_output_tokens: Some(5),
                context_window: Some(200_000),
                total_cost_usd: Some(0.0125),
            }),
            spend: Some(TurnSpend {
                real_spend: true,
                is_overage: true,
                overage_resets_at: None,
            }),
            model: None,
            effort: None,
            hydration_key: Some("msg_live01".to_owned()),
        };
        let value = serde_json::to_value(&event).unwrap();
        // The live-matched key is frontend-facing — it must serialize onto the
        // wire (the per-agent merge dedups on it).
        assert_eq!(value["hydration_key"], "msg_live01");
        assert_eq!(value["usage"]["input_tokens"], 100);
        assert_eq!(value["usage"]["output_tokens"], 25);
        assert_eq!(value["usage"]["cached_input_tokens"], 50);
        assert_eq!(value["usage"]["cache_creation_input_tokens"], 30);
        assert_eq!(value["usage"]["context_input_tokens"], 180);
        assert_eq!(value["usage"]["reasoning_output_tokens"], 5);
        assert_eq!(value["usage"]["context_window"], 200_000);
        assert_eq!(value["usage"]["total_cost_usd"], 0.0125);
        assert_eq!(value["spend"]["real_spend"], true);
        assert_eq!(value["spend"]["is_overage"], true);
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn turn_end_failed_wire_shape() {
        let event = NormalizedEvent::TurnEnd {
            turn_id: fresh_turn_id(),
            outcome: TurnOutcome::Failed {
                kind: FailureKind::HarnessError,
                message: "bad model".to_owned(),
            },
            ended_at: fresh_time(),
            usage: None,
            spend: None,
            model: None,
            effort: None,
            hydration_key: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["outcome"]["status"], "failed");
        assert_eq!(value["outcome"]["kind"], "harness_error");
        assert_eq!(value["outcome"]["message"], "bad model");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn turn_end_cancelled_wire_shape() {
        for (source, tag) in [
            (CancelSource::User, "user"),
            (CancelSource::Workflow, "workflow"),
            (CancelSource::Shutdown, "shutdown"),
        ] {
            let event = NormalizedEvent::TurnEnd {
                turn_id: fresh_turn_id(),
                outcome: TurnOutcome::Cancelled { source },
                ended_at: fresh_time(),
                usage: None,
                spend: None,
                model: None,
                effort: None,
                hydration_key: None,
            };
            let value = serde_json::to_value(&event).unwrap();
            assert_eq!(value["outcome"]["status"], "cancelled");
            assert_eq!(value["outcome"]["source"], tag);
            let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
            assert_eq!(parsed, event);
        }
    }

    #[test]
    fn auth_failure_kind_wire_shape() {
        let event = NormalizedEvent::TurnEnd {
            turn_id: fresh_turn_id(),
            outcome: TurnOutcome::Failed {
                kind: FailureKind::AuthFailure,
                message: "Claude authentication required — run `claude auth login`".to_owned(),
            },
            ended_at: fresh_time(),
            usage: None,
            spend: None,
            model: None,
            effort: None,
            hydration_key: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["outcome"]["status"], "failed");
        assert_eq!(value["outcome"]["kind"], "auth_failure");
        assert_eq!(
            value["outcome"]["message"],
            "Claude authentication required — run `claude auth login`"
        );
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn adapter_failure_kind_wire_shape() {
        let event = NormalizedEvent::TurnEnd {
            turn_id: fresh_turn_id(),
            outcome: TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message: "crash".to_owned(),
            },
            ended_at: fresh_time(),
            usage: None,
            spend: None,
            model: None,
            effort: None,
            hydration_key: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["outcome"]["kind"], "adapter_failure");
    }

    #[test]
    fn tool_started_wire_shape() {
        let event = NormalizedEvent::ToolStarted {
            turn_id: fresh_turn_id(),
            tool_use_id: "toolu_abc".to_owned(),
            kind: ToolKind::Builtin,
            name: "Bash".to_owned(),
            input: json!({"command": "ls"}),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "tool_started");
        assert_eq!(value["tool_use_id"], "toolu_abc");
        assert_eq!(value["kind"], "builtin");
        assert_eq!(value["name"], "Bash");
        assert_eq!(value["input"]["command"], "ls");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn tool_started_mcp_kind_wire_shape() {
        let event = NormalizedEvent::ToolStarted {
            turn_id: fresh_turn_id(),
            tool_use_id: "toolu_xyz".to_owned(),
            kind: ToolKind::Mcp,
            name: "mcp__server__action".to_owned(),
            input: json!({}),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["kind"], "mcp");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn tool_completed_wire_shape() {
        let event = NormalizedEvent::ToolCompleted {
            turn_id: fresh_turn_id(),
            tool_use_id: "toolu_abc".to_owned(),
            output: "hello\n".to_owned(),
            is_error: false,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "tool_completed");
        assert_eq!(value["tool_use_id"], "toolu_abc");
        assert_eq!(value["output"], "hello\n");
        assert_eq!(value["is_error"], false);
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn rate_limit_event_wire_shape() {
        let event = NormalizedEvent::RateLimitEvent {
            agent_id: fresh_agent_id(),
            info: json!({"status": "allowed", "resetsAt": 1_778_701_800u64}),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "rate_limit_event");
        assert_eq!(value["info"]["status"], "allowed");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn agent_idle_wire_shape() {
        let agent_id = fresh_agent_id();
        let event = NormalizedEvent::AgentIdle { agent_id };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "agent_idle");
        assert_eq!(value["agent_id"], agent_id.to_string());
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn session_meta_wire_shape() {
        let event = NormalizedEvent::SessionMeta {
            agent_id: fresh_agent_id(),
            model: "claude-sonnet-4-6".to_owned(),
            harness_version: "2.1.140".to_owned(),
            tools: vec!["Bash".to_owned(), "Read".to_owned()],
            mcp_servers: vec![McpServerStatus {
                name: "tiddly".to_owned(),
                status: "connected".to_owned(),
            }],
            skills: vec!["debug".to_owned()],
            raw: json!({"subtype": "init", "cwd": "/tmp"}),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "session_meta");
        assert_eq!(value["model"], "claude-sonnet-4-6");
        assert_eq!(value["harness_version"], "2.1.140");
        assert_eq!(value["tools"], json!(["Bash", "Read"]));
        assert_eq!(value["mcp_servers"][0]["name"], "tiddly");
        assert_eq!(value["mcp_servers"][0]["status"], "connected");
        assert_eq!(value["skills"], json!(["debug"]));
        assert_eq!(value["raw"]["subtype"], "init");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn adapter_event_lifts_to_normalized_content_chunk() {
        let adapter = AdapterEvent::ContentChunk {
            turn_id: fresh_turn_id(),
            kind: ContentKind::Text,
            text: "hi".to_owned(),
        };
        let normalized = adapter
            .into_normalized()
            .expect("lifts to a normalized event");
        assert!(matches!(
            normalized,
            NormalizedEvent::ContentChunk {
                kind: ContentKind::Text,
                text,
                ..
            } if text == "hi"
        ));
    }

    #[test]
    fn adapter_event_lifts_to_normalized_liveness() {
        let turn_id = fresh_turn_id();
        let normalized = AdapterEvent::Liveness { turn_id }
            .into_normalized()
            .expect("lifts to a normalized event");
        assert!(matches!(
            normalized,
            NormalizedEvent::Liveness { turn_id: t } if t == turn_id
        ));
    }

    #[test]
    fn adapter_event_lifts_to_normalized_turn_end_completed() {
        let adapter = AdapterEvent::TurnEnd {
            turn_id: fresh_turn_id(),
            outcome: TurnOutcome::Completed,
            ended_at: fresh_time(),
            usage: None,
            context_window_source: None,
            stable_message_id: None,
            first_message_id: None,
            spend: None,
            model: None,
            effort: None,
        };
        let normalized = adapter
            .into_normalized()
            .expect("lifts to a normalized event");
        assert!(matches!(
            normalized,
            NormalizedEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                usage: None,
                ..
            }
        ));
    }

    #[test]
    fn adapter_event_turn_end_carries_model_effort_and_first_id_as_hydration_key() {
        // Distinct first/last ids prove `hydration_key` is routed from the
        // *first* id (the dedup identity), not `stable_message_id` (the final id,
        // which stays internal/dropped at this boundary).
        let adapter = AdapterEvent::TurnEnd {
            turn_id: fresh_turn_id(),
            outcome: TurnOutcome::Completed,
            ended_at: fresh_time(),
            usage: None,
            context_window_source: None,
            stable_message_id: Some("msg_final".to_owned()),
            first_message_id: Some("msg_first".to_owned()),
            spend: None,
            model: Some("gpt-5.5".to_owned()),
            effort: Some("high".to_owned()),
        };
        let normalized = adapter.into_normalized().expect("lifts");
        match normalized {
            NormalizedEvent::TurnEnd {
                model,
                effort,
                hydration_key,
                ..
            } => {
                assert_eq!(model.as_deref(), Some("gpt-5.5"));
                assert_eq!(effort.as_deref(), Some("high"));
                assert_eq!(hydration_key.as_deref(), Some("msg_first"));
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn adapter_event_lifts_to_normalized_turn_end_failed() {
        let adapter = AdapterEvent::TurnEnd {
            turn_id: fresh_turn_id(),
            outcome: TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message: "oops".to_owned(),
            },
            ended_at: fresh_time(),
            usage: None,
            context_window_source: None,
            stable_message_id: None,
            first_message_id: None,
            spend: None,
            model: None,
            effort: None,
        };
        let normalized = adapter
            .into_normalized()
            .expect("lifts to a normalized event");
        assert!(matches!(
            normalized,
            NormalizedEvent::TurnEnd {
                outcome: TurnOutcome::Failed {
                    kind: FailureKind::AdapterFailure,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn adapter_event_lifts_to_normalized_tool_started() {
        let adapter = AdapterEvent::ToolStarted {
            turn_id: fresh_turn_id(),
            tool_use_id: "t".to_owned(),
            kind: ToolKind::Builtin,
            name: "Bash".to_owned(),
            input: json!({}),
        };
        let normalized = adapter
            .into_normalized()
            .expect("lifts to a normalized event");
        assert!(matches!(
            normalized,
            NormalizedEvent::ToolStarted { name, .. } if name == "Bash"
        ));
    }

    #[test]
    fn adapter_event_lifts_to_normalized_tool_completed() {
        let adapter = AdapterEvent::ToolCompleted {
            turn_id: fresh_turn_id(),
            tool_use_id: "t".to_owned(),
            output: "ok".to_owned(),
            is_error: false,
        };
        let normalized = adapter
            .into_normalized()
            .expect("lifts to a normalized event");
        assert!(matches!(
            normalized,
            NormalizedEvent::ToolCompleted { output, .. } if output == "ok"
        ));
    }

    #[test]
    fn adapter_event_lifts_to_normalized_rate_limit_event_dropping_source() {
        let agent_id = fresh_agent_id();
        let adapter = AdapterEvent::RateLimitEvent {
            agent_id,
            info: json!({"x": 1}),
            source: RateLimitSource::StreamOnly,
        };
        // The `source` discriminator is internal — it must not survive the
        // conversion to the wire-format event (the frontend never sees it).
        let normalized = adapter
            .into_normalized()
            .expect("lifts to a normalized event");
        assert!(matches!(
            normalized,
            NormalizedEvent::RateLimitEvent { agent_id: a, .. } if a == agent_id
        ));
        let value = serde_json::to_value(&normalized).unwrap();
        assert!(
            value.get("source").is_none(),
            "source must not appear on the wire: {value}"
        );
    }

    #[test]
    fn adapter_event_lifts_to_normalized_session_meta() {
        let adapter = AdapterEvent::SessionMeta {
            agent_id: fresh_agent_id(),
            model: "claude-sonnet-4-6".to_owned(),
            harness_version: "2.1.140".to_owned(),
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            raw: json!({}),
        };
        let normalized = adapter
            .into_normalized()
            .expect("lifts to a normalized event");
        assert!(matches!(
            normalized,
            NormalizedEvent::SessionMeta { model, .. } if model == "claude-sonnet-4-6"
        ));
    }

    #[test]
    fn session_locator_captured_does_not_lift_to_a_wire_event() {
        // Internal adapter→dispatcher event: persisted to the registry, never
        // forwarded to the frontend, so it has no NormalizedEvent counterpart.
        let adapter = AdapterEvent::SessionLocatorCaptured {
            locator: SessionLocator::Uuid(Uuid::now_v7()),
        };
        assert!(adapter.into_normalized().is_none());
    }
}
