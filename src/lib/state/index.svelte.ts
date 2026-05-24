// App-level state for the unified-stream model.
//
// **Why this module exists.** Per-agent state outlives any particular UI
// component: subscriptions persist for the lifetime of the app session,
// regardless of which agent the user is "looking at" (per AGENTS.md: no
// singleton "active" or "focused" agent). The state therefore lives one
// layer above any component, in this module.
//
// **Lifetime contract** (per system-design §3):
// - Subscriptions register at agent creation/load time (`registerAgent`).
// - They are NEVER torn down. `set_active_project` is display-only and
//   does not unregister listeners (background events for other projects
//   continue flowing into state).
// - On app close: process exit; no explicit cleanup needed.
//
// **Per-agent isolation.** Each event arrives on `agent:<agent_id>`. The
// listener registered for that channel is the only one that sees those
// events; routing is by channel, not by any in-payload `agent_id`
// matching. This makes cross-agent contamination structurally impossible
// (a regression would require the wrong channel name).
//
// **Wall-clock boundary.** This module is the **only place** that mints
// receive-time timestamps for tool events. The pure reducers
// (`transcriptReducer` / `runtimeReducer`) accept a `receivedAt` parameter;
// this module computes `new Date().toISOString()` once per event at the
// listener boundary and threads it through. Tests can drive the reducers
// with fixed timestamps for deterministic assertions.
//
// **UI integration**: App.svelte calls `registerAgent` at project-open
// time and on dynamic agent add. Sidebar / UnifiedTranscript / ComposeBar
// components read `transcripts` / `runtimes` / `ui` directly. ComposeBar
// drives `dispatchUserTurn` + Tauri `send_message` + `failSendStart` on
// IPC error.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { loadTranscript } from "$lib/api";
import type {
  AgentId,
  AgentRecord,
  FailureKind,
  Hydrate,
  MessageId,
  NormalizedEvent,
  TurnId,
} from "$lib/types";
import { HEARTBEAT_TIMEOUT_MS } from "$lib/types";
import { _internal, freshRuntime, runtimeReducer, transcriptReducer } from "./reducers";
import type { AgentRuntime, RuntimeMap, ToolCall, TranscriptMap, Turn } from "./types";

/// Per-agent turn lists, keyed by `agent_id`. The unified-view renderer
/// merges across all agents at render time:
/// `activeProject.agents.flatMap(id => transcripts[id]).sort_by(started_at)`.
export const transcripts = $state<TranscriptMap>({});

/// Per-agent operational state, keyed by `agent_id`. Powers the sidebar
/// (run_status, last_error, meta, last_rate_limit, hydration_status) and
/// the compose-bar Send gate.
export const runtimes = $state<RuntimeMap>({});

/// Session-local ergonomic — the recipient picker preselects whichever
/// agent the user last sent to. Not persisted across app reloads
/// (ergonomic, not a semantic privilege).
export const ui = $state<{ lastRecipientId: AgentId | null }>({ lastRecipientId: null });

/// Per-agent unlisten functions for the Tauri event channel. Keyed by
/// `agent_id`. We hold these so the test harness can drain them via
/// `_testing.reset()`; production callers never unregister.
//
// Plain `Map` (not `SvelteMap`) is deliberate — this registry is not
// reactive state. Components don't observe it; it just holds non-state
// resources (channel handles).
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const listenerRegistry = new Map<AgentId, UnlistenFn>();

/// In-flight `registerAgent` promises, keyed by `agent_id`. Without this
/// map, two overlapping `registerAgent` calls for the same agent both
/// pass the `listenerRegistry.has` check before either reaches the post-
/// `await listen` write — double-registering the listener and corrupting
/// idempotency. Each call stores its promise here before the first await;
/// subsequent overlapping calls return the same promise.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const pendingRegistrations = new Map<AgentId, Promise<void>>();

