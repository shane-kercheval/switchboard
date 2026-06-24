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

// Result of resolving a manual cross-agent forward (`forward_message`). Mirrors
// `ForwardOutcome` in `crates/app/src/commands.rs` (`tag = "status"`). The
// backend resolves + composes but does NOT dispatch; the frontend dispatches the
// returned `resolved.body` through the normal send path (so the forward groups,
// cancels, and renders exactly like any send). `resolved.body` is the composed
// message (the frontend can't compose it — the forwarded blocks hold each
// source's resolved output, which only the backend has); `resolved.skipped`
// names sources that had no output (the partial-empty caption). `invalidated` /
// `cancelled` mean nothing resolved (restore the composer).
export type ForwardOutcome =
  | { status: "resolved"; body: string; skipped: string[] }
  | { status: "invalidated"; reason: string }
  | { status: "cancelled" };

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

// One staged file attached to a send. Mirrors the Rust `switchboard_core::Attachment`
// wire shape. `kind` drives the chip label prefix and thumbnail-vs-filename
// rendering; `"unknown"` is the cross-version fallback for a kind a newer build
// wrote that this build doesn't recognize (renders as a generic file).
export type AttachmentKind = "image" | "text" | "file" | "unknown";

export type Attachment = {
  label: string;
  kind: AttachmentKind;
  path: string;
  original_name: string;
};

// Result of staging a dropped file (`stage_attachment`): the staged absolute
// path plus the original basename. The frontend assigns `label`/`kind` and
// builds the full `Attachment` from these.
export type StagedAttachment = {
  path: string;
  original_name: string;
};

export type NormalizedEvent =
  | {
      type: "turn_start";
      turn_id: TurnId;
      message_id: MessageId;
      // The originating send's id, shared across a fan-out's recipients. Lets the
      // live UI group concurrent turns of one send side-by-side even when the
      // frontend didn't originate the send (e.g. a workflow dispatch), which has
      // no local `pending_sends` entry to derive the grouping from.
      send_id: SendId;
      started_at: string;
    }
  // A user-side message a workflow `send` dispatched to this agent (the frontend
  // has no optimistic user turn for a backend-originated send). The reducer turns
  // it into a user turn; a fan-out's recipients share `send_id` so they group into
  // one user row + per-recipient columns. Manual sends don't use this.
  | { type: "user_message"; send_id: SendId; text: string; at: string }
  | { type: "content_chunk"; turn_id: TurnId; kind: ContentKind; text: string }
  // Content-free liveness signal: the harness is still alive mid-turn but
  // produced no renderable content (e.g. Claude Opus 4.8's redacted thinking
  // deltas). Re-arms the per-turn heartbeat; renders nothing.
  | { type: "liveness"; turn_id: TurnId }
  // Early dedup identity for the in-flight turn (Claude). The reducer stamps
  // `hydration_key` onto the live turn so a concurrent disk re-read collapses
  // against it instead of duplicating. `hydration_key` is the first assistant
  // message id — the same value `turn_end` carries.
  | { type: "turn_identity"; turn_id: TurnId; hydration_key: string }
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
      // transcript footer. `model` populated for all model-reporting
      // harnesses; `effort` Codex-only. Absent → render nothing.
      model?: string | null;
      effort?: string | null;
      // Live-matched stable hydration key — the same per-turn id this turn will
      // carry on disk, so the hydrate merge can recognize a turn that streamed
      // live and is later re-read as one turn. Populated only for live-matched
      // harnesses (Claude's *first* non-subagent assistant message.id — distinct
      // from the cost-join's final id, parse-invariant so a mid-flight re-read
      // dedups correctly); absent otherwise.
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
  | {
      type: "message_failed";
      message_id: MessageId;
      // The **durably recorded** send this failure belongs to, or `null` if the
      // send never reached the journal. When set, a backend-originated send
      // (workflow) attaches its failed marker via this; when `null` it renders no
      // row (reload can't reconstruct an unrecorded send). Manual sends ignore it
      // and resolve via `pending_sends`.
      send_id: SendId | null;
      agent_id: AgentId;
      error: string;
      at: string;
    }
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
/// - `"missing"`: probe completed negatively.
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
  // The user's selected model + reasoning effort (intent), shown in the sidebar.
  // `null`/absent for a no-capability harness (Antigravity carries
  // neither; Gemini carries no effort) or a pre-feature agent.
  model?: string | null;
  effort?: string | null;
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

// --- Git view (mirror of `switchboard_git` model + `crates/app` RepoListing) --
// Branch-primary, two-level status (see the crate docs): branch-level signals on
// BranchView/RemoteBranchView, worktree-level on WorktreeView. `null` fields are
// the Rust `Option::None` ("couldn't determine") wire form.

