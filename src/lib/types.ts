// Wire-format types. Must match the Rust definitions in
// `crates/harness/src/events.rs` and `crates/core/src/{agent,project}.rs`,
// which use `#[serde(tag = "type", rename_all = "snake_case")]`.

export type TurnId = string;
export type AgentId = string;
export type ProjectId = string;

export type FailureKind = "harness_error" | "adapter_failure" | "auth_failure";
// Future: "timeout" — added if/when an active per-turn timeout lands.
// `auth_failure` is detected via stream events: Claude's
// `assistant.error == "authentication_failed"` and Codex's
// `turn.failed.error` containing `"401 Unauthorized"`.

// Who initiated a cancellation. Carried on the `cancelled` outcome.
export type CancelSource = "user" | "workflow" | "shutdown";

export type TurnOutcome =
  | { status: "completed" }
  | { status: "failed"; kind: FailureKind; message: string }
  | { status: "cancelled"; source: CancelSource };

// ContentChunk.kind discriminates rendering. `thinking` carries model reasoning,
// rendered distinct from (and subordinate to) the answer (the `ThinkingWidget`).
// Emitted by Antigravity (live + on reopen) and Claude Sonnet 4.6 (live + on
// reopen). Gemini's reasoning is disk-only and deliberately dropped
// (stale-on-reopen UX). Claude's redaction is per-model: Opus 4.8 redacts the
// text to empty, so its reasoning surfaces only as a non-rendering `liveness`
// event. See docs/research/harness-behavior.md §3.2 for per-harness reality.
export type ContentKind = "text" | "thinking";

// ToolStarted.kind discriminates tool origin so the UI can label calls
// without scraping the name. `plugin` and `other` are
// reserved-but-not-currently-emitted (a forward-compat pattern).
export type ToolKind = "builtin" | "mcp" | "plugin" | "other";

export type McpServerStatus = { name: string; status: string };

// Per-turn usage carried on `turn_end.usage`. `total_cost_usd` is Claude
// Code only (subscription auth has no dollar number for Codex). Tokens are
// not displayed by the current UI per the cost-surface contract; the wire
// format carries them so future versions can surface without a
// wire-break.
export type TurnUsage = {
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens?: number | null;
  cache_creation_input_tokens?: number | null;
  // Harness-reconciled input-side tokens occupying the context window after
  // this turn. The emitting adapter computes it because harnesses count cached
  // tokens differently (Claude: disjoint additions; Codex: a subset already in
  // input_tokens). Context utilization consumes this directly so the frontend
  // formula stays harness-agnostic. `null` where a harness doesn't compute it.
  context_input_tokens?: number | null;
  reasoning_output_tokens?: number | null;
  context_window?: number | null;
  total_cost_usd?: number | null;
};

// Per-turn real-spend attribution — the gate for showing a turn's cost and an
// overage marker inline on the message. Stamped per turn by the adapter, so the
// frontend renders on `real_spend` without a harness check. `real_spend` is the
// harness-agnostic gate (for Claude == `is_overage`, since subscription cost is
// only real money in overage); `is_overage` is the Claude-derived source kept
// distinct so the seam stays honest; `overage_resets_at` (ISO-8601) is the
// credit-window reset for the marker tooltip when reported. Absent/`null` =
// no real-spend info → show neither cost nor marker.
export type TurnSpend = {
  real_spend: boolean;
  is_overage: boolean;
  overage_resets_at?: string | null;
};

// Identifier minted by the dispatcher for every accepted send (idle or
// queued), returned synchronously from `send_message`. The turn later started
// for that message carries the same `message_id` on its `turn_start`, so the
// optimistic user bubble (keyed by `message_id`) can flip to running. A send
// that fails before any turn starts surfaces as `message_failed`.
export type MessageId = string;

// Identifier the frontend mints once per Send action and passes on every
// per-recipient `send_message` call, so a fan-out's turns share it (the
// backend groups the user's message once by `send_id`, and `cancel_send` is
// scoped to it).
export type SendId = string;

