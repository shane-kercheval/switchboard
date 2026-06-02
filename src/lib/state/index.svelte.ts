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
// components read `transcripts` / `runtimes` directly. ComposeBar
// drives `dispatchUserTurn` + Tauri `send_message` + `failSendStart` on
// IPC error.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  cancelAgent as apiCancelAgent,
  cancelSend as apiCancelSend,
  cancelTurn as apiCancelTurn,
  loadTranscript,
} from "$lib/api";
import type {
  AgentId,
  AgentRecord,
  FailureKind,
  Hydrate,
  MessageId,
  NormalizedEvent,
  SendId,
  TurnId,
} from "$lib/types";
import { HEARTBEAT_TIMEOUT_MS } from "$lib/types";
import { _internal, freshRuntime, runtimeReducer, transcriptReducer } from "./reducers";
import type { AgentRuntime, PendingSend, RuntimeMap, ToolCall, TranscriptMap, Turn } from "./types";

/// Per-agent turn lists, keyed by `agent_id`. The unified-view renderer
/// merges across all agents at render time:
/// `activeProject.agents.flatMap(id => transcripts[id]).sort_by(started_at)`.
export const transcripts = $state<TranscriptMap>({});

/// Per-agent operational state, keyed by `agent_id`. Powers the sidebar
/// (run_status, last_error, meta, last_rate_limit, hydration_status) and
/// the compose-bar Send gate.
export const runtimes = $state<RuntimeMap>({});

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
// `handle` is `undefined` after the timer has fired: the entry is retained
// (so the next activity event can re-arm and clear `quiet_since`) but no live
// timer remains. clearTimeout(undefined) is a no-op.
type Heartbeat = { turn_id: TurnId; handle: ReturnType<typeof setTimeout> | undefined };
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
    applyAgentHydrate(agentId, loaded);
  } catch (e) {
    console.warn("[switchboard] hydrateAgent failed", { agent_id: agentId, error: e });
    const after = runtimes[agentId];
    if (after !== undefined) {
      runtimes[agentId] = { ...after, hydration_status: "failed" };
    }
  }
}