// Mirror of Rust `SyncState` (`#[serde(tag = "kind", rename_all = "snake_case")]`)
// — a branch's position vs. its own upstream. `local_only` (never pushed) is
// deliberately distinct from `unknown` (couldn't compute).
export type SyncState =
  | { kind: "in_sync" }
  | { kind: "ahead"; commits: number }
  | { kind: "behind"; commits: number }
  | { kind: "diverged"; ahead: number; behind: number }
  | { kind: "local_only" }
  | { kind: "unknown" };

export type WorktreeWarning = "orphaned" | "prunable";

export type WorktreeView = {
  path: string;
  dirty: boolean;
  untracked: boolean;
  detached_hash: string | null;
  warning: WorktreeWarning | null;
};

export type BranchView = {
  name: string;
  upstream: string | null;
  sync: SyncState;
  behind_base: number | null;
  merged: boolean | null;
  dangling: boolean;
  worktree: WorktreeView | null;
};

// Remote branches carry only the cleanup signals (merged, behind_base) — the
// local-branch fields are meaningless for a remote-tracking ref.
export type RemoteBranchView = {
  name: string;
  merged: boolean | null;
  behind_base: number | null;
};

export type RepoView = {
  root: string;
  name: string;
  default_branch: string | null;
  available: boolean;
  is_bare: boolean;
  local_branches: BranchView[];
  remote_branches: RemoteBranchView[];
  detached_worktrees: WorktreeView[];
};

// Mirror of Rust `LinkedProject` — a Switchboard project linked to a worktree by
// exact path-match.
export type LinkedProject = {
  id: ProjectId;
  name: string;
  directory: string;
};

// Mirror of Rust `RepoListing`. `repo` is the git read-model verbatim;
// `linked_projects` is a worktree-path → projects map joined at render time
// (linking is computed on the backend, kept alongside rather than nested into
// `RepoView`).
export type RepoListing = {
  repo: RepoView;
  linked_projects: Record<string, LinkedProject[]>;
};

// Mirror of Rust `ChangeKind` / `ChangedFile` — one changed file in a worktree,
// for the diff panel's file list.
export type ChangeKind = "modified" | "added" | "deleted" | "renamed" | "untracked";

export type ChangedFile = {
  path: string;
  change: ChangeKind;
};

// Mirror of Rust `CommitChanges` — one commit's changed files plus whether the
// commit still resolved. `found: false` (with empty `files`) means the commit is
// gone (gc'd / branch force-updated), distinct from a real commit that changed
// nothing.
export type CommitChanges = {
  found: boolean;
  files: ChangedFile[];
};

// Mirror of Rust `FileDiff` / `DiffHunk` / `DiffLine` / `DiffLineKind` — a file's
// working-tree diff as structured hunks (built from libgit2's structured diff, not
// parsed from unified text). The frontend renders rows directly from this.
export type DiffLineKind = "context" | "added" | "removed";

export type DiffLine = {
  origin: DiffLineKind;
  // Present only on the side where the line exists (added → old null; removed → new null).
  old_lineno: number | null;
  new_lineno: number | null;
  // Line text, without the +/-/space marker and without the trailing newline.
  content: string;
};

export type DiffHunk = {
  header: string;
  lines: DiffLine[];
};

export type FileDiff = {
  path: string;
  // Binary change: `hunks` is empty; the UI shows a placeholder instead of a body.
  binary: boolean;
  // The diff exceeded the render cap and was cut off (a prefix is still shown).
  truncated: boolean;
  // The file is past the inline-diff size limit and was never rendered (distinct
  // from `truncated`: nothing is shown, not a prefix). `too_large_bytes` is the
  // file size for the "open externally" message; null whenever `too_large` is false.
  too_large: boolean;
  too_large_bytes: number | null;
  hunks: DiffHunk[];
};

// Mirror of Rust `GitCommitSummary` — one commit's summary line for the branch
// commit list. `null` fields are the Rust `Option::None` wire form (e.g. a
// non-UTF-8 author identity). `authored_at` is RFC-3339.
export type GitCommitSummary = {
  oid: string;
  short_oid: string;
  subject: string;
  author_name: string | null;
  author_email: string | null;
  authored_at: string | null;
  branch_work: boolean;
  // True when this local commit isn't on its upstream yet ("not pushed").
  unpushed: boolean;
};

// Mirror of Rust `CommitRangeKind` (a bare snake_case string on the wire).
// Unpushed commits are flagged per-commit within the `recent` range rather than
// being their own kind.
export type CommitRangeKind = "recent" | "incoming";