/// Per-agent heartbeat tracking. Keyed by `agent_id`. The `turn_id` field
/// is the turn the timer is currently watching — re-arms on per-turn
/// activity for that turn; clears on terminal events.
//
// Plain `Map` (not `SvelteMap`) for the same reason as `listenerRegistry`
// — internal bookkeeping, not reactive state.
type Heartbeat = { turn_id: TurnId; handle: ReturnType<typeof setTimeout> };
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const heartbeats = new Map<AgentId, Heartbeat>();

/// Agents this session has already attempted to hydrate. Once an agent is
/// in this set, subsequent `hydrateAgent` calls are no-ops — regardless of
/// whether the prior attempt succeeded, failed, or is in flight. The set
/// stays sticky across success AND failure for the duration of the session.
///
/// **Why sticky across failure**: parsers mint fresh `turn_id`s per-turn at
/// parse time. If hydration ran twice for the same session file, the second
/// call's turns would have different ids than the first's, and the reducer's
/// `existingIds.has(t.turn_id)` dedupe in the `hydrate` arm would NOT catch
/// the duplication — the same conversation lands twice. Even on the failure
/// branch, the safer default is "don't retry implicitly" rather than risk
/// the duplicate-content case. An explicit retry UX (per-agent retry button)
/// is future work; it would mutate this set out-of-band.
///
/// **TODO**: clear this set when the bound directory rebinds — the agents
/// in a different directory are a different population. Out of scope here
/// (no directory-rebind path exists yet that wouldn't already reset the
/// whole app state).
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const hydrationAttempted = new Set<AgentId>();

/// Hydrate an agent's transcript history from its harness session file.
///
/// Drives the `hydration_status` ladder: `pending` → `loading` → `complete`
/// or `failed`. The hydrate reducer input is per-agent and non-destructive:
/// live in-flight turns and already-populated runtime metadata are
/// preserved (live > disk).
///
/// **Idempotency**. Tracked via `hydrationAttempted` (a module-scope Set),
/// not via inspecting `hydration_status`. Earlier versions short-circuited
/// on `complete`/`failed` — but that left the door open for a caller (e.g.,
/// project-reopen) to forcibly reset `hydration_status: "pending"` and
/// silently re-trigger hydration, producing duplicate turns since parsers
/// mint fresh `turn_id`s at parse time. The set is authoritative; the
/// status field is presentational.
///
/// **Failure scope**: only lookup-mechanism failures (IPC reject) land
/// in `hydration_status: "failed"`. Per-line parse warnings flow through
/// as `LoadedTranscript.warnings` inside an otherwise-`complete` result.
export async function hydrateAgent(agentId: AgentId): Promise<void> {
  const current = runtimes[agentId];
  if (current === undefined) {
    console.error("[switchboard] hydrateAgent called for unregistered agent", {
      agent_id: agentId,
    });
    return;
  }
  if (hydrationAttempted.has(agentId)) return;
  hydrationAttempted.add(agentId);
  runtimes[agentId] = { ...current, hydration_status: "loading" };
  try {
    const loaded = await loadTranscript(agentId);
    const hydrate: Hydrate = {
      type: "hydrate",
      agent_id: agentId,
      turns: loaded.turns,
      meta: loaded.meta ?? null,
      last_rate_limit: loaded.last_rate_limit ?? null,
      warnings: loaded.warnings,
    };
    const priorTurns = transcripts[agentId] ?? [];
    transcripts[agentId] = transcriptReducer(priorTurns, hydrate, agentId, "");
    const priorRuntime = runtimes[agentId];
    if (priorRuntime !== undefined) {
      runtimes[agentId] = runtimeReducer(priorRuntime, hydrate);
    }
  } catch (e) {
    console.warn("[switchboard] hydrateAgent failed", { agent_id: agentId, error: e });
    const after = runtimes[agentId];
    if (after !== undefined) {
      runtimes[agentId] = { ...after, hydration_status: "failed" };
    }
  }
}