export type NormalizedEvent =
  | { type: "turn_start"; turn_id: TurnId; message_id: MessageId; started_at: string }
  | { type: "content_chunk"; turn_id: TurnId; kind: ContentKind; text: string }
  // Content-free liveness signal: the harness is still alive mid-turn but
  // produced no renderable content (e.g. Claude Opus 4.8's redacted thinking
  // deltas). Re-arms the per-turn heartbeat; renders nothing.
  | { type: "liveness"; turn_id: TurnId }
  | {
      type: "tool_started";
      turn_id: TurnId;
      tool_use_id: string;
      kind: ToolKind;
      name: string;
      // serde_json::Value on the Rust side; arbitrary JSON shape here.
      input: unknown;
    }
  | {
      type: "tool_completed";
      turn_id: TurnId;
      tool_use_id: string;
      output: string;
      is_error: boolean;
    }
  | {
      type: "turn_end";
      turn_id: TurnId;
      outcome: TurnOutcome;
      ended_at: string;
      usage?: TurnUsage | null;
      spend?: TurnSpend | null;
      // The model and reasoning effort this turn ran on, for the per-turn
      // transcript footer (M6). `model` populated for all model-reporting
      // harnesses; `effort` Codex-only. Absent → render nothing.
      model?: string | null;
      effort?: string | null;
      // Live-matched stable hydration key — the same per-turn id this turn will
      // carry on disk, so the hydrate merge can recognize a turn that streamed
      // live and is later re-read as one turn. Populated only for live-matched
      // harnesses (Claude's final assistant message.id); absent otherwise.
      hydration_key?: string | null;
    }
  | { type: "rate_limit_event"; agent_id: AgentId; info: unknown }
  | {
      type: "session_meta";
      agent_id: AgentId;
      model: string;
      harness_version: string;
      tools: string[];
      mcp_servers: McpServerStatus[];
      skills: string[];
      raw: unknown;
    }
  // Emitted by the dispatcher as the last event on the per-agent channel
  // for a dispatch — immediately before the dispatcher's internal idle
  // guard drops. Two contracts the frontend may rely on:
  //   1. Channel-ordering: no further events arrive for this dispatch.
  //   2. Sendability: when this event is processed, a fresh send to the
  //      same agent will not return `Busy`.
  // The frontend's compose-bar gates Send enablement on
  // `runtimes[recipient].run_status === "idle"`; `agent_idle` is the
  // sole event that flips `run_status` from `processing` back to `idle`
  // (the only path out of `processing` — see `AgentRuntime.run_status`
  // docstring in `src/lib/state/types.ts` for the full state machine).
  | { type: "agent_idle"; agent_id: AgentId }
  // A send failed before any turn started (journal write failed, or the
  // adapter failed to launch pre-`turn_start`). Keyed by `message_id` — there
  // is no live turn. Carries no prompt; the frontend still holds the
  // optimistically-rendered text and marks that bubble failed.
  | { type: "message_failed"; message_id: MessageId; agent_id: AgentId; error: string; at: string }
  // A queued send was cancelled before it started (its backlog item was dropped
  // by cancel_send / cancel_agent). Keyed by `message_id`, no `turn_id`. The
  // authoritative signal that a not-yet-started send is gone — the frontend
  // renders its cancelled row from this rather than optimistically guessing.
  | { type: "message_cancelled"; message_id: MessageId; agent_id: AgentId; at: string };

// Synthetic reducer input — fired by the state module's heartbeat timer when
// no per-turn activity has been observed for HEARTBEAT_TIMEOUT_MS while a turn
// is in flight. It does NOT fail the turn: a silent-but-alive turn still holds
// the backend's busy-lock, so the frontend only surfaces the silence by
// setting a transient `quiet_since` timestamp on the agent runtime (cleared on
// the next activity event, or on turn end). Real stream death is failed by the backend.
//
// Lives on the reducer-input union (not the wire-format `NormalizedEvent`)
// because it's frontend-synthesized, not emitted by the dispatcher. The
// `at` timestamp is supplied by the caller (the state module's timer
// callback) at fire time — keeping the reducer pure (no `new Date()`
// inside `reduce()`).
export type HeartbeatTimeout = { type: "heartbeat_timeout"; turn_id: TurnId; at: string };

