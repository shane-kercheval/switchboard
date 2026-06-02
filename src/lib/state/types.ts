// Internal frontend state types for the unified-stream model.
//
// Companion to `$lib/types.ts` (wire-format types only). Component state
// shape lives here; events on the IPC boundary live there. Reducers in
// this module's `reducers.ts` consume wire-format events and produce the
// shapes defined below.
//
// **Naming convention.** snake_case throughout, matching the wire-format
// (`turn_id`, `agent_id`, `started_at`, `ended_at`). Aligning state shape
// with wire shape eliminates rename-at-IPC-boundary drift — load-bearing
// for session-file rehydration, where disk-parsed turns flow through the
// same reducer as live-stream turns without any field translation.

import type {
  AgentId,
  ContentKind,
  FailureKind,
  McpServerStatus,
  MessageId,
  ParseWarning,
  SendId,
  ToolKind,
  TurnId,
  TurnUsage,
} from "$lib/types";

/// Role-mixed turn entries — user prompts and agent responses live in the
/// same chronological stream. Harness session files store user and assistant
/// events as separate entries; this shape matches that, so disk rehydration
/// produces the same structure as live-stream dispatch with no translation.
///
/// `agent_id` is present on both roles because the unified transcript view
/// merges turns across all agents and needs the attribution at every entry.
/// A user prompt's `agent_id` is the **recipient** of that prompt — the
/// agent the user sent it to.
export type Turn =
  | {
      role: "user";
      turn_id: TurnId;
      agent_id: AgentId;
      /// The Send this user turn belongs to. A fan-out's recipients share one,
      /// so the unified view groups the user's message once (and renders the N
      /// responses as one side-by-side group). Live turns carry it from the
      /// frontend-minted id; hydrated history recovers it from the journal.
      /// Optional until the fan-out grouping pass consumes it.
      send_id?: SendId;
      started_at: string;
      text: string;
    }
  | {
      role: "agent";
      turn_id: TurnId;
      agent_id: AgentId;
      /// The Send this response belongs to (groups a fan-out's responses
      /// side-by-side). Live: stamped from the dispatching send. Hydrated:
      /// recovered by the backend's journal join — `undefined` when no Send
      /// matched (pre-journal history).
      send_id?: SendId;
      started_at: string;
      ended_at?: string;
      /// `"cancelled"` is a terminal state distinct from `"failed"`: the user
      /// (or a workflow / shutdown) stopped the turn, it is not an error. Full
      /// cancelled-turn presentation (partial output labelled cancelled) is a
      /// later milestone; this status keeps the distinction in state today.
      status: "streaming" | "complete" | "failed" | "cancelled";
      /// Ordered stream of turn content items — text chunks and tool calls
      /// interleaved in arrival order. Real Claude turns produce
      /// text → tool → text patterns; two separate arrays would lose that
      /// ordering. Discriminated by `item_kind` (state-only field; not on
      /// the wire) so the renderer can branch on `item.item_kind === "tool"`
      /// for exhaustive narrowing.
      items: TurnItem[];
      usage?: TurnUsage;
      /// Populated when status = "failed". Preserved so retry UX can distinguish
      /// recoverable from non-recoverable failures (HarnessError → suggest retry;
      /// AdapterFailure → suggest "report bug"; AuthFailure → "run claude auth login").
      error?: string;
      error_kind?: FailureKind;
    };

/// One ordered entry in an agent turn's content stream. Discriminated by
/// `item_kind` — `"text"` for streamed text chunks, `"tool"` for tool
/// invocations. `item_kind` is a state-only discriminator (the wire
/// format already discriminates events by `type`); the per-variant `kind`
/// fields match their wire-format counterparts (`ContentKind` from
/// `content_chunk.kind`, `ToolKind` from `tool_started.kind`).
export type TurnItem = TextChunk | ToolCall;

export type TextChunk = {
  item_kind: "text";
  /// Mirrors wire `content_chunk.kind`. `"thinking"` carries model reasoning,
  /// rendered distinct from the answer (see harness-behavior.md §3.2).
  kind: ContentKind;
  text: string;
};

/// One tool call attached to an agent turn. Lifecycle: `ToolStarted` appends
/// an entry with `output`/`is_error`/`completed_at` undefined; `ToolCompleted`
/// fills those in by matching `tool_use_id`.
///
/// `input` is `unknown` because the harnesses emit arbitrary JSON
/// (`command_execution.command`, `mcp_tool_call.arguments`, Claude's per-tool
/// schemas). The renderer pretty-prints; nothing in this module inspects it.
export type ToolCall = {
  item_kind: "tool";
  tool_use_id: string;
  /// Mirrors wire `tool_started.kind`. `"builtin"` / `"mcp"` are emitted
  /// today; `"plugin"` / `"other"` are reserved.
  kind: ToolKind;
  name: string;
  input: unknown;
  output?: string;
  is_error?: boolean;
  started_at: string;
  completed_at?: string;
};