/// Apply a resolved hydration payload to an agent's transcript + runtime via
/// the non-destructive `hydrate` reducer path (live in-flight turns win over
/// disk; the runtime reducer flips `hydration_status` to `"complete"` and fills
/// meta/rate-limit only where absent). Shared by the per-agent `hydrateAgent`
/// path and the project-scoped hydration in the workspace store, which feeds
/// agent-turn content regrouped from `load_project_conversation`. The caller
/// owns idempotency (the per-agent `hydrationAttempted` set / the per-project
/// hydration guard) — this helper only applies.
export function applyAgentHydrate(
  agentId: AgentId,
  loaded: {
    turns: Hydrate["turns"];
    meta?: Hydrate["meta"];
    last_rate_limit?: Hydrate["last_rate_limit"];
    last_rate_limit_as_of?: Hydrate["last_rate_limit_as_of"];
    warnings?: Hydrate["warnings"];
  },
): void {
  const hydrate: Hydrate = {
    type: "hydrate",
    agent_id: agentId,
    turns: loaded.turns,
    meta: loaded.meta ?? null,
    last_rate_limit: loaded.last_rate_limit ?? null,
    last_rate_limit_as_of: loaded.last_rate_limit_as_of ?? null,
    warnings: loaded.warnings,
  };
  const priorTurns = transcripts[agentId] ?? [];
  transcripts[agentId] = transcriptReducer(priorTurns, hydrate, agentId, "");
  const priorRuntime = runtimes[agentId];
  if (priorRuntime !== undefined) {
    runtimes[agentId] = runtimeReducer(priorRuntime, hydrate);
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
///   for a dispatch that won't happen.
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
  sendId: SendId,
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
  const existing = transcripts[agentId] ?? [];
  transcripts[agentId] = _internal.appendUserTurn(
    existing,
    agentId,
    userTurnId,
    text,
    startedAt,
    sendId,
  );
  // Append a pending-send entry regardless of whether the agent is idle or
  // busy — send-while-busy is un-gated (the backend queues), so a second send
  // just lines up behind the running turn. The entry (keyed by user_turn_id,
  // receipt filled later) is what stamps each response's `send_id` and lets a
  // failure prune the right send.
  const pending = [...(runtime.pending_sends ?? []), { send_id: sendId, user_turn_id: userTurnId }];
  // Only an idle agent transitions to "starting" (the run_status machine
  // governs the single running turn); a send to a busy agent leaves its
  // run_status alone — its turn waits in the backend queue and surfaces when
  // its `turn_start` arrives.
  runtimes[agentId] =
    runtime.run_status === "idle"
      ? { ...runtime, run_status: "starting", last_error: undefined, pending_sends: pending }
      : { ...runtime, pending_sends: pending };
}

/// Record the accepted-send receipt (`message_id`) onto this send's pending
/// entry (matched by `user_turn_id`). Called by the compose-bar after
/// `send_message` resolves; the receipt lets the correlated `turn_start` /
/// `message_failed` event find the right entry. A no-op if the entry is already
/// gone (its `turn_start` / failure raced the IPC reply and consumed it).
export function recordSendAccepted(
  agentId: AgentId,
  userTurnId: TurnId,
  messageId: MessageId,
): void {
  const runtime = runtimes[agentId];
  if (runtime === undefined) {
    console.error("[switchboard] recordSendAccepted called for unregistered agent", {
      agent_id: agentId,
    });
    return;
  }
  const pending = runtime.pending_sends;
  if (pending === undefined) return;
  const idx = pending.findIndex((p) => p.user_turn_id === userTurnId);
  const entry = idx >= 0 ? pending[idx] : undefined;
  if (entry === undefined) return;
  // Record the receipt either way (the entry must carry `message_id` so a later
  // `message_cancelled` / `turn_start` can match it).
  const next = [...pending];
  next[idx] = { ...entry, message_id: messageId };
  runtimes[agentId] = { ...runtime, pending_sends: next };
  if (entry.cancel_requested) {
    // The user cancelled before the backend accepted this send; now that it's
    // accepted, fire the deferred send-scoped cancel. The backend reports the
    // outcome (a `message_cancelled` event if still queued, a `Cancelled`
    // terminal if it had started) — no optimistic synthesis here.
    void apiCancelSend(entry.send_id, [agentId]);
  }
}

/// Mark a send-start failure: the compose-bar called `dispatchUserTurn`, then
/// invoked the `send_message` Tauri IPC which rejected before the backend's
/// `TurnStart` arrived. Prunes this send's pending entry (by `user_turn_id`,
/// wherever it sits — a queued send's entry is not at the front) and surfaces
/// `last_error`. Flips `run_status` back to `"idle"` only if this was the
/// *starting* send; a queued send failing while another turn is `processing`
/// must not stomp the live turn. The optimistic user turn stays in the
/// transcript (the user did submit it) and a failed agent turn is appended
/// beneath it, so the failure surfaces in the transcript rather than only in
/// runtime `last_error`.
export function failSendStart(
  agentId: AgentId,
  userTurnId: TurnId,
  error?: { message: string; kind: FailureKind },
): void {
  const runtime = runtimes[agentId];
  if (runtime === undefined) {
    console.error("[switchboard] failSendStart called for unregistered agent", {
      agent_id: agentId,
    });
    return;
  }
  const pending = runtime.pending_sends;
  const idx = pending?.findIndex((p) => p.user_turn_id === userTurnId) ?? -1;
  // Entry already gone ⇒ TurnStart raced ahead and consumed it; the backend is
  // genuinely processing this send. No-op (don't stomp the live turn).
  if (pending === undefined || idx < 0) return;
  const entry = pending[idx];
  const remaining = [...pending.slice(0, idx), ...pending.slice(idx + 1)];
  const pending_sends = remaining.length === 0 ? undefined : remaining;
  runtimes[agentId] =
    runtime.run_status === "starting"
      ? { ...runtime, run_status: "idle", last_error: error, pending_sends }
      : { ...runtime, last_error: error, pending_sends };
  // Surface the failure in the transcript (the same place post-start failures
  // and the post-reload journal marker render it) rather than only in runtime
  // state. The optimistic user turn already sits above it; this adds the failed
  // response beneath. Keyed on `user_turn_id` (the IPC-reject path has no
  // backend `message_id`), so it can't collide with a `message_failed` event's
  // `failed-${message_id}` row.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const at = new Date().toISOString();
  transcripts[agentId] = _internal.appendFailedTurn(
    transcripts[agentId] ?? [],
    agentId,
    `failed-${userTurnId}`,
    at,
    error?.message ?? "send failed before the turn started",
    entry?.send_id,
  );
}

/// Cancel a whole send across `agentIds` (the group cancel-send control, or a
/// single-element list for one recipient's Cancel). Fire-and-forget: the
/// backend cancels each recipient's in-flight turn (→ a `Cancelled` `turn_end`)
/// or drops its still-queued item (→ a `message_cancelled` event); the
/// transcript renders cancellation from whichever event arrives. No optimistic
/// synthesis — that guessing is what created the start-race.
///
/// The one exception is a recipient whose entry isn't backend-accepted yet (no
/// `message_id`): firing now races the in-flight `send_message` IPC and could
/// miss, so we defer (`cancel_requested`) and `recordSendAccepted` fires it once
/// the send is confirmed.
export function cancelSend(sendId: SendId, agentIds: AgentId[]): void {
  const fireNow: AgentId[] = [];
  for (const agentId of agentIds) {
    const rt = runtimes[agentId];
    if (rt === undefined) continue;
    const pending = rt.pending_sends;
    const entry = pending?.find((p) => p.send_id === sendId);
    if (entry !== undefined && entry.message_id === undefined) {
      runtimes[agentId] = {
        ...rt,
        pending_sends: pending!.map((p) =>
          p.send_id === sendId ? { ...p, cancel_requested: true } : p,
        ),
      };
      continue;
    }
    fireNow.push(agentId);
  }
  if (fireNow.length > 0) void apiCancelSend(sendId, fireNow);
}

export function cancelTurn(agentId: AgentId): void {
  void apiCancelTurn(agentId);
}

/// Stop an agent (sidebar "Stop agent"): cancel its in-flight turn and clear its
/// entire queued backlog. Fire-and-forget: `cancel_agent` cancels the running
/// turn (→ `Cancelled` terminal) and drops each accepted queued send (→ a
/// `message_cancelled` event per send); the transcript renders from those
/// events. A not-yet-accepted send (no `message_id`) can't be cancelled
/// backend-side yet, so it's flagged `cancel_requested` and `recordSendAccepted`
/// fires its cancel once confirmed.
export function stopAgent(agentId: AgentId): void {
  const rt = runtimes[agentId];
  const pending = rt?.pending_sends;
  if (rt !== undefined && pending !== undefined) {
    runtimes[agentId] = {
      ...rt,
      pending_sends: pending.map((p) =>
        p.message_id === undefined ? { ...p, cancel_requested: true } : p,
      ),
    };
  }
  void apiCancelAgent(agentId);
}

/// Mark an agent as already-hydrated so the per-agent `hydrateAgent` path
/// won't re-parse it. Used by the project-scoped hydration in the workspace
/// store, which hydrates roster agents through `applyAgentHydrate` directly:
/// without this, a later `hydrateAgent` call on the same agent would re-parse
/// its session file and duplicate its turns (the reducer dedups by `turn_id`,
/// but parsers mint fresh ids each parse). Keeps the "hydrate an agent at most
/// once per session" invariant holding regardless of which path runs first.
export function markHydrationAttempted(agentId: AgentId): void {
  hydrationAttempted.add(agentId);
}

/// Tear down per-agent state for the given agents: unsubscribe their event
/// channels, cancel heartbeats, and drop their transcript/runtime/guard
/// entries. Called when a directory is removed (the frontend lifecycle teardown
/// matching the backend drain) so a remove-then-re-add of the same project ids
/// — ids are persisted on disk and survive removal — starts clean rather than
/// reusing stale listeners, transcripts, or hydration guards.
export function unregisterAgents(agentIds: AgentId[]): void {
  for (const agentId of agentIds) {
    const unlisten = listenerRegistry.get(agentId);
    if (unlisten !== undefined) {
      unlisten();
      listenerRegistry.delete(agentId);
    }
    pendingRegistrations.delete(agentId);
    hydrationAttempted.delete(agentId);
    clearHeartbeat(agentId);
    delete transcripts[agentId];
    delete runtimes[agentId];
  }
}

// --- internal ---

/// The pending entry a `turn_start { message_id }` belongs to: the entry
/// matching `messageId`, else the front (covers the race where the IPC receipt
/// hasn't been recorded yet). Mirrors `reducers.ts::pickPendingIndex` so the
/// transcript stamp and the runtime removal pick the same entry.
function pendingEntryFor(runtime: AgentRuntime, messageId: MessageId): PendingSend | undefined {
  const pending = runtime.pending_sends;
  const front = pending?.[0];
  if (front === undefined) return undefined;
  const byMsg = pending?.find((p) => p.message_id === messageId);
  if (byMsg !== undefined) return byMsg;
  // Front-fallback only during the pre-receipt race (mirrors pickPendingIndex).
  return front.message_id === undefined ? front : undefined;
}

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
  // On turn_start, find the pending-send entry this turn belongs to (by
  // message_id, else the front — the backend runs turns in dispatch order) and
  // pass its send_id so the new agent turn is stamped; `runtimeReducer` removes
  // the same entry in lockstep.
  const startEntry =
    event.type === "turn_start" ? pendingEntryFor(priorRuntime, event.message_id) : undefined;
  // For a `message_cancelled` event, resolve the send_id of the dropped queued
  // entry (exact message_id match) so the reducer can render its cancelled row.
  const cancelledSendId =
    event.type === "message_cancelled"
      ? priorRuntime.pending_sends?.find((p) => p.message_id === event.message_id)?.send_id
      : undefined;
  // For a `message_failed` event, resolve the failed send via the same
  // `pendingEntryFor` lookup `turn_start` uses (and that `runtimeReducer`
  // mirrors via `pickPendingIndex`): exact message_id, else the front entry
  // during the pre-receipt race (message_failed beating the IPC receipt). This
  // keeps the transcript row and the runtime pruning on the *same* entry. A
  // post-start failure finds no entry (turn_start consumed it) → no row, so the
  // live turn still owns the outcome and there is no double-render.
  const failedSendId =
    event.type === "message_failed"
      ? pendingEntryFor(priorRuntime, event.message_id)?.send_id
      : undefined;
  const sendId = startEntry?.send_id ?? cancelledSendId ?? failedSendId;
  transcripts[agentId] = transcriptReducer(priorTurns, event, agentId, receivedAt, sendId);
  runtimes[agentId] = runtimeReducer(priorRuntime, event);
  manageHeartbeat(agentId, event);

  // Deferred cancel: if this turn started for a send the user cancelled before
  // the backend accepted it, the turn ran anyway — fire the send-scoped cancel
  // now (the live turn's `Cancelled` terminal will follow). The real turn is
  // already rendered, so there is nothing to synthesize.
  if (startEntry?.cancel_requested) {
    void apiCancelSend(startEntry.send_id, [agentId]);
  }
}

