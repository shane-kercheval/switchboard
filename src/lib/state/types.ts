// Internal frontend state types for the M2.5 unified-stream model.
//
// Companion to `$lib/types.ts` (wire-format types only). Component state
// shape lives here; events on the IPC boundary live there. Reducers in
// this module's `reducers.ts` consume wire-format events and produce the
// shapes defined below.
//
// **Naming convention.** snake_case throughout, matching the wire-format
// (`turn_id`, `agent_id`, `started_at`, `ended_at`). Aligning state shape
// with wire shape eliminates rename-at-IPC-boundary drift — load-bearing
// for M2.6's session-file rehydration, where disk-parsed turns flow
// through the same reducer as live-stream turns without any field
// translation.

import type {
  AgentId,
  ContentKind,
  FailureKind,
  McpServerStatus,
  ParseWarning,
  ToolKind,
  TurnId,
  TurnUsage,
} from "$lib/types";

/// Role-mixed turn entries — user prompts and agent responses live in the
/// same chronological stream. Harness session files store user and assistant
/// events as separate entries; this shape matches that, so M2.6 rehydration
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
      started_at: string;
      text: string;
    }
  | {
      role: "agent";
      turn_id: TurnId;
      agent_id: AgentId;
      started_at: string;
      ended_at?: string;
      status: "streaming" | "complete" | "failed";
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
      /// AdapterFailure → suggest "report bug"; AuthFailure → "run claude login").
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
  /// Mirrors wire `content_chunk.kind`. `"thinking"` is reserved for M3+
  /// reasoning UI but not emitted in M2.
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
  /// Mirrors wire `tool_started.kind`. `"builtin"` / `"mcp"` are emitted in
  /// M2; `"plugin"` / `"other"` are reserved.
  kind: ToolKind;
  name: string;
  input: unknown;
  output?: string;
  is_error?: boolean;
  started_at: string;
  completed_at?: string;
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
/// - `last_error`: sidebar display surface for the most-recent failure.
///   Does NOT gate Send. Cleared on the next successful `turn_end`.
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
  /// - `"starting"` — user clicked Send; IPC is in flight; `TurnStart` hasn't
  ///   arrived yet. Compose-bar Send disabled. Without this state, a
  ///   second click in the ~100ms gap between user-submit and backend-emit
  ///   would slip through the frontend gate and surface a confusing
  ///   `Busy` error from the dispatcher.
  /// - `"processing"` — `TurnStart` has arrived; the backend's
  ///   `AgentIdleGuard` is held. Compose-bar Send disabled.
  ///
  /// State machine:
  ///
  /// ```
  /// idle  --dispatchUserTurn-->  starting
  /// starting  --(turn_start event)-->  processing
  /// starting  --(failSendStart action)-->  idle
  /// processing  --(agent_idle event)-->  idle
  /// ```
  ///
  /// `failSendStart` is the **only legal path** from `starting` back to
  /// `idle` without going through `processing` — `agent_idle` is guarded
  /// to only flip `processing → idle` (a stray `agent_idle` in the
  /// starting window must not race the gate open).
  ///
  /// **Stuck-in-starting diagnostic.** If an agent remains in `"starting"`
  /// indefinitely, either: (a) the dispatcher accepted `send_message` but
  /// never emitted `TurnStart` (dispatcher regression — TurnStart is
  /// dispatcher-emitted and contractually guaranteed per AGENTS.md), or
  /// (b) the compose-bar caller forgot to invoke `failSendStart` on IPC
  /// failure. Look at `crates/dispatcher/src/lib.rs::send_message` and
  /// the compose-bar's catch block respectively.
  run_status: "idle" | "starting" | "processing";
  /// The turn the heartbeat timer is tracking. Distinct from `run_status`
  /// because for fast-events races the entire stream can fire before the
  /// IPC reply lands; this tracking key lets late events still extend the
  /// timer correctly.
  in_flight_turn_id?: TurnId;
  /// Display-only surface for the last turn's failure. Set on failed
  /// `TurnEnd` or `heartbeat_timeout`; cleared on the next successful turn.
  /// Does NOT gate sendability.
  last_error?: { message: string; kind: FailureKind };
  /// Populated by live `SessionMeta` events or M2.6 disk hydration.
  /// Undefined on agents whose first dispatch hasn't happened yet.
  meta?: AgentMeta;
  /// Most-recent Codex `RateLimitEvent.info` payload. Opaque — the
  /// renderer reads `primary.used_percent` for the sidebar quota signal.
  /// Undefined for Claude agents (Claude doesn't emit rate-limit events).
  last_rate_limit?: unknown;
  /// Disk-rehydration lifecycle. M2.6 owns the `"loading"` and `"failed"`
  /// transitions; M2.5 always lands `"complete"` for newly-created agents
  /// (nothing to hydrate at creation time). Compose-bar Send is gated on
  /// this being `"complete"` (or `"failed"`, which enters degraded-dispatch
  /// mode with a banner).
  hydration_status: "pending" | "loading" | "complete" | "failed";
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