/// One optimistic send awaiting its `turn_start`. `user_turn_id` keys the
/// optimistic user turn in the transcript (so a client-side IPC failure can
/// prune the right entry wherever it sits in the list); `message_id` is the
/// accepted-send receipt, filled by `recordSendAccepted` once `send_message`
/// resolves (absent during the window before that, hence optional).
export type PendingSend = {
  send_id: SendId;
  user_turn_id: TurnId;
  message_id?: MessageId;
  /// Set when the user cancelled/stopped this send *before* the backend
  /// accepted it (no `message_id` yet). Firing the backend cancel then would
  /// race the in-flight `send_message` IPC and could miss, letting the send run
  /// anyway. Instead the entry is flagged and the cancel is deferred to whenever
  /// the send is confirmed — `recordSendAccepted` (queued → drop) or `turn_start`
  /// (already running → cancel the live turn). Such an entry is no longer "live"
  /// work (excluded from the composer's stop affordance).
  cancel_requested?: boolean;
};

/// Per-agent operational state.
///
/// **Three separate fields for three separate concerns** (deliberately
/// not collapsed into a single `status` enum):
///
/// - `run_status`: pure dispatch lifecycle. **Sole sendability signal**
///   when combined with `hydration_status`. After a failed turn, the agent
///   IS sendable again — `run_status` flips back to `"idle"` on `AgentIdle`
///   regardless of whether the turn succeeded or failed.
/// - `last_error`: runtime record of the most-recent failure. Failures are
///   rendered in the transcript (a failed agent turn), not in the sidebar, so
///   this is not a display surface today; it is kept for devtools/logging and
///   future retry UX. Does NOT gate Send. Cleared on the next successful
///   `turn_end`.
/// - `in_flight_turn_id`: heartbeat scope. The turn the timer is tracking.
///
/// Conflating these (e.g., a status enum with `"errored"`) would force the
/// "send" gate to also encode "last turn health," which is the wrong
/// semantic — a transient failure shouldn't paint the agent as unusable.
export type AgentRuntime = {
  agent_id: AgentId;
  /// Three-state dispatch lifecycle. **Sole sendability signal** when
  /// combined with `hydration_status`:
  ///
  /// - `"idle"` — dispatcher will accept a new send. Compose-bar Send enabled.
  /// - `"starting"` — user clicked Send; the send has been accepted (a
  ///   `message_id` minted) but the correlated `TurnStart` hasn't arrived
  ///   yet. Compose-bar Send disabled. Without this state, a second click in
  ///   the gap between user-submit and backend-emit would slip through the
  ///   frontend gate and surface a confusing `Busy` error from the
  ///   dispatcher. An idle recipient leaves this state almost immediately
  ///   (TurnStart arrives at once); a busy recipient's send is queued
  ///   server-side and stays in `"starting"` until its turn later dispatches.
  /// - `"processing"` — the correlated `TurnStart` has arrived; the backend's
  ///   `AgentIdleGuard` is held. Compose-bar Send disabled.
  ///
  /// **`message_id` correlation.** `send_message` returns a `message_id`
  /// (the accepted-send receipt), recorded in `pending_message_id` while
  /// `"starting"`. The turn the dispatcher later starts for that send carries
  /// the same `message_id` on its `turn_start`; a pre-turn failure surfaces as
  /// a `message_failed` carrying it. The reducer correlates either event back
  /// to this `pending_message_id`.
  ///
  /// State machine:
  ///
  /// ```
  /// idle  --dispatchUserTurn-->  starting        (records pending_message_id)
  /// starting  --(turn_start event, matched message_id)-->  processing
  /// starting  --(message_failed event, matched message_id)-->  idle
  /// starting  --(failSendStart action)-->  idle
  /// processing  --(agent_idle event)-->  idle
  /// ```
  ///
  /// `message_failed` / `failSendStart` are the **only legal paths** from
  /// `starting` back to `idle` without going through `processing` —
  /// `agent_idle` is guarded to only flip `processing → idle` (a stray
  /// `agent_idle` in the starting window must not race the gate open).
  ///
  /// **Stuck-in-starting diagnostic.** If an agent remains in `"starting"`
  /// indefinitely with no queued backlog ahead of it, either: (a) the
  /// dispatcher accepted `send_message` but never emitted `TurnStart`
  /// (dispatcher regression — TurnStart is dispatcher-emitted and
  /// contractually guaranteed per AGENTS.md), or (b) the compose-bar caller
  /// forgot to invoke `failSendStart` on IPC failure. Look at
  /// `crates/dispatcher/src/lib.rs` and the compose-bar's catch block
  /// respectively.
  run_status: "idle" | "starting" | "processing";
  /// Ordered list of sends dispatched to this agent that haven't yet produced a
  /// `turn_start` — the optimistic user turns still waiting on their response.
  /// One entry per send, in dispatch order (the order the backend runs them).
  ///
  /// This single structure replaces the old scalar `pending_message_id`: it
  /// must track *several* pending sends at once, because send-while-busy is
  /// un-gated (a send to a busy agent queues behind the running turn). Each
  /// entry carries enough identity to prune the *right* one on every path that
  /// ends a send without a `turn_start`:
  /// - `turn_start` consumes the entry matching its `message_id` (else the
  ///   front, covering the race where the IPC receipt hasn't landed yet) and
  ///   stamps that response's `send_id`.
  /// - `message_failed` prunes the matching/front entry (a pre-start failure is
  ///   always the next-to-run send).
  /// - a client-side IPC failure (`failSendStart`) prunes by `user_turn_id` —
  ///   a queued send's failed entry can be anywhere in the list, not the front.
  /// - cancel-send prunes every entry of the cancelled `send_id`.
  pending_sends?: PendingSend[];
  /// The turn the heartbeat timer is tracking. Distinct from `run_status`
  /// because for fast-events races the entire stream can fire before the
  /// IPC reply lands; this tracking key lets late events still extend the
  /// timer correctly.
  in_flight_turn_id?: TurnId;
  /// Transient: the ISO-8601 instant the heartbeat timer expired while this turn
  /// was still in flight with no activity (i.e. when the turn went "quiet").
  /// The turn is alive on the backend (it still holds the busy-lock) but silent,
  /// so this drives a soft, counting-up "No response (…)" indicator — never a
  /// failure. `undefined` means not quiet. Set on `heartbeat_timeout` (to that
  /// event's `at`); cleared on the next activity event for `in_flight_turn_id`
  /// (content/tool/liveness) or on turn end. The indicator is scoped to
  /// `in_flight_turn_id` so it never paints an unrelated streaming turn's
  /// footer; the footer derives elapsed silence as `now - quiet_since +
  /// HEARTBEAT_TIMEOUT_MS` (the timer fired one threshold after the last
  /// activity).
  quiet_since?: string;
  /// Runtime record of the last turn's failure (not rendered — failures surface
  /// in the transcript as a failed agent turn). Set on failed `TurnEnd` or a
  /// pre-start `message_failed`; cleared on the next successful turn. Does NOT
  /// gate sendability. (A heartbeat timeout no longer sets this — a silent turn
  /// isn't a failure; see `quiet_since`.)
  last_error?: { message: string; kind: FailureKind };
  /// Populated by live `SessionMeta` events or by disk hydration of the
  /// agent's session file. Undefined on agents whose first dispatch
  /// hasn't happened yet.
  meta?: AgentMeta;
  /// Most-recent `RateLimitEvent.info` payload. Opaque — the renderer reads
  /// `primary.used_percent` (Codex) or `isUsingOverage` (Claude). Populated
  /// by live events or by hydration from the metadata sidecar.
  last_rate_limit?: unknown;
  /// Capture time of `last_rate_limit` when it came from the metadata
  /// sidecar on hydration (a stream-only/class-C value restored across
  /// restart). ISO-8601 string. `null` once a live `rate_limit_event`
  /// overwrites the in-memory value (it's no longer an on-disk snapshot)
  /// and for class-B sources. Drives the UI "as of …" staleness qualifier:
  /// the staleness check is `as_of != null && age(as_of) > threshold`.
  last_rate_limit_as_of?: string | null;
  /// Disk-rehydration lifecycle. Newly-created agents start at
  /// `"complete"` (nothing to hydrate); registered/attached agents pass
  /// through `"pending"` → `"loading"` → `"complete"`. Compose-bar Send
  /// is gated on this being `"complete"` (or `"failed"`, which enters
  /// degraded-dispatch mode with a banner).
  hydration_status: "pending" | "loading" | "complete" | "failed";
  /// Set when this agent's transcript failed to load entirely during
  /// project-scoped hydration (a corrupt sidecar / unreadable session file —
  /// the backend's per-agent `load_error`). Distinct from `last_error` (a
  /// failed *turn*): this is "couldn't load history," not "a turn errored."
  /// The rest of the project still renders; the sidebar surfaces this so the
  /// user knows which agent's history is missing and why.
  hydration_error?: string;
  /// Non-blocking parser warnings surfaced by `load_transcript` — stale
  /// Codex sidecar, malformed JSONL lines, etc. Empty / undefined when no
  /// warnings landed. Display-only; never gates sendability or hydration
  /// success.
  parse_warnings?: ParseWarning[];
};

export type AgentMeta = {
  model: string;
  harness_version: string;
  tools: string[];
  mcp_servers: McpServerStatus[];
  skills: string[];
};

/// Per-agent turn lists, keyed by `agent_id`. Render-time merge produces the
/// unified project transcript: `activeProject.agents.flatMap(id =>
/// transcripts[id]).sort_by(started_at)`.
export type TranscriptMap = Record<AgentId, Turn[]>;

export type RuntimeMap = Record<AgentId, AgentRuntime>;
