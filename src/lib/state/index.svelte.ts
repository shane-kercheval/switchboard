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
  compactAgent as apiCompactAgent,
  contextReportAgent as apiContextReportAgent,
  loadTranscript,
} from "$lib/api";
import type {
  AgentId,
  AgentRecord,
  Attachment,
  FailureKind,
  HarnessKind,
  Hydrate,
  MessageId,
  NormalizedEvent,
  ProjectId,
  SendId,
  TurnId,
} from "$lib/types";
import { HEARTBEAT_TIMEOUT_MS } from "$lib/types";
import { _internal, freshRuntime, runtimeReducer, transcriptReducer } from "./reducers";
import {
  markRecipientStarted,
  settleAgentIdle,
  settleAgentsRemoved,
  settleRecipient,
  settleTurn,
} from "./sendCompletion";
import type { AgentRuntime, PendingSend, RuntimeMap, ToolCall, TranscriptMap, Turn } from "./types";
import {
  clearUsageRefusal,
  nameUsageModel,
  observeUsage,
  recordUsageRefusal,
  _testing as usageTesting,
} from "$lib/state/harnessUsage.svelte";

/// Per-agent turn lists, keyed by `agent_id`. The unified-view renderer
/// merges across all agents at render time:
/// `activeProject.agents.flatMap(id => transcripts[id]).sort_by(started_at)`.
export const transcripts = $state<TranscriptMap>({});

/// Monotonic revision of transcript content, bumped by `setTranscript` — the
/// "did anything change?" signal for consumers that must react to every
/// content write without walking the data (the transcript's re-anchor effect;
/// the digest walk this replaces cost O(transcript) of reactive-proxy reads
/// per streamed chunk).
///
/// SINGLE-WRITER CONTRACT (structural, like composeStore's flush() guard):
/// every write to `transcripts` that live re-anchoring must observe goes
/// through `setTranscript`. A writer that bypasses it renders fine but
/// silently stops signalling re-anchors. Production writes (this module), the
/// browser-test harness (`seedTurns`), and the dev seeding hook all route
/// through it; jsdom component tests seeding static fixtures may assign
/// directly — re-anchoring is inert without layout, so there is nothing for
/// the bump to drive there.
let transcriptRevision = $state(0);

export function getTranscriptRevision(): number {
  return transcriptRevision;
}

/// Local sends, PROJECT-SCOPED and RECIPIENT-AWARE. Sending a message is the
/// user's explicit request to see the response, so each transcript containing
/// a recipient force-pins — wherever the user had scrolled — and auto-follow
/// engages before the first chunk. Other panes must keep their reading place.
///
/// Scoped, not global: a forward dispatches from a closure that can outlive a
/// project switch (see `dispatchToRecipients`, which takes its project
/// explicitly for that reason), so an unscoped signal would force-pin
/// whichever project the user had moved on to. Published ONCE per send action
/// rather than per recipient, because a fan-out to N agents is one send in
/// this project's vocabulary — N turns, one send.
///
/// The state layer reports the domain fact; the transcript owns the decision
/// to force-pin.
/// Each recipient retains the sequence of its latest send. That preserves two
/// same-project sends coalesced into one reactive flush (held forwards can do
/// this): panes containing recipients of either send still see a sequence newer
/// than their last observation. A single latest-recipient list would lose the
/// earlier pane.
type LocalSend = {
  sendId: SendId;
  seq: number;
  recipientSeqs: Record<AgentId, number>;
};

const localSends = $state<Record<ProjectId, LocalSend>>({});

export function getLocalSend(projectId: ProjectId): LocalSend | undefined {
  return localSends[projectId];
}

/// Announce a local send action. Called once per send by the compose path,
/// before it dispatches to the recipients.
export function noteLocalSend(projectId: ProjectId, sendId: SendId, recipientIds: AgentId[]): void {
  const previous = localSends[projectId];
  const seq = (previous?.seq ?? 0) + 1;
  const recipientSeqs = { ...(previous?.recipientSeqs ?? {}) };
  for (const recipientId of recipientIds) recipientSeqs[recipientId] = seq;
  localSends[projectId] = { sendId, seq, recipientSeqs };
}