function manageHeartbeat(agentId: AgentId, event: NormalizedEvent): void {
  switch (event.type) {
    case "turn_start":
      armHeartbeat(agentId, event.turn_id);
      return;

    case "content_chunk":
    case "liveness":
    case "tool_started":
    case "tool_completed": {
      // Re-arm on any sign the harness is alive for the turn the heartbeat is
      // watching. A long shell tool call legitimately produces zero
      // content_chunks for minutes (`Bash` running a test suite), and a long
      // redacted thinking block produces only `liveness` (Claude Opus 4.8's
      // redacted thinking deltas) — without re-arming on those, the heartbeat
      // would falsely flag healthy turns as silent. `quiet_since` (if the timer
      // already fired) is
      // cleared on this same activity by `runtimeReducer`. Stale events for
      // unrelated turns are ignored. Re-arming after the timer has fired works
      // because the fire path retains the heartbeats entry (see armHeartbeat).
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
    // The turn has been silent for HEARTBEAT_TIMEOUT_MS but is still alive on
    // the backend (it holds the busy-lock). Do NOT fail it — a frontend
    // "failed" would be a lie and would not release the lock. Instead record
    // `quiet_since` via the runtime reducer so the UI surfaces the silence; the
    // real terminal (or a user cancel) resolves the turn.
    //
    // Retain the heartbeats entry (drop only the now-fired handle) so the next
    // activity event for this turn re-arms via `manageHeartbeat` and clears
    // `quiet_since`. Deleting the entry here would strand it set forever,
    // because the re-arm guard keys on an existing entry. Deliberate: do NOT
    // call `manageHeartbeat()` here — the synthetic heartbeat_timeout is not a
    // re-arm trigger, and only the runtime (not the transcript) changes.
    heartbeats.set(agentId, { turn_id: turnId, handle: undefined });
    const at = new Date().toISOString();
    const synthetic = { type: "heartbeat_timeout" as const, turn_id: turnId, at };
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