/// Initialize state for an agent and subscribe to its event channel.
///
/// Idempotent under both **sequential** and **concurrent** calls:
/// - Sequential second call → `listenerRegistry.has` short-circuits.
/// - Concurrent second call (overlapping awaits) → `pendingRegistrations`
///   short-circuits, returning the in-flight promise. Without this guard,
///   two concurrent calls would both pass the `has` check, both await
///   `listen`, and both register — duplicating the channel subscription.
///
/// Idempotency is load-bearing because the project-open path and the
/// dynamic-add path (create_agent/attach_agent success) both call this,
/// and a freshly-created agent that's also in `list_agents()` would
/// otherwise double-register.
export async function registerAgent(agent: AgentRecord): Promise<void> {
  if (listenerRegistry.has(agent.id)) return;
  const pending = pendingRegistrations.get(agent.id);
  if (pending !== undefined) return pending;

  const promise = (async () => {
    try {
      // Initialize the runtime entry before subscribing — guarantees that
      // the first event arriving on the channel finds a runtime to
      // mutate. Without this ordering, an early event could land before
      // the state object had the agent's key, and the reducer's
      // `...runtime` spread would crash.
      if (!(agent.id in runtimes)) {
        runtimes[agent.id] = freshRuntime(agent.id);
      }
      if (!(agent.id in transcripts)) {
        transcripts[agent.id] = [];
      }

      const channel = `agent:${agent.id}`;
      const unlisten = await listen<NormalizedEvent>(channel, (event) => {
        handleEvent(agent.id, event.payload);
      });
      listenerRegistry.set(agent.id, unlisten);
    } finally {
      pendingRegistrations.delete(agent.id);
    }
  })();
  pendingRegistrations.set(agent.id, promise);
  return promise;
}

/// Synchronously append a user-role turn AND transition the agent's
/// `run_status` to `"starting"`. Called by the compose-bar's Send handler
/// at submit time, before the IPC reply arrives. The user's message is
/// part of the conversation, not transient UI state — appending here
/// means it survives reload (via session-file hydration on next project
/// open) and renders immediately without waiting for the backend
/// round-trip. The `"starting"` state closes the pre-`TurnStart`
/// sendability race (see `AgentRuntime` docstring for the full state
/// machine).
///
/// **Defensive invariants** (compose-bar should gate first; these are
/// fail-loud defense-in-depth):
/// - Runtime must exist (agent registered via `registerAgent`).
/// - `run_status` must be `"idle"`. A second click during `"starting"` /
///   `"processing"` is rejected here so no phantom user turn is appended
///   for a dispatch that won't happen, and `lastRecipientId` stays put.
///
/// Both violations log via `console.error` and no-op (the alternative —
/// silently appending a turn for a dispatch we won't fire — would corrupt
/// the transcript).
///
/// This is the **single production path** for adding a user turn. The
/// underlying pure helper lives at `reducers.ts::_internal.appendUserTurn`.
export function dispatchUserTurn(
  agentId: AgentId,
  userTurnId: TurnId,
  text: string,
  // Timestamp generation, not reactive state.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  startedAt: string = new Date().toISOString(),
): void {
  const runtime = runtimes[agentId];
  if (runtime === undefined) {
    console.error("[switchboard] dispatchUserTurn called for unregistered agent", {
      agent_id: agentId,
    });
    return;
  }
  if (runtime.run_status !== "idle") {
    console.error("[switchboard] dispatchUserTurn called while agent not idle", {
      agent_id: agentId,
      run_status: runtime.run_status,
    });
    return;
  }
  const existing = transcripts[agentId] ?? [];
  transcripts[agentId] = _internal.appendUserTurn(existing, agentId, userTurnId, text, startedAt);
  runtimes[agentId] = {
    ...runtime,
    run_status: "starting",
    last_error: undefined,
    // The send hasn't been accepted yet — `recordSendAccepted` fills the
    // `message_id` once `send_message` resolves. Cleared here so a stale
    // receipt from a prior send can't linger into this dispatch.
    pending_message_id: undefined,
  };
  ui.lastRecipientId = agentId;
}