// Mirror of Rust `GitCommitRange` — a capped, labelled slice of a branch's
// history (the local `recent` list, plus `incoming` when the upstream is ahead).
export type GitCommitRange = {
  kind: CommitRangeKind;
  label: string;
  commits: GitCommitSummary[];
  truncated: boolean;
};

// Mirror of Rust `BranchKind` — which ref namespace a commit read targets.
export type BranchKind = "local" | "remote";

// How the diff panel lays out a file's changes. Persisted in `config.yaml`.
export type DiffStyle = "side_by_side" | "unified";

// Mirror of Rust `Preferences` (`crates/app/src/preferences.rs`) — backend-owned
// `config.yaml`. `editor_command` defaults to "code"; null → OS default
// folder-open. `terminal_app` defaults to "Terminal"; `diff_style` defaults to
// "unified". `show_builtins` defaults to true (the read-only built-in prompts &
// workflows appear in the pickers; off → only the user's own content).
// Theme is NOT here — it stays in frontend localStorage (a device-local concern).
export type Preferences = {
  editor_command: string | null;
  terminal_app: string;
  diff_style: DiffStyle;
  show_builtins: boolean;
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
      // Files attached to this send, from the grouped journal `Send` (identical
      // across a fan-out's recipients). The backend always sends this (possibly
      // empty); optional here to match the additive-field style of this union and
      // tolerate pre-feature fixtures/blobs. Absent for an imported prompt.
      attachments?: Attachment[];
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
      // Per-turn model + effort reconstructed from the harness session file —
      // same source + meaning as `LoadedTurn.model`/`effort`. Carried so the
      // footer's model survives restart (the project-conversation path, not just
      // the per-agent `load_transcript` path).
      model?: string | null;
      effort?: string | null;
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
    }
  | {
      // A harness-recorded inter-turn event (currently only compaction), sourced
      // from one agent's session file. Agent-scoped (`agent_id`) — rendered as a
      // per-agent marker, never correlated to a send. `id` is the harness
      // `turn_id` (stable render identity, not a send key).
      kind: "system_marker";
      id: string;
      agent_id: AgentId;
      marker: SystemMarker;
      at: string;
    };

// Mirror of Rust `SystemMarker` (`crates/harness/src/transcript.rs`,
// `#[serde(tag = "marker_kind")]`). Discriminated union so a future marker kind
// lands additively; reducers default-branch on an unknown `marker_kind`.
export type SystemMarker = { marker_kind: "compaction"; summary: string };

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

// Mirror of Rust `SessionFingerprint` / `AgentSessionFingerprint`
// (`crates/app/src/commands.rs`). The staleness-refresh gate: a cheap per-agent
// stat (no parse) the frontend diffs against the value stored at last hydration
// to decide whether to re-read a session file the user may have continued in the
// harness's own TUI.
export type SessionFingerprint = {
  source_path: string;
  // ISO-8601 instant of the file's last modification.
  modified_at: string;
  byte_len: number;
};