// Mirror of Rust `LoadedTranscript` from `crates/harness/src/transcript.rs`.
// Used by the transcript-hydration flow: `load_transcript` Tauri command
// returns this shape; the reducer's `hydrate` input consumes it.
export type LoadedTranscript = {
  turns: LoadedTurn[];
  meta?: SessionMetaInfo | null;
  last_rate_limit?: unknown;
  /// Capture time of `last_rate_limit` when restored from the per-agent
  /// metadata sidecar (a stream-only/class-C value, e.g. Claude's overage
  /// signal, that would otherwise be lost on restart). ISO-8601 string.
  /// `null` for live values and for class-B (already-durable) sources;
  /// drives the UI "as of …" staleness qualifier.
  last_rate_limit_as_of?: string | null;
  warnings: ParseWarning[];
};

export type ParseWarning = { line_number: number; reason: string };

export type SessionMetaInfo = {
  model: string;
  harness_version: string;
  tools: string[];
  mcp_servers: McpServerStatus[];
  skills: string[];
};

// Wire shape of `crate::transcript::Turn` — matches the in-state `Turn`
// shape but is separate so the on-the-wire deserialization is explicit
// and the state module can defensively normalize.
export type LoadedTurn =
  | { role: "user"; turn_id: TurnId; agent_id: AgentId; started_at: string; text: string }
  | {
      role: "agent";
      turn_id: TurnId;
      agent_id: AgentId;
      send_id?: SendId | null;
      started_at: string;
      ended_at?: string | null;
      status: "streaming" | "complete" | "failed";
      items: LoadedTurnItem[];
      usage?: TurnUsage | null;
      // Per-turn cost/overage re-joined from the turn-metadata sidecar on
      // reopen. Present only on real-spend turns that were persisted; absent
      // for normal-quota and pre-feature turns (render neither cost nor marker).
      spend?: TurnSpend | null;
      // Per-turn model + effort reconstructed by the backend from the harness
      // session file (not a sidecar — harness-owned). `model` for all
      // model-reporting harnesses; `effort` Codex-only. Absent → render nothing.
      model?: string | null;
      effort?: string | null;
      // Stable hydration key (re-parse-invariant): the hydrate merge dedups on
      // it so re-reading a session file never duplicates this turn. Absent for
      // keyless harnesses (Antigravity) — the merge falls back to `turn_id`.
      hydration_key?: string | null;
    };

export type LoadedTurnItem =
  | { item_kind: "text"; kind: ContentKind; text: string }
  | {
      item_kind: "tool";
      tool_use_id: string;
      kind: ToolKind;
      name: string;
      input: unknown;
      output?: string | null;
      is_error?: boolean | null;
      started_at: string;
      completed_at?: string | null;
    };

// Hydrate reducer input — frontend-synthesized after a `load_transcript`
// IPC reply lands. Per-agent scope. Non-destructive: existing in-flight
// turns + already-populated runtime metadata are preserved (live > disk).
//
// `warnings` carries `ParseWarning` entries surfaced by the per-harness
// parser (stale Codex sidecar, malformed JSONL line, etc.) — non-blocking;
// the hydration still succeeds with whatever could be salvaged. The
// runtime reducer copies them onto `AgentRuntime.parse_warnings` for the
// sidebar to render as a non-blocking indicator.
export type Hydrate = {
  type: "hydrate";
  agent_id: AgentId;
  turns: LoadedTurn[];
  meta?: SessionMetaInfo | null;
  last_rate_limit?: unknown;
  /// Capture time of `last_rate_limit` from the metadata sidecar (see
  /// `LoadedTranscript.last_rate_limit_as_of`). `null` when the value is
  /// live or class-B.
  last_rate_limit_as_of?: string | null;
  warnings?: ParseWarning[];
};

export type ReducerInput = NormalizedEvent | HeartbeatTimeout | Hydrate;

// Internal state types (Turn, AgentRuntime, etc.) live in
// `src/lib/state/types.ts`. This file is wire-format-only.

export type HarnessKind = "claude_code" | "codex" | "gemini" | "antigravity";

/// State of the `which`-on-PATH binary probe for a single harness.
/// - `"checking"`: probe in flight (the initial value at mount). Form
///   gating treats this as not-selectable (silent disable — no scary
///   "Checking…" copy) so a user racing the probe can't submit before
///   the result lands. Fail-closed by type, not by polite hope.
/// - `"available"`: probe completed positively.
/// - `"missing"`: probe completed negatively. Banner copy is actionable
///   (install link).
export type BinaryState = "available" | "missing" | "checking";