/// Record the accepted-send receipt (`message_id`) for the agent's in-flight
/// `"starting"` dispatch. Called by the compose-bar after `send_message`
/// resolves. The recorded `message_id` is what correlates the eventual
/// `turn_start` / `message_failed` event back to this optimistic dispatch
/// (see `AgentRuntime.run_status` docstring for the full state machine).
///
/// **Guarded**: only writes while `"starting"`. If `turn_start` raced the
/// IPC reply (the agent is already `"processing"`), the receipt is moot —
/// the turn correlated itself already, and stamping a stale `message_id`
/// would be incorrect.
export function recordSendAccepted(agentId: AgentId, messageId: MessageId): void {
  const runtime = runtimes[agentId];
  if (runtime === undefined) {
    console.error("[switchboard] recordSendAccepted called for unregistered agent", {
      agent_id: agentId,
    });
    return;
  }
  if (runtime.run_status !== "starting") return;
  runtimes[agentId] = { ...runtime, pending_message_id: messageId };
}

/// Mark a send-start failure: the compose-bar called `dispatchUserTurn`,
/// then invoked the `send_message` Tauri IPC which rejected before the
/// backend's `TurnStart` could arrive. Restores `run_status` to `"idle"`
/// (re-enabling Send for retry) and surfaces `last_error` for the sidebar.
///
/// **Guarded transition**: only flips `"starting" → "idle"`. If
/// `TurnStart` raced ahead and the agent is already `"processing"`, this
/// is a no-op — the backend is genuinely processing the turn and the
/// frontend's "send failed" intuition was wrong. The optimistic user
/// turn stays in the transcript regardless (the user did submit it; the
/// failure is a separate surface).
export function failSendStart(
  agentId: AgentId,
  error?: { message: string; kind: FailureKind },
): void {
  const runtime = runtimes[agentId];
  if (runtime === undefined) {
    console.error("[switchboard] failSendStart called for unregistered agent", {
      agent_id: agentId,
    });
    return;
  }
  if (runtime.run_status !== "starting") {
    // No-op — TurnStart raced ahead, or the agent is otherwise not in
    // the starting state. The frontend's "send failed" intuition was
    // wrong; the backend is actually processing.
    return;
  }
  runtimes[agentId] = {
    ...runtime,
    run_status: "idle",
    last_error: error,
    pending_message_id: undefined,
  };
}

// --- internal ---

function handleEvent(agentId: AgentId, event: NormalizedEvent): void {
  // Check runtime BEFORE applying any reducer. If runtime is missing,
  // applying transcriptReducer first would mutate transcripts while the
  // runtime stays stale — the user would see content streaming in but
  // run_status would never flip to "processing" and the compose bar
  // would stay enabled mid-turn. Fail-loud here so the regression is
  // visible in devtools / production logs, rather than producing
  // silently inconsistent UI state.
  const priorRuntime = runtimes[agentId];
  if (priorRuntime === undefined) {
    console.error("[switchboard] invariant violation: event arrived for unregistered agent", {
      agent_id: agentId,
      event_type: event.type,
    });
    return;
  }

  // Mint the receive-time timestamp once per event at this listener
  // boundary — the only legitimate `new Date()` call for tool-event
  // timestamps. Threaded to the reducer as `receivedAt`; reducers
  // themselves stay pure and deterministic.
  const receivedAt = new Date().toISOString();

  const priorTurns = transcripts[agentId] ?? [];
  transcripts[agentId] = transcriptReducer(priorTurns, event, agentId, receivedAt);
  runtimes[agentId] = runtimeReducer(priorRuntime, event);
  manageHeartbeat(agentId, event);
}

