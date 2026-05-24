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

// ContentChunk.kind discriminates rendering. `thinking` is reserved in the
// wire format but not currently emitted — future reasoning UI can land
// without a wire-format break.
export type ContentKind = "text" | "thinking";

// ToolStarted.kind discriminates tool origin so the UI can label calls
// without scraping the name. `plugin` and `other` are
// reserved-but-not-currently-emitted (same pattern as ContentKind.thinking).
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
  reasoning_output_tokens?: number | null;
  context_window?: number | null;
  total_cost_usd?: number | null;
};

// Identifier minted by the dispatcher for every accepted send (idle or
// queued), returned synchronously from `send_message`. The turn later started
// for that message carries the same `message_id` on its `turn_start`, so the
// optimistic user bubble (keyed by `message_id`) can flip to running. A send
// that fails before any turn starts surfaces as `message_failed`.
export type MessageId = string;

export type NormalizedEvent =
  | { type: "turn_start"; turn_id: TurnId; message_id: MessageId; started_at: string }
  | { type: "content_chunk"; turn_id: TurnId; kind: ContentKind; text: string }
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
  | { type: "message_failed"; message_id: MessageId; agent_id: AgentId; error: string; at: string };

// Synthetic reducer input — fired by the state module's heartbeat timer
// when no per-turn activity has been observed for HEARTBEAT_TIMEOUT_MS
// while a turn is in flight. The reducer treats it as a transition to
// "failed."
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
      started_at: string;
      ended_at?: string | null;
      status: "streaming" | "complete" | "failed";
      items: LoadedTurnItem[];
      usage?: TurnUsage | null;
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
  warnings?: ParseWarning[];
};

export type ReducerInput = NormalizedEvent | HeartbeatTimeout | Hydrate;

// Internal state types (Turn, AgentRuntime, etc.) live in
// `src/lib/state/types.ts`. This file is wire-format-only.

// Mirror of `crates/core::AgentRecord`. `session_id` is `null` for harnesses
// that assign their own session ID (Codex and Antigravity — set to `null`
// for life; the per-agent session-link sidecar is the system-of-record for
// the captured conversation UUID); for Claude Code and Gemini it's
// pre-generated at registration time.
export type HarnessKind = "claude_code" | "codex" | "gemini" | "antigravity";

/// Result of the startup-time per-harness probes. `binary` is the
/// `which`-on-PATH check; `auth` is the best-effort subscription-auth
/// detection (Codex only — Claude's auth lives in the macOS keychain with
/// no reliable file signal, deferred to a future release).
///
/// **Discriminated union, not a flat record.** The v1 invariant "auth
/// detection is Codex-only; Claude's auth is always `unsupported`" is
/// encoded structurally in the type: the Claude variant's `auth` is
/// the literal `"unsupported"`, the Codex variant's covers the real
/// probe states. Consumers narrow on `harness` before accessing `auth`,
/// and `a.auth === "missing"` is only type-checkable for the Codex
/// variant. This eliminates the runtime-guard-with-defensive-arm
/// pattern that a flat record forces on every consumer.
///
/// A future harness with file-detectable auth adds a new variant
/// rather than widening Claude's auth field — the compiler then forces
/// every consumer (banner copy, form gating) to acknowledge the new
/// case explicitly.
///
/// **State semantics** (per the Codex variant; Claude has only
/// `unsupported`):
/// - `"checking"`: probe in flight; the initial value at mount. Form
///   gating treats this as not-selectable (silent disable — no scary
///   "Checking…" copy) so a user racing the probe can't submit before
///   we know.
/// - `"available"`: probe completed positively.
/// - `"missing"`: probe completed negatively. Banner copy is actionable
///   (install link / `codex login`).
/// - `"unsupported"`: only on the Claude variant.
///
/// Replacing the original optimistic-"available" default with `"checking"`
/// makes the pre-probe semantics structural rather than convention-based:
/// fail-closed by type, not by polite hope.
export type BinaryState = "available" | "missing" | "checking";

export type HarnessAvailability =
  | {
      harness: "claude_code";
      binary: BinaryState;
      auth: "unsupported";
    }
  | {
      harness: "codex";
      binary: BinaryState;
      auth: "available" | "missing" | "checking";
    }
  | {
      harness: "gemini";
      binary: BinaryState;
      auth: "available" | "missing" | "checking";
    }
  | {
      harness: "antigravity";
      binary: BinaryState;
      auth: "available" | "missing" | "checking";
    };

/// Structured banner shape. The App.svelte banner-stack ordering rule:
/// binary-missing banners first; for any harness whose binary is missing,
/// the auth banner is suppressed (auth is irrelevant if the CLI isn't
/// installed).
///
/// **v1 invariant encoded in the type**: `auth_missing` is restricted to
/// harnesses with detectable auth — Codex's `~/.codex/auth.json`,
/// Gemini's `~/.gemini/settings.json`, and Antigravity's macOS Keychain
/// entry (service `gemini`, account `antigravity`). Claude's auth is
/// `"unsupported"` (keychain-based on macOS with no reliable probe yet —
/// see `HarnessAvailability` docstring). A future Claude auth probe must
/// add a new banner variant or extend the `harness` literal explicitly;
/// the closed set prevents accidental auth-banner copy from leaking onto
/// Claude rows.
export type HarnessBanner =
  | { kind: "binary_missing"; harness: HarnessKind }
  | { kind: "auth_missing"; harness: "codex" | "gemini" | "antigravity" };

export type AgentRecord = {
  id: AgentId;
  project_id: ProjectId;
  name: string;
  harness: HarnessKind;
  session_id: string | null;
  created_at: string;
};

export type ProjectSummary = {
  id: ProjectId;
  name: string;
  created_at: string;
};

export type DirectoryInfo = {
  path: string;
  has_switchboard: boolean;
  projects: ProjectSummary[];
};

export const HEARTBEAT_TIMEOUT_MS = 60_000;
// Heartbeat re-arms on any per-turn activity event for the heartbeat's
// tracked turn: `content_chunk`, `tool_started`, `tool_completed`. 60s of
// total silence across all three is the "stream is silent" threshold. Tool
// events are load-bearing here — a long shell command (build, test run,
// large grep) emits no `content_chunk`s for minutes and would otherwise
// trigger a false-positive failure. Agent-scoped events (`session_meta`,
// `rate_limit_event`) intentionally do NOT re-arm — they're not turn-anchored
// and can flow at any time without indicating turn progress.