export type AgentSessionFingerprint = {
  agent_id: AgentId;
  // Whether this agent's harness may be refreshed at all (the live-matched
  // capability). The frontend only acts on a changed fingerprint when true.
  refresh_capable: boolean;
  // Absent when refresh is unsupported (not statted) or no session file exists.
  fingerprint?: SessionFingerprint | null;
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

// ── Workflows (system-design §7) ──────────────────────────────────────────────
// Mirror the Rust types in `crates/app/src/workflow_commands.rs`.

// One declared workflow input as the invocation form renders it. `ty` is the
// base type; `text?` is `ty: "text"` with `optional: true`. List inputs
// (`[agent]`/`[text]`) are `agent_list`/`text_list`. There is no prompt type — a
// step's prompt is hardcoded; its arguments are auto-derived (see DerivedArgInfo).
export type WorkflowInputType = "agent" | "agent_list" | "text" | "text_list";

export type WorkflowInputInfo = {
  name: string;
  ty: WorkflowInputType;
  optional: boolean;
  description: string | null;
};

// A workflow as the menu/list shows it: parsed metadata OR a parse error, plus
// the read-only/built-in flag and the up-front `invocable` flag (false when it
// uses a not-yet-runnable step — `pause_for_user`/`for_each`).
export type WorkflowListing = {
  name: string;
  is_builtin: boolean;
  description: string | null;
  inputs: WorkflowInputInfo[];
  invocable: boolean;
  parse_error: string | null;
};

// A user-fillable prompt argument auto-derived from a workflow's hardcoded
// prompt(s) — surfaced as a form field alongside the declared inputs. `prompts`
// lists the hardcoded prompt id(s) it feeds (more than one when two prompts share
// a same-named argument).
export type DerivedArgInfo = {
  name: string;
  required: boolean;
  description: string | null;
  prompts: string[];
};

// A binding/collision problem that blocks invocation (drift between a workflow
// and its hardcoded prompt). `argument` is empty when the whole prompt id is the
// problem (malformed).
export type BindingIssue = {
  prompt: string;
  argument: string;
  reason: string;
};

// Whether a picked workflow's hardcoded prompts are runnable as-is.
//  - `ok`: every prompt resolved, every binding valid.
//  - `incompatible`: a prompt drifted (invalid binding / malformed id / disallowed
//    collision) — blocks Run with the listed issues.
//  - `unresolved`: a prompt isn't resolvable yet (cold MCP cache) — pending, not an
//    error; the form shows a "resolving" affordance and re-fetches on `prompts:synced`.
export type FormCompatibility =
  | { state: "ok" }
  | { state: "incompatible"; issues: BindingIssue[] }
  | { state: "unresolved"; prompts: string[] };

// The complete invocation form for a picked workflow: declared inputs plus the
// auto-derived user-fillable prompt-argument fields, plus a compatibility verdict.
// Resolved per-pick via `describe_workflow_form` (not in `list_workflows`).
// A declared recipient reference for a step. `literal` is a hardcoded agent name;
// `slot` is an `agent`/`[agent]` input the user binds — the composer preview
// resolves a slot against the form's bindings live, and a live run carries
// recipients already resolved to `literal`s.
export type RecipientRef = { kind: "literal"; name: string } | { kind: "slot"; input: string };

// What a step is, so the progress view can group a `send` with the `wait` that
// synchronizes it. Both wait variants are `"wait"`. The reducer's default branch
// degrades gracefully on a kind a newer build added but this one doesn't know
// (Rust `#[non_exhaustive]`) — such a step renders as its own honest row.
export type WorkflowStepKind = "send" | "wait" | "pause" | "for_each";

// The prompt a `send` step runs, surfaced as a "which prompt" chip: a named
// prompt (`builtin:code-review`) or inline text. `null` on a step that runs no
// prompt (a wait/pause, or a pure-forward send).
export type StepPrompt = { kind: "named"; id: string } | { kind: "inline" };

// One step as the progress/preview views render it. `recipients` are declared
// (slots unresolved) on a `WorkflowFormDescriptor`, and resolved to concrete
// agent names on a live `WorkflowRunInfo`.
export type WorkflowStepInfo = {
  kind: WorkflowStepKind;
  label: string;
  // One-line explanation, rendered as a sub-line under the label.
  description: string | null;
  // The prompt this step runs, shown as a chip; `null` when the step runs none.
  prompt: StepPrompt | null;
  recipients: RecipientRef[];
  feeds_from: RecipientRef[];
};

export type WorkflowFormDescriptor = {
  name: string;
  description: string | null;
  is_builtin: boolean;
  invocable: boolean;
  inputs: WorkflowInputInfo[];
  derived_args: DerivedArgInfo[];
  compatibility: FormCompatibility;
  // Declared steps for the composer preview (slots unresolved).
  steps: WorkflowStepInfo[];
};

// A run as the indicator shows it (from `list_workflow_runs`). `status` is
// `running` (live), `failed` (retained terminal), or `interrupted` (no terminal,
// not live). `step` is the zero-based current/failing step.
export type WorkflowRunStatus = "running" | "failed" | "interrupted";

export type WorkflowRunInfo = {
  run_id: string;
  workflow: string;
  step: number;
  total: number;
  status: WorkflowRunStatus;
  reason: string | null;
  // Per-step display info: resolved recipients for a live run, declared for a
  // disk-sourced failed/interrupted run. May be empty for a legacy run file.
  steps: WorkflowStepInfo[];
};

// The `workflow:<project-id>` channel payload. `status` is `running` while live,
// or a terminal (`complete`/`cancelled`/`failed`). Carries no agent output text.
export type WorkflowProgressStatus = "running" | "complete" | "cancelled" | "failed";

export type WorkflowProgressPayload = {
  run_id: string;
  workflow: string;
  step: number;
  total: number;
  status: WorkflowProgressStatus;
  reason: string | null;
};

// An input value supplied at invocation. A scalar (agent/prompt_id/text) is a
// bare string; a list (`[agent]`/`[text]`) is a string array — matching the
// untagged Rust `InputValue`.
export type WorkflowInputValue = string | string[];