function manageHeartbeat(agentId: AgentId, event: NormalizedEvent): void {
  switch (event.type) {
    case "turn_start":
      armHeartbeat(agentId, event.turn_id);
      return;

    case "content_chunk":
    case "tool_started":
    case "tool_completed": {
      // Re-arm on any per-turn activity for the turn the heartbeat is
      // watching. A long shell tool call legitimately produces zero
      // content_chunks for minutes (`Bash` running a test suite); without
      // tool-event re-arming, the heartbeat would falsely fail those
      // turns. Stale events for unrelated turns are ignored.
      const heartbeat = heartbeats.get(agentId);
      if (heartbeat?.turn_id === event.turn_id) {
        armHeartbeat(agentId, event.turn_id);
      }
      return;
    }

    case "turn_end": {
      const heartbeat = heartbeats.get(agentId);
      if (heartbeat?.turn_id === event.turn_id) {
        clearHeartbeat(agentId);
      }
      return;
    }

    // Agent-scoped events (rate_limit_event, session_meta, agent_idle) and
    // unknown future variants do NOT re-arm — they're not turn-anchored
    // and can flow at any time without indicating turn progress.
    default:
      return;
  }
}

function armHeartbeat(agentId: AgentId, turnId: TurnId): void {
  clearHeartbeat(agentId);
  const handle = setTimeout(() => {
    // Deliberate: do NOT call `manageHeartbeat()` here even though we
    // feed events to the same reducers below. The heartbeat is cleared
    // by `heartbeats.delete(agentId)` on the next line; re-arming via
    // manageHeartbeat would leak a timer (the synthetic event isn't a
    // terminal one from manageHeartbeat's perspective). Symmetric-looking
    // refactors with `handleEvent` are wrong at this site.
    heartbeats.delete(agentId);
    const at = new Date().toISOString();
    const synthetic = { type: "heartbeat_timeout" as const, turn_id: turnId, at };
    const priorTurns = transcripts[agentId] ?? [];
    // `at` doubles as `receivedAt` here — the synthetic event is itself
    // the "received" event from the reducer's perspective. Tool-event
    // timestamps aren't relevant for a heartbeat_timeout (no tool item
    // is being created), but the parameter is required by the signature.
    transcripts[agentId] = transcriptReducer(priorTurns, synthetic, agentId, at);
    const priorRuntime = runtimes[agentId];
    if (priorRuntime !== undefined) {
      runtimes[agentId] = runtimeReducer(priorRuntime, synthetic);
    }
  }, HEARTBEAT_TIMEOUT_MS);
  heartbeats.set(agentId, { turn_id: turnId, handle });
}

function clearHeartbeat(agentId: AgentId): void {
  const existing = heartbeats.get(agentId);
  if (existing !== undefined) {
    clearTimeout(existing.handle);
    heartbeats.delete(agentId);
  }
}

/// Test-only API surface. Production never calls these; the state-rune
/// module is a singleton, so tests reset between runs to avoid bleed.
/// Hidden behind the `_testing` namespace so a production caller
/// grepping for "reset" won't autocomplete into clearing all app state.
export const _testing = {
  reset(): void {
    for (const unlisten of listenerRegistry.values()) {
      unlisten();
    }
    listenerRegistry.clear();
    pendingRegistrations.clear();
    hydrationAttempted.clear();
    for (const heartbeat of heartbeats.values()) {
      clearTimeout(heartbeat.handle);
    }
    heartbeats.clear();
    for (const key of Object.keys(transcripts)) {
      delete transcripts[key];
    }
    for (const key of Object.keys(runtimes)) {
      delete runtimes[key];
    }
    ui.lastRecipientId = null;
  },
  hasListener(agentId: AgentId): boolean {
    return listenerRegistry.has(agentId);
  },
  hasHeartbeat(agentId: AgentId): boolean {
    return heartbeats.has(agentId);
  },
  heartbeatTurnId(agentId: AgentId): TurnId | undefined {
    return heartbeats.get(agentId)?.turn_id;
  },
};

// Re-export the state-shape types so consumers can import everything from
// one path without reaching into `./types` directly.
export type { AgentRuntime, RuntimeMap, ToolCall, TranscriptMap, Turn };
