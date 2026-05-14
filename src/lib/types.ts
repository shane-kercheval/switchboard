// Wire-format types. Must match the Rust definitions in
// `crates/harness/src/events.rs` and `crates/core/src/{agent,project}.rs`,
// which use `#[serde(tag = "type", rename_all = "snake_case")]`.

export type TurnId = string;
export type AgentId = string;
export type ProjectId = string;

export type FailureKind = "harness_error" | "adapter_failure";
// Future: "timeout" — added if/when an active per-turn timeout lands.

export type TurnOutcome =
  | { status: "completed" }
  | { status: "failed"; kind: FailureKind; message: string };
// Future: | { status: "cancelled"; source: CancelSource } — added in M4 when per-turn cancel lands.

export type NormalizedEvent =
  | { type: "turn_start"; turn_id: TurnId; started_at: string }
  | { type: "content_chunk"; turn_id: TurnId; text: string }
  | { type: "turn_end"; turn_id: TurnId; outcome: TurnOutcome; ended_at: string };

// Synthetic reducer input — fired by the AgentPane's heartbeat timer when no
// `content_chunk` activity has been observed for HEARTBEAT_TIMEOUT_MS while a
// turn is in flight. The reducer treats it as a transition to "failed."
//
// Lives on the reducer-input union (not the wire-format `NormalizedEvent`)
// because it's frontend-synthesized, not emitted by the dispatcher. The
// `at` timestamp is supplied by the caller (AgentPane) at fire time —
// keeping the reducer pure (no `new Date()` inside `reduce()`).
export type HeartbeatTimeout = { type: "heartbeat_timeout"; turn_id: TurnId; at: string };

export type ReducerInput = NormalizedEvent | HeartbeatTimeout;

export type Turn =
  | {
      id: TurnId;
      role: "user";
      text: string;
      submittedAt: string;
    }
  | {
      id: TurnId;
      role: "agent";
      text: string;
      status: "streaming" | "complete" | "failed";
      error?: string;
      // Cause of the failure when status is "failed". Preserved so M2's
      // retry-policy UI can distinguish recoverable from non-recoverable
      // failures (e.g., HarnessError → suggest retry; AdapterFailure →
      // suggest "report bug"). Undefined for streaming/complete turns.
      errorKind?: FailureKind;
      startedAt: string;
      endedAt?: string;
    };

export type AgentTranscript = {
  agentId: AgentId;
  turns: Turn[];
};

// Mirror of `crates/core::AgentRecord`. `session_id` is `null` for harnesses
// that assign their own session ID (Codex, M2+); for Claude Code it's
// pre-generated at registration time.
export type AgentRecord = {
  id: AgentId;
  project_id: ProjectId;
  name: string;
  harness: "claude_code";
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
// M1 heuristic: 60s with no `content_chunk` is a "stream is silent" signal.
// This is appropriate for text-only turns where chunks arrive continuously.
//
// M2 NOTE: when tool calls land (`ToolStarted` / `ToolCompleted` events),
// this rule becomes unsafe — a long tool execution can legitimately produce
// zero `content_chunk`s for minutes while emitting other (parser-skipped)
// stream events. Revisit then: heartbeat on any event, or expose tool-call
// lifecycle markers to the reducer so the timer resets appropriately.