export function setTranscript(agentId: AgentId, turns: Turn[]): void {
  // The reducer returns the SAME array reference for content-free events
  // (`liveness`, `session_meta`, and every defensive no-op branch). A
  // same-reference write is not a content change, so it must not advance the
  // revision — otherwise the re-anchor effect does layout work for a heartbeat
  // mid-stream. Reference equality is exactly the reducer's "nothing changed"
  // signal; this keeps the revision meaning "content actually changed."
  if (transcripts[agentId] === turns) return;
  transcripts[agentId] = turns;
  transcriptRevision += 1;
}

/// Per-agent operational state, keyed by `agent_id`. Powers the sidebar
/// (run_status, last_error, meta, last_rate_limit, hydration_status) and
/// the compose-bar Send gate.
export const runtimes = $state<RuntimeMap>({});

/// Display-only "working" predicate for passive activity indicators. A send
/// that has already been cancel-requested no longer counts here; the sidebar's
/// Stop gate deliberately uses a broader local predicate until the backend
/// confirms cancellation.
export function agentIsWorking(runtime: AgentRuntime | undefined): boolean {
  return (
    runtime !== undefined &&
    (runtime.run_status !== "idle" ||
      (runtime.pending_sends ?? []).some((pending) => pending.cancel_requested !== true))
  );
}

/// Called when an agent's turn reaches a terminal, with the outcome.
///
/// A registered hook rather than a direct call because the dependency runs the
/// other way: the workspace store imports this module, so this module cannot
/// import it back. The one consumer is the forked-agent inherited-history
/// refresh, which needs the *project* conversation merge (not the per-agent
/// loader) and therefore lives in the workspace store.
///
/// Driven from the existing `turn_end` boundary rather than a second `listen`
/// per agent — that would break the one-listener-per-agent invariant this
/// module documents.
type TurnTerminalHook = (
  agentId: AgentId,
  outcome: "completed" | "failed" | "cancelled",
  /// What the turn was. `"compaction"` is the signal the workspace store needs to
  /// re-read the project conversation: a completed compaction leaves a recap
  /// marker in the harness's own session file, and nothing else would ever fetch
  /// it. Absent for an ordinary response.
  kind?: "compaction",
) => void;

let turnTerminalHook: TurnTerminalHook | undefined;

export function setTurnTerminalHook(hook: TurnTerminalHook | undefined): void {
  turnTerminalHook = hook;
}

/// Called when a send fails before its turn starts (`message_failed`): the
/// adapter refused to spawn, or a queued item was dropped by a shutdown. One of
/// those failures is the working directory having vanished, a fact the project
/// registry only learns when it is re-listed, so the consumer re-reads it. Same
/// registration shape as `TurnTerminalHook`, for the same dependency reason.
type DispatchFailedHook = (agentId: AgentId) => void;

let dispatchFailedHook: DispatchFailedHook | undefined;

/// Install (or, with `undefined`, clear) the hook. Returns a disposer that
/// removes only the hook it installed, so an owner tearing down late cannot
/// unhook a successor.
export function setDispatchFailedHook(hook: DispatchFailedHook | undefined): () => void {
  dispatchFailedHook = hook;
  return () => {
    if (dispatchFailedHook === hook) dispatchFailedHook = undefined;
  };
}

/// Per-agent unlisten functions for the Tauri event channel. Keyed by
/// `agent_id`. We hold these so the test harness can drain them via
/// `_testing.reset()`; production callers never unregister.
//
// Plain `Map` (not `SvelteMap`) is deliberate — this registry is not
// reactive state. Components don't observe it; it just holds non-state
// resources (channel handles).
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const listenerRegistry = new Map<AgentId, UnlistenFn>();