/// Frontend availability surface. Tracks binary presence only — auth is
/// **not** a frontend concern in v1: a logged-out harness is discovered
/// reactively when the user sends, surfaced as an `AuthFailure` turn in
/// the transcript (with an authored actionable message per adapter). No
/// proactive banner, no picker gate on auth grounds.
///
/// The backend `check_*_auth` Tauri commands exist for the getting-started
/// surface (no-project state) to consume; nothing in the working UI calls
/// them.
export type HarnessAvailability = {
  harness: HarnessKind;
  binary: BinaryState;
};

/// Structured banner shape. The only banner variant is the
/// binary-missing case — a missing CLI is a real install problem the
/// user needs to act on before sends will work at all. Auth has no
/// banner: a logged-out harness is surfaced reactively in the transcript
/// when the user sends.
export type HarnessBanner = {
  kind: "binary_missing";
  harness: HarnessKind;
};

/// Install status of a harness CLI for the getting-started surface.
/// Mirrors the Rust `HarnessInstallStatus`. A missing binary is
/// `installed: false` with `version: null` — data, not an error.
export type HarnessInstallStatus = {
  installed: boolean;
  version: string | null;
};

/// Mirror of the Rust `SessionLocator` (externally tagged enum) — the identity
/// Switchboard uses to find and resume a harness's conversation. Most harnesses
/// identify a session by one UUID (`{ uuid }`); Codex needs a thread-id string
/// plus the local date its rollout file is partitioned under (`{ codex }`).
export type SessionLocator =
  | { uuid: string }
  | { codex: { thread_id: string; partition_date: string } };

// Mirror of `crates/core::AgentRecord`. `session_locator` is `null` for
// harnesses that assign their own session id at runtime (Codex and Antigravity)
// until the first dispatch captures it; for Claude Code and Gemini it's
// pre-generated at registration time as a `{ uuid }` locator.
export type AgentRecord = {
  id: AgentId;
  project_id: ProjectId;
  name: string;
  harness: HarnessKind;
  session_locator: SessionLocator | null;
  created_at: string;
};

export type ProjectSummary = {
  id: ProjectId;
  name: string;
  created_at: string;
};

// Mirror of Rust `ProjectListing` (`crates/app/src/commands.rs`) — one row of
// the flat cross-directory project list. `directory` is the owning directory's
// path (label + spawn cwd); `available` is whether that directory is currently
// loaded/readable; `last_activity` is the recency-ordering key (journal mtime
// or `created_at`).
export type ProjectListing = {
  id: ProjectId;
  name: string;
  created_at: string;
  directory: string;
  available: boolean;
  last_activity: string;
  /// User-global view-state (from `workspace.yaml`): the user archived this
  /// project, hiding it from the default `Active` view. Not on-disk project state.
  archived: boolean;
};

// Mirror of Rust `WorkspaceDirectoryInfo` / `WorkspaceDirectories`. The
// switcher renders directory rows independent of projects (so empty directories
// appear), and `persistable === false` means an existing `workspace.yaml`
// couldn't be read this session — surfaced distinctly from a fresh install so a
// transient read error doesn't lure the user into re-adding directories that
// then silently fail to save.
export type WorkspaceDirectoryInfo = {
  path: string;
  available: boolean;
};

export type WorkspaceDirectories = {
  directories: WorkspaceDirectoryInfo[];
  persistable: boolean;
};

// Mirror of Rust `ProjectConversation` / `ConversationItem` / `OutcomeStatus` /
// `AgentConversationMeta` (`crates/app/src/commands.rs`). The post-restart
// unified history: the three `ConversationItem` kinds are disjoint sources
// (user messages ← journal, agent content ← harness files, outcome markers ←
// journal), so there is no cross-source dedup. Items arrive pre-sorted by
// timestamp (user message before its content/markers at equal instants).
export type OutcomeStatus = "cancelled" | "failed";