/// Each registered agent's harness, so the account-scoped usage store can be
/// keyed without this module importing the project roster (which imports *this*
/// module — the cycle that lookup would create). Written at registration, dropped
/// with the agent.
//
// Plain `Map` for the same reason as `listenerRegistry` — internal bookkeeping,
// not reactive state.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const agentHarness = new Map<AgentId, HarnessKind>();

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
/// **Failure scope**: lookup failures and cases where recorded history cannot
/// be recovered land in `hydration_status: "failed"`. Per-line parse damage
/// remains a successful best-effort load; only diagnostics owned by a tool row
/// cross the UI boundary on that row.
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
    // Retain the error text on the runtime (not just `console.warn` it) so the
    // failure is surfaced where the user is looking — the transcript-region
    // banner and the sidebar line read the same `hydration_error` field. This
    // mirrors the project-batch path (`workspace.svelte.ts`), which already
    // sets `hydration_error` from the backend's per-agent `load_error`.
    const message = e instanceof Error ? e.message : String(e);
    console.warn("[switchboard] hydrateAgent failed", { agent_id: agentId, error: e });
    const after = runtimes[agentId];
    if (after !== undefined) {
      runtimes[agentId] = { ...after, hydration_status: "failed", hydration_error: message };
    }
  }
}

/// Re-attempt an agent's event subscription after [`registerAgent`] recorded a
/// `listener_error`. Idempotent and safe to call repeatedly: `registerAgent`
/// returns early once the listener is registered, and it never re-creates the
/// agent — the record was already durable before the first attempt.
export async function retryAgentSubscription(agent: AgentRecord): Promise<void> {
  await registerAgent(agent);
}

/// Re-attempt an agent's hydration after a failure. Clears the sticky
/// `hydrationAttempted` guard (so `hydrateAgent` actually re-runs) and drops the
/// prior `hydration_error` (so the UI shows the loading state, not a stale
/// failure, during the re-attempt). Safe even without the idempotent merge: a
/// failed hydration applies *nothing* (the load is all-or-nothing at the IPC
/// boundary — `loadTranscript` either returns a complete value that is then
/// applied, or throws and applies nothing), so a retry-after-failure cannot
/// duplicate turns. Shared by the transcript-region and sidebar retry
/// affordances.
export async function retryAgentHydration(agentId: AgentId): Promise<void> {
  const current = runtimes[agentId];
  if (current === undefined) {
    console.error("[switchboard] retryAgentHydration called for unregistered agent", {
      agent_id: agentId,
    });
    return;
  }
  // Re-entrancy guard: a hydration is already in flight. Without this, a second
  // retry would `hydrationAttempted.delete` the guard the first call just
  // re-added and start a *second* concurrent `load_transcript`; both would
  // resolve and both apply, and since each parse mints fresh `turn_id`s the
  // un-keyed merge can't dedup them — duplicating the agent's history.
  // `hydrateAgent` sets `"loading"` synchronously before its await, so a
  // racing retry observes it here.
  if (current.hydration_status === "loading") return;
  hydrationAttempted.delete(agentId);
  runtimes[agentId] = { ...current, hydration_error: undefined };
  await hydrateAgent(agentId);
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
  /// Exactly the reducer event's own fields, minus the two this function
  /// supplies. Derived from `Hydrate` rather than hand-listed so there is one
  /// type to keep in sync instead of two: a field added to the wire event is
  /// readable here without editing this signature.
  loaded: Omit<Hydrate, "type" | "agent_id">,
): void {
  /// `Required<Hydrate>` rather than `Hydrate`: every optional wire field must
  /// be named below or this stops compiling. The inventory's capture time was
  /// once dropped exactly here — computed by the backend, read by the reducer,
  /// and lost in this rebuild — and because the field is optional on the wire
  /// type, omitting it type-checked. Note `Pick` would not help; it preserves
  /// optionality. This catches only *construction* completeness: a field the
  /// backend never sends, or one hardcoded to `null`, still compiles, which is
  /// why the seam tests in `index.test.ts` drive each field through the mocked
  /// IPC reply rather than into the reducer directly.
  const hydrate: Required<Hydrate> = {
    type: "hydrate",
    agent_id: agentId,
    turns: loaded.turns,
    meta: loaded.meta ?? null,
    last_rate_limit: loaded.last_rate_limit ?? null,
    last_rate_limit_model: loaded.last_rate_limit_model ?? null,
    last_rate_limit_as_of: loaded.last_rate_limit_as_of ?? null,
    last_rate_limit_observed_at: loaded.last_rate_limit_observed_at ?? null,
    meta_as_of: loaded.meta_as_of ?? null,
    last_context_report: loaded.last_context_report ?? null,
    last_context_report_at: loaded.last_context_report_at ?? null,
  };
  const priorTurns = transcripts[agentId] ?? [];
  // Pass the in-flight turn_id so a refresh re-read can't supersede an
  // actively-streaming live turn (which now carries an early `hydration_key`).
  const inFlightTurnId = runtimes[agentId]?.in_flight_turn_id;
  setTranscript(
    agentId,
    transcriptReducer(priorTurns, hydrate, agentId, "", undefined, inFlightTurnId),
  );
  const priorRuntime = runtimes[agentId];
  if (priorRuntime !== undefined) {
    runtimes[agentId] = runtimeReducer(priorRuntime, hydrate);
  }
  recordRestoredUsage(agentId, hydrate);
}

/// Offer a restored reading to the account-scoped usage store.
///
/// **Ranked, not applied.** A reading recovered from disk is one candidate among
/// every agent's on any harness, so it is handed to `observeUsage` and loses to
/// anything newer already held — which is what stops reopening an older project
/// from pulling the display backwards.
///
/// Depends on the agent being registered first, which the project-open path
/// guarantees: it awaits every `registerAgent` before starting hydration. An
/// unregistered agent contributes nothing rather than guessing a harness.
function recordRestoredUsage(agentId: AgentId, hydrate: Required<Hydrate>): void {
  const harness = agentHarness.get(agentId);
  if (harness === undefined || hydrate.last_rate_limit == null) return;
  observeUsage(harness, {
    payload: hydrate.last_rate_limit,
    // The measured instant when the harness recorded one, else the snapshot's
    // capture time. A reading with neither ranks at the epoch: unknown age loses
    // to every stamped reading, which is the conservative direction — it can be
    // superseded but never supersede.
    observed_at:
      hydrate.last_rate_limit_observed_at ??
      hydrate.last_rate_limit_as_of ??
      new Date(0).toISOString(),
    model: hydrate.last_rate_limit_model ?? undefined,
  });
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
        setTranscript(agent.id, []);
      }
      // Before the listener is attached: the first event through the channel
      // already needs to know which harness account it is reporting on.
      agentHarness.set(agent.id, agent.harness);

      const channel = `agent:${agent.id}`;
      try {
        const unlisten = await listen<NormalizedEvent>(channel, (event) => {
          handleEvent(agent.id, event.payload);
        });
        listenerRegistry.set(agent.id, unlisten);
        const settled = runtimes[agent.id];
        if (settled?.listener_error !== undefined) {
          runtimes[agent.id] = { ...settled, listener_error: undefined };
        }
      } catch (e) {
        // **Subscribing is not creating.** By the time this runs the agent is
        // already durable — `create_agent` / `attach_agent` / `fork_agent` all
        // append to the registry before returning. Rejecting here would make
        // every caller treat a committed agent as a failed one: it never reaches
        // the roster, the user retries, and they end up with two agents while the
        // first surfaces out of nowhere on the next restart. Record the failure
        // on the runtime instead and resolve, so callers roster the agent that
        // exists and the UI can say what is actually wrong.
        const message = e instanceof Error ? e.message : String(e);
        console.warn("[switchboard] agent event subscription failed", {
          agent_id: agent.id,
          error: e,
        });
        const after = runtimes[agent.id];
        if (after !== undefined) {
          runtimes[agent.id] = { ...after, listener_error: message };
        }
      }
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
  attachments: Attachment[],
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
  setTranscript(
    agentId,
    _internal.appendUserTurn(existing, agentId, userTurnId, text, attachments, startedAt, sendId),
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

/// Ask `agentId` to compact its own conversation, and register the pending entry
/// that tracks it.
///
/// **The whole flow, not just the state half.** Registering the pending entry
/// *before* the IPC is what makes the pre-receipt race safe — `turn_start` can
/// arrive before `compact_agent` resolves, and it must find an entry to consume.
/// Splitting registration from dispatch across two callers would let a future one
/// get that order wrong, so there is one function and the menu item is a
/// one-liner.
///
/// The entry goes in `pending_sends` alongside queued sends for the same reason:
/// its `turn_start` must consume **its own** slot. An entry kept outside that
/// list would let the compaction's `turn_start` claim a concurrent send's entry
/// in the pre-receipt race and stamp that send's id onto the wrong turn.
///
/// No user turn is appended — a compaction is not something the user said. The
/// queued row is derived from this entry instead, which is why it carries
/// `queued_at`: with no user turn to borrow a timestamp from, that is the only
/// thing that can place it in the timeline.
export async function dispatchCompaction(
  agentId: AgentId,
  sendId: SendId,
  // Both generated, not reactive state.
  pendingTurnId: TurnId = crypto.randomUUID(),
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  queuedAt: string = new Date().toISOString(),
): Promise<void> {
  const runtime = runtimes[agentId];
  if (runtime === undefined) {
    console.error("[switchboard] dispatchCompaction called for unregistered agent", {
      agent_id: agentId,
    });
    return;
  }
  const pending = [
    ...(runtime.pending_sends ?? []),
    {
      send_id: sendId,
      user_turn_id: pendingTurnId,
      kind: "compaction" as const,
      queued_at: queuedAt,
    },
  ];
  // Same rule as a send: only an idle agent moves to "starting". A compaction
  // requested while a turn runs just queues behind it.
  runtimes[agentId] =
    runtime.run_status === "idle"
      ? { ...runtime, run_status: "starting", last_error: undefined, pending_sends: pending }
      : { ...runtime, pending_sends: pending };
  try {
    const messageId = await apiCompactAgent(agentId, sendId);
    recordSendAccepted(agentId, pendingTurnId, messageId);
  } catch (e) {
    // An IPC rejection (a refused harness, no session, an unmaterialized branch)
    // never reaches the event stream, so it routes through the same pre-start
    // failure path a rejected send uses — pruning the entry and rendering the
    // reason in the transcript.
    failSendStart(agentId, pendingTurnId, {
      message: e instanceof Error ? e.message : String(e),
      kind: "adapter_failure",
    });
  }
}

/// Ask `agentId` what is occupying its context window, and register both the
/// pending entry that correlates the turn and the request record the panel
/// renders from.
///
/// Structured exactly like [`dispatchCompaction`] — the pending entry is
/// registered *before* the IPC so the pre-receipt `turn_start` finds its own
/// slot — with one addition it does not need: a `context_report_request`.
///
/// **That record exists because a report has no transcript row.** A failed
/// compaction is legible because its row says so; a report has nothing at any
/// phase, so queued/running/failed/cancelled would all look identical (nothing
/// happening) without somewhere to put them.
///
/// **The previous report is deliberately left in place.** A refresh that fails
/// should leave the last good breakdown on screen with the failure beside it,
/// not blank the panel — the old measurement is still the best one available.
export async function dispatchContextReport(
  agentId: AgentId,
  sendId: SendId,
  // Both generated, not reactive state.
  pendingTurnId: TurnId = crypto.randomUUID(),
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  queuedAt: string = new Date().toISOString(),
): Promise<void> {
  const runtime = runtimes[agentId];
  if (runtime === undefined) {
    console.error("[switchboard] dispatchContextReport called for unregistered agent", {
      agent_id: agentId,
    });
    return;
  }
  const pending = [
    ...(runtime.pending_sends ?? []),
    {
      send_id: sendId,
      user_turn_id: pendingTurnId,
      kind: "context_report" as const,
      queued_at: queuedAt,
    },
  ];
  // One slot, replaced outright: the panel opener refuses to dispatch while a
  // request is queued or running, so the only way here is from a settled
  // request — and the new one is what the user is now waiting on.
  const next: AgentRuntime = {
    ...runtime,
    pending_sends: pending,
    context_report_request: { send_id: sendId, phase: "queued" },
  };
  // Same rule as a send: only an idle agent moves to "starting".
  runtimes[agentId] =
    runtime.run_status === "idle"
      ? { ...next, run_status: "starting", last_error: undefined }
      : next;
  try {
    const messageId = await apiContextReportAgent(agentId, sendId);
    recordSendAccepted(agentId, pendingTurnId, messageId);
    // Stamp the receipt on the request too, so a later `message_failed` — which
    // carries no `send_id` when nothing was journaled — can still find it.
    // Read fresh: `turn_start` may have advanced the phase while the IPC was in
    // flight, and that progress must not be rolled back.
    const current = runtimes[agentId];
    if (current?.context_report_request?.send_id === sendId) {
      runtimes[agentId] = {
        ...current,
        context_report_request: { ...current.context_report_request, message_id: messageId },
      };
    }
  } catch (e) {
    // An IPC rejection (a refused harness, no session, an unmaterialized branch)
    // never reaches the event stream, so it routes through the same pre-start
    // failure path a rejected send uses — which, for this kind, records the
    // reason on the request rather than in the transcript.
    failSendStart(agentId, pendingTurnId, {
      message: e instanceof Error ? e.message : String(e),
      kind: "adapter_failure",
    });
  }
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
  // A pre-dispatch IPC rejection never reaches the event stream, so the send
  // tracker has to be told here or a send that failed for every recipient would
  // silently never notify.
  settleRecipient(entry?.send_id, agentId, "failed");
  const remaining = [...pending.slice(0, idx), ...pending.slice(idx + 1)];
  const pending_sends = remaining.length === 0 ? undefined : remaining;
  // A refused context report has no row to fail in, so the refusal has to land
  // on the request record — which is the only place the panel can read it. The
  // request is matched by `send_id` because this path runs *before* any
  // `message_id` exists: the IPC that would have minted one is what rejected.
  const context_report_request =
    entry?.kind === "context_report" &&
    runtime.context_report_request?.send_id === entry.send_id &&
    error !== undefined
      ? { ...runtime.context_report_request, phase: "failed" as const, error: error.message }
      : runtime.context_report_request;
  runtimes[agentId] =
    runtime.run_status === "starting"
      ? {
          ...runtime,
          run_status: "idle",
          last_error: error,
          pending_sends,
          context_report_request,
        }
      : { ...runtime, last_error: error, pending_sends, context_report_request };
  // Surface the failure in the transcript (the same place post-start failures
  // and the post-reload journal marker render it) rather than only in runtime
  // state. The optimistic user turn already sits above it; this adds the failed
  // response beneath. Keyed on `user_turn_id` (the IPC-reject path has no
  // backend `message_id`), so it can't collide with a `message_failed` event's
  // `failed-${message_id}` row.
  // A report renders nothing at any phase — see the `pendingKind` contract on
  // `transcriptReducer`. Its failure is already on the request record above.
  if (entry?.kind === "context_report") return;
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const at = new Date().toISOString();
  setTranscript(
    agentId,
    _internal.appendFailedTurn(
      transcripts[agentId] ?? [],
      agentId,
      `failed-${userTurnId}`,
      at,
      error?.message ?? "send failed before the turn started",
      entry?.send_id,
      // Without this a refused compaction — no session yet, an unmaterialized
      // branch, an unsupported harness — renders as an empty failed *response*
      // with no prompt above it, which reads as "the CLI broke" rather than as
      // the precondition the backend actually named.
      entry?.kind,
    ),
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
  // Before the listeners go: a torn-down agent will never report an outcome, and
  // an unsettled recipient blocks its whole send — including the notification the
  // *surviving* recipients earned.
  settleAgentsRemoved(agentIds);
  for (const agentId of agentIds) {
    const unlisten = listenerRegistry.get(agentId);
    if (unlisten !== undefined) {
      unlisten();
      listenerRegistry.delete(agentId);
    }
    pendingRegistrations.delete(agentId);
    hydrationAttempted.delete(agentId);
    agentHarness.delete(agentId);
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
/// The pending entry an event consumes. Mirrors `pickPendingIndex` exactly —
/// the two must agree or the transcript and the runtime act on different entries
/// — so see that function for why identity is tried before position.
function pendingEntryFor(
  runtime: AgentRuntime,
  messageId: MessageId,
  sendId?: SendId,
): PendingSend | undefined {
  const pending = runtime.pending_sends;
  const front = pending?.[0];
  if (front === undefined) return undefined;
  const byMsg = pending?.find((p) => p.message_id === messageId);
  if (byMsg !== undefined) return byMsg;
  if (sendId !== undefined) return pending?.find((p) => p.send_id === sendId);
  // Front-fallback only during the pre-receipt race (mirrors pickPendingIndex).
  return front.message_id === undefined ? front : undefined;
}

/// The pending entry a `message_cancelled` event refers to — the exact receipt
/// match first, then the event's authoritative `send_id` when cancellation raced
/// ahead of `recordSendAccepted`. Mirrors the `runtimeReducer` arm that prunes
/// the same entry, so the transcript and the runtime never act on different ones.
function cancelledEntryFor(
  runtime: AgentRuntime,
  messageId: MessageId,
  sendId: SendId,
): PendingSend | undefined {
  const pending = runtime.pending_sends;
  return (
    pending?.find((p) => p.message_id === messageId) ?? pending?.find((p) => p.send_id === sendId)
  );
}

/// Feed the account-scoped usage store from a live event.
///
/// Separate from `runtimeReducer` because what it updates is not this agent's
/// state: a quota reading and a refusal are facts about the harness account, and
/// every agent on that harness reports the same ones. Driven from the same
/// boundary so the two cannot see different events.
///
/// `turn_end` moves the refusal verdict and `rate_limit_event` moves the reading.
/// A cancellation and an unrelated failure move neither — see
/// [`clearUsageRefusal`] for why neither counts as evidence the quota recovered.
function recordAccountUsage(agentId: AgentId, event: NormalizedEvent, receivedAt: string): void {
  const harness = agentHarness.get(agentId);
  if (harness === undefined) return;
  if (event.type === "rate_limit_event") {
    observeUsage(harness, {
      payload: event.info,
      // Arrival time, not a measured instant: a live reading is current by
      // construction, and this is what ranks it above anything restored from
      // disk.
      observed_at: receivedAt,
      model: runtimes[agentId]?.current_turn_model,
    });
  } else if (event.type === "session_meta") {
    // **Late model label.** Claude's per-model weekly window never names its own
    // model, so the model of the turn that delivered the reading is what labels
    // it — and a stream can emit the rate-limit event *before* its `init`, which
    // is the recorded order on a compaction stream. The reading then lands
    // unlabelled and this fills it once the model is known.
    //
    // Only ever fills a blank, and only from the reducer's `current_turn_model`
    // (this turn's own `init`), never from `meta.model`, which survives across
    // turns and would name the previous model. The narrow cost is that two agents
    // interleaving inside the milliseconds between one turn's reading and its
    // `init` could label a window with the other's model; that is strictly better
    // than dropping the label, which is the alternative.
    nameUsageModel(harness, runtimes[agentId]?.current_turn_model);
  } else if (event.type === "turn_end") {
    if (event.outcome.status === "completed") {
      clearUsageRefusal(harness);
    } else if (event.outcome.status === "failed" && event.outcome.kind === "usage_limit") {
      recordUsageRefusal(harness);
    }
  }
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
    event.type === "turn_start"
      ? pendingEntryFor(priorRuntime, event.message_id, event.send_id)
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
      ? // Prefer the pending entry (manual sends); fall back to the event's own
        // `send_id` for a backend-originated send (a workflow step that fails before
        // `turn_start`), which has no pending entry — so its failed marker renders
        // under the workflow's live user row. A `null` event `send_id` means the
        // send was never durably recorded → coerce to `undefined` so the reducer
        // renders no row (matching the empty reload).
        (pendingEntryFor(priorRuntime, event.message_id, event.send_id ?? undefined)?.send_id ??
        event.send_id ??
        undefined)
      : undefined;
  // Prefer the locally-tracked pending-send (frontend-originated sends correlate
  // by `message_id`); fall back to the `turn_start` event's own `send_id` for a
  // send the frontend didn't originate (e.g. a workflow dispatch), so its fan-out
  // turns still group side-by-side live without waiting for a reload's journal merge.
  const eventSendId = event.type === "turn_start" ? event.send_id : undefined;
  const cancelledSendId = event.type === "message_cancelled" ? event.send_id : undefined;
  const sendId = startEntry?.send_id ?? eventSendId ?? cancelledSendId ?? failedSendId;
  // The kind of the entry this event consumes, resolved from the *same* lookups
  // that produced `sendId` so the transcript row and the runtime pruning always
  // agree about what they are acting on. A backend-originated send (a workflow
  // step) has no entry and is therefore never a compaction.
  const pendingKind =
    event.type === "turn_start"
      ? startEntry?.kind
      : event.type === "message_failed"
        ? pendingEntryFor(priorRuntime, event.message_id, event.send_id ?? undefined)?.kind
        : event.type === "message_cancelled"
          ? cancelledEntryFor(priorRuntime, event.message_id, event.send_id)?.kind
          : undefined;
  if (event.type === "turn_start") markRecipientStarted(sendId, agentId, event.turn_id);
  // Settle the send tracker from the events that carry a terminal outcome. Driven
  // from this existing boundary rather than a second `listen` per agent — that
  // would break the one-listener-per-agent invariant this module documents.
  if (event.type === "turn_end") {
    const outcome =
      event.outcome.status === "cancelled"
        ? "cancelled"
        : event.outcome.status === "failed"
          ? "failed"
          : "completed";
    settleTurn(event.turn_id, agentId, outcome);
    // Read off the turn itself, not a pending entry — `turn_start` consumed that
    // entry when it created the turn, so by the terminal the turn is the only
    // thing that still knows what this was.
    const ending = priorTurns.find((t) => t.turn_id === event.turn_id);
    turnTerminalHook?.(agentId, outcome, ending?.role === "agent" ? ending.kind : undefined);
  } else if (event.type === "message_failed") {
    settleRecipient(failedSendId, agentId, "failed");
    dispatchFailedHook?.(agentId);
  } else if (event.type === "message_cancelled") {
    settleRecipient(event.send_id, agentId, "cancelled");
  } else if (event.type === "agent_idle") {
    settleAgentIdle(agentId);
  }
  setTranscript(
    agentId,
    transcriptReducer(
      priorTurns,
      event,
      agentId,
      receivedAt,
      sendId,
      priorRuntime?.in_flight_turn_id,
      pendingKind,
    ),
  );
  runtimes[agentId] = runtimeReducer(priorRuntime, event);
  recordAccountUsage(agentId, event, receivedAt);
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
    agentHarness.clear();
    // The account-scoped usage store is app state like the rest of this module's,
    // so it is reset here too rather than leaving every suite to remember it.
    usageTesting.reset();
    for (const heartbeat of heartbeats.values()) {
      clearTimeout(heartbeat.handle);
    }
    heartbeats.clear();
    transcriptRevision = 0;
    for (const key of Object.keys(localSends)) {
      delete localSends[key];
    }
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