export type ConversationItem =
  | {
      kind: "user_message";
      // Stable render identity: the journal `send_id` for a dispatched send, the
      // harness `turn_id` for an imported prompt. Keys the row; not a join key.
      id: string;
      // Grouping key for a fan-out. Null for an imported prompt that predates
      // journaling (an attached session's history) — it has no journal Send.
      send_id?: string | null;
      agent_ids: AgentId[];
      text: string;
      at: string;
    }
  | {
      kind: "agent_turn";
      turn_id: TurnId;
      agent_id: AgentId;
      // Recovered by joining this turn's `turn_id` against the journal's Send
      // records, so a historical fan-out's responses group by `send_id` exactly
      // like live ones. Null when no Send matched (pre-journal / failed write).
      send_id?: SendId | null;
      started_at: string;
      ended_at?: string | null;
      status: "streaming" | "complete" | "failed";
      items: LoadedTurnItem[];
      usage?: TurnUsage | null;
      // Per-turn cost/overage re-joined from the turn-metadata sidecar on
      // reopen — same source + meaning as `LoadedTurn.spend`.
      spend?: TurnSpend | null;
      // Stable hydration key — same source + meaning as `LoadedTurn.hydration_key`.
      hydration_key?: string | null;
    }
  | {
      kind: "outcome";
      turn_id: TurnId;
      send_id: string;
      agent_id: AgentId;
      status: OutcomeStatus;
      reason?: string | null;
      at: string;
    };

// Per-agent metadata carried alongside the merged items. `warnings` and
// `load_error` are agent-scoped: one agent's transcript failing to load leaves
// its `load_error` set (and turns absent) while the rest of the project still
// renders.
export type AgentConversationMeta = {
  agent_id: AgentId;
  meta?: SessionMetaInfo | null;
  last_rate_limit?: unknown;
  /// Capture time of `last_rate_limit` from the metadata sidecar (ISO-8601);
  /// `null`/absent for live or class-B sources. See
  /// `LoadedTranscript.last_rate_limit_as_of`.
  last_rate_limit_as_of?: string | null;
  warnings: ParseWarning[];
  load_error?: string | null;
};

export type ProjectConversation = {
  items: ConversationItem[];
  agents: AgentConversationMeta[];
};

export type DirectoryInfo = {
  path: string;
  has_switchboard: boolean;
  projects: ProjectSummary[];
};

export const HEARTBEAT_TIMEOUT_MS = 60_000;
// Heartbeat re-arms on any per-turn sign of life for the tracked turn:
// `content_chunk`, `liveness`, `tool_started`, `tool_completed`. 1 minute of
// total silence across all of these is the "stream is silent" threshold (kept
// short because the indicator is harmless and the user can always cancel).
// `liveness` and tool events are load-bearing: a long redacted thinking block
// emits only `liveness` (Claude Opus 4.8's redacted thinking deltas) and a
// streaming tool input emits only `liveness` (input_json_delta), while a long
// shell command (build, test run) emits no events between `tool_started` and
// `tool_completed`. On
// expiry the turn is NOT failed — it is marked transiently quiet (see
// `AgentRuntime.quiet_since`), because a silent-but-alive turn still holds the
// backend busy-lock. The threshold is therefore "when to surface the silence,"
// not "when to fail." The footer counts up from the quiet onset once crossed.
// Agent-scoped events (`session_meta`, `rate_limit_event`) intentionally do NOT
// re-arm — they're not turn-anchored and can flow at any time without
// indicating turn progress.

// ── Prompt providers (MCP server management — system-design §6) ───────────────
// Mirror the Rust `#[serde(tag = "state", rename_all = "snake_case")]` shape.
export type ProviderStatus =
  | { state: "ok"; prompt_count: number }
  | { state: "errored"; message: string }
  | { state: "store_unavailable" }
  | { state: "unknown" };

export type McpProviderInfo = {
  name: string;
  url: string;
  has_token: boolean;
  status: ProviderStatus;
};

// A prompt as listed from the cache. Mirrors the Rust `Prompt`. `provider` is
// the prefix it resolves under (`local` or an MCP provider's name); `arguments`
// are the declared template variables the composer renders as inputs.
export type PromptArgument = {
  name: string;
  description: string | null;
  required: boolean;
};

export type Prompt = {
  provider: string;
  name: string;
  // Human-friendly display name (MCP `title`); `name` is the slug identifier.
  // Null for local prompts and servers that omit it — the UI falls back to `name`.
  title: string | null;
  description: string | null;
  arguments: PromptArgument[];
  tags: string[];
};

// The finished text returned by `render_prompt` — what the agent receives.
export type RenderedPrompt = {
  text: string;
};
