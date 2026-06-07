// Pure reducers for the unified-stream model.
//
// Two reducers, one per state map:
//
// - `transcriptReducer(turns, event, agentId, receivedAt)`: produces the
//   next turn list for one agent.
// - `runtimeReducer(runtime, event)`: produces the next operational-state
//   record for one agent.
//
// **Purity contract.** Both reducers are pure functions: no side effects,
// no `new Date()`, no global access. Wall-clock timestamps for tool
// events (`tool_started.started_at`, `tool_completed.completed_at`) are
// minted at the **listener boundary** in `index.svelte.ts` and threaded
// in as the `receivedAt` parameter. This keeps the reducer deterministic
// (tests pass fixed timestamps; assertions check exact values) and
// satisfies AGENTS.md's "deterministic — no time-of-day or wall-clock
// dependencies in unit tests" rule.
//
// **Late-event drop semantics**:
// - Events for unknown `turn_id` → dropped. Defense against cross-agent
//   delivery bugs and races where a stream emits after the heartbeat has
//   already failed the turn.
// - Events for turns already in a terminal state (`complete` / `failed`) →
//   dropped. The dispatcher's drain task may continue emitting after the
//   frontend has heartbeat-timed-out a turn; without this guard the failed
//   turn would resurrect with late content.
//
// **Per-agent isolation**: the listener-and-state design routes events to
// the right `agent_id` *before* calling the reducer — these reducers
// operate on one agent's slice at a time. Unknown-discriminant events (a
// future Rust release adding a variant) fall through the `default` arm
// unchanged; the wire-format enums are `#[non_exhaustive]` on the Rust
// side so this graceful-degradation is the supported behavior.

import type {
  AgentId,
  FailureKind,
  LoadedTurn,
  LoadedTurnItem,
  MessageId,
  NormalizedEvent,
  ReducerInput,
  SendId,
  TurnId,
} from "$lib/types";
import type { AgentRuntime, PendingSend, ToolCall, Turn, TurnItem } from "./types";

export function transcriptReducer(
  turns: Turn[],
  input: ReducerInput,
  agentId: AgentId,
  receivedAt: string,
  // The send this turn belongs to, supplied by the caller for `turn_start`
  // (popped from the agent's pending-send FIFO). Stamped onto the new agent
  // turn so a fan-out's live responses group like its historical ones.
  sendId?: SendId,
  // The agent's currently in-flight turn_id (from runtime), supplied so the
  // `hydrate` merge never supersedes an actively-streaming live turn with a disk
  // re-read once that turn carries an early `hydration_key`. See
  // `resolveTurnCollision`'s `protectedTurnId`.
  inFlightTurnId?: TurnId,
): Turn[] {
  switch (input.type) {
    case "turn_start": {
      // Defense-in-depth: duplicate turn_start (dispatcher bug, late retry
      // delivery) must not append a second agent turn with the same id —
      // the unified-view's `{#each ... (turn_id)}` keyed render would
      // silently collapse them and state would diverge from the DOM.
      const existing = findTurn(turns, input.turn_id);
      if (existing !== undefined && existing.role === "agent") return turns;
      return [
        ...turns,
        {
          role: "agent",
          turn_id: input.turn_id,
          agent_id: agentId,
          send_id: sendId,
          started_at: input.started_at,
          status: "streaming",
          items: [],
        },
      ];
    }

    case "turn_identity": {
      // The live turn's dedup identity, stamped **early** (first assistant
      // message) so a concurrent disk re-read collapses against it instead of
      // duplicating. Two steps:
      //   1. Stamp `hydration_key` onto the live turn (idempotent on re-delivery).
      //   2. Reconcile: collapse any OTHER resident that already carries this key
      //      — a disk copy a refresh inserted before this event landed — INTO the
      //      live turn, which is `protectedTurnId` (`input.turn_id`) so it always
      //      survives, absorbing the disk copy's fields.
      // A stray event (unknown turn_id) is a no-op; absent entirely, dedup falls
      // back to the `turn_end` stamp (graceful degradation to the turn-end key).
      const existing = findTurn(turns, input.turn_id);
      if (existing === undefined || existing.role !== "agent") return turns;
      const key = input.hydration_key;
      const liveStamped: AgentTurn =
        existing.hydration_key === undefined ? { ...existing, hydration_key: key } : existing;

      let live = liveStamped;
      const collapsed = new Set<TurnId>();
      for (const t of turns) {
        if (t.turn_id === input.turn_id || t.role !== "agent") continue;
        if (t.hydration_key === key) {
          live = resolveTurnCollision(live, t, input.turn_id);
          collapsed.add(t.turn_id);
        }
      }
      if (liveStamped === existing && collapsed.size === 0) return turns;
      return turns
        .filter((t) => !collapsed.has(t.turn_id))
        .map((t) => (t.turn_id === input.turn_id ? live : t));
    }

    case "message_cancelled": {
      // A *queued* send was dropped before it started (backend-authoritative —
      // see `NormalizedEvent::MessageCancelled`). Render a cancelled agent turn
      // under its prompt. `sendId` is resolved by the caller from the pending
      // entry (by `message_id`); without it there is nothing to attribute, so
      // this is a stray and a no-op. Idempotent on the derived `turn_id`.
      if (sendId === undefined) return turns;
      const turn_id = `cancelled-${input.message_id}`;
      if (findTurn(turns, turn_id) !== undefined) return turns;
      return [
        ...turns,
        {
          role: "agent",
          turn_id,
          agent_id: agentId,
          send_id: sendId,
          started_at: receivedAt,
          status: "cancelled",
          items: [],
        },
      ];
    }

    case "message_failed": {
      // A send failed before any turn started (adapter failed to launch, or the
      // journal write failed). Render a failed agent turn under its prompt so
      // the failure surfaces in the transcript — the same place post-start
      // failures and the post-reload journal marker appear. `sendId` is resolved
      // by the caller from the pending entry (the same lookup `turn_start` uses,
      // including the pre-receipt front-fallback). A failure whose entry was
      // already consumed by a `turn_start` is post-start (the live turn renders
      // it) and arrives here with `sendId` undefined — a no-op, so there is no
      // double-render. Idempotent on the derived `turn_id`.
      if (sendId === undefined) return turns;
      return appendFailedTurnImpl(
        turns,
        agentId,
        `failed-${input.message_id}`,
        receivedAt,
        input.error,
        sendId,
      );
    }

    case "content_chunk": {
      const existing = findTurn(turns, input.turn_id);
      if (existing === undefined || existing.role !== "agent") return turns;
      if (existing.status !== "streaming") return turns;
      // Coalesce adjacent text chunks of the same `kind` into a single
      // TextChunk item. Real Claude streaming produces ~10-char chunks
      // per content_chunk event; without coalescing the renderer turns a
      // normal paragraph into N separately-rendered <div>s. A tool call
      // between two text runs is NOT coalesced (it sits at its own
      // index in `items`, preserving the text/tool/text ordering
      // contract). Different ContentKind (text vs. thinking) stays
      // separate so future reasoning rendering doesn't accidentally fold
      // into plain text.
      const lastIndex = existing.items.length - 1;
      const lastItem = lastIndex >= 0 ? existing.items[lastIndex] : undefined;
      if (lastItem?.item_kind === "text" && lastItem.kind === input.kind) {
        const updatedItems = [...existing.items];
        updatedItems[lastIndex] = {
          item_kind: "text",
          kind: lastItem.kind,
          text: lastItem.text + input.text,
        };
        return updateTurn(turns, input.turn_id, { ...existing, items: updatedItems });
      }
      return updateTurn(turns, input.turn_id, {
        ...existing,
        items: [...existing.items, { item_kind: "text", kind: input.kind, text: input.text }],
      });
    }

    case "tool_started": {
      const existing = findTurn(turns, input.turn_id);
      if (existing === undefined || existing.role !== "agent") return turns;
      if (existing.status !== "streaming") return turns;
      // Defensive duplicate-guard: if the same tool_use_id is already
      // tracked, ignore the late re-arrival rather than appending a
      // duplicate row. Adapter contracts say one ToolStarted per
      // tool_use_id, but the cost of being wrong is silent UI duplication.
      if (
        existing.items.some(
          (item) => item.item_kind === "tool" && item.tool_use_id === input.tool_use_id,
        )
      ) {
        return turns;
      }
      const newTool: ToolCall = {
        item_kind: "tool",
        tool_use_id: input.tool_use_id,
        kind: input.kind,
        name: input.name,
        input: input.input,
        started_at: receivedAt,
      };
      return updateTurn(turns, input.turn_id, {
        ...existing,
        items: [...existing.items, newTool],
      });
    }

    case "tool_completed": {
      const existing = findTurn(turns, input.turn_id);
      if (existing === undefined || existing.role !== "agent") return turns;
      if (existing.status !== "streaming") return turns;
      // Find by tool_use_id within items (skip non-tool items). If no
      // matching ToolStarted preceded this ToolCompleted, drop — the
      // adapter would be violating contract.
      const idx = existing.items.findIndex(
        (item) => item.item_kind === "tool" && item.tool_use_id === input.tool_use_id,
      );
      if (idx === -1) return turns;
      const prior = existing.items[idx];
      // findIndex above guarantees this is a ToolCall, but the predicate
      // narrowing doesn't survive the array subscript — narrow again at
      // the assignment site.
      if (prior?.item_kind !== "tool") return turns;
      const updatedItems = [...existing.items];
      updatedItems[idx] = {
        ...prior,
        output: input.output,
        is_error: input.is_error,
        completed_at: receivedAt,
      };
      return updateTurn(turns, input.turn_id, {
        ...existing,
        items: updatedItems,
      });
    }

    case "turn_end": {
      const existing = findTurn(turns, input.turn_id);
      if (existing === undefined || existing.role !== "agent") return turns;
      if (existing.status !== "streaming") return turns;
      // Stamp the live-matched hydration key (Claude only) onto the completing
      // turn so a later disk re-read of the same turn dedups against it instead
      // of appending a second copy (see the `hydrate` merge). **Preserve an
      // early key when the terminal carries none**: an in-flight turn may
      // already hold a `hydration_key` from the `turn_identity` event, and a
      // *synthesized* terminal (a cancelled turn's `TurnEnd`, built by the
      // dispatcher) carries `hydration_key: None` — clearing it would leave the
      // cancelled turn keyless and duplicate it against its keyed disk copy. For
      // parser-emitted terminals `input.hydration_key` is present and equals the
      // early key, so this is a no-op there.
      const hydration_key = input.hydration_key ?? existing.hydration_key;
      const baseUpdate = {
        ...existing,
        ended_at: input.ended_at,
        usage: input.usage ?? undefined,
        spend: input.spend ?? undefined,
        model: input.model ?? undefined,
        effort: input.effort ?? undefined,
        hydration_key,
      };
      if (input.outcome.status === "completed") {
        return updateTurn(turns, input.turn_id, {
          ...baseUpdate,
          status: "complete",
        });
      }
      if (input.outcome.status === "cancelled") {
        return updateTurn(turns, input.turn_id, {
          ...baseUpdate,
          status: "cancelled",
          items: stopPendingTools(existing.items, input.ended_at, "cancelled"),
        });
      }
      return updateTurn(turns, input.turn_id, {
        ...baseUpdate,
        status: "failed",
        error: input.outcome.message,
        error_kind: input.outcome.kind,
        items: stopPendingTools(existing.items, input.ended_at, "failed"),
      });
    }

    case "hydrate": {
      // Per-agent scope. Dedup on the **stable hydration key** when a turn has
      // one, falling back to `turn_id` for keyless turns. Keying on `turn_id`
      // alone is unsafe for re-reads: a parser mints a *fresh* `turn_id` every
      // parse, so the same on-disk turn would look new on a second read and
      // duplicate — the stable key is parse-invariant and recognizes it.
      //
      // **Collision handling is upsert-by-precedence, not first-write-wins.**
      // When a disk turn collides with a resident:
      //   - **Real `hydration_key` match** (both AGENT turns carry the same key):
      //     resolve by terminality (`resolveTurnCollision`). A terminal disk turn
      //     *supersedes* a still-`streaming` resident — this is the stranded
      //     mid-flight partial (e.g. a switch-back/reopen re-read that caught the
      //     turn before it finished) being replaced by its completed copy; the
      //     resident's live-only fields are merged in. Otherwise the resident is
      //     kept. This is what makes the merge converge instead of stacking a
      //     "complete-looking turn with a spinning tool."
      //   - **`turn_id` fallback match** (either side lacks a key): keep the
      //     resident, drop the disk turn. This is the "don't clobber a live
      //     in-flight turn" path — a live turn has *no* `hydration_key` until it
      //     ends, so it only ever matches via `turn_id`. Superseding it would
      //     also change its `turn_id` and orphan its still-arriving live events.
      //
      // **Scope: keyed AGENT turns only.** User turns (and keyless harnesses)
      // fall back to `turn_id`, fresh per parse, so they never dedup across
      // re-reads — user content stays owned by the journal overlay (replaced
      // wholesale, dup-safe) and must not be re-merged into a per-agent slice.
      const dedupKey = (t: { hydration_key?: string | null; turn_id: TurnId }): string =>
        t.hydration_key ?? t.turn_id;
      const residentByKey = new Map(turns.map((t) => [dedupKey(t), t]));
      const disk = input.turns.map(loadedTurnToTurn);

      // Resident replacements (in place, preserving order) for key-matched
      // collisions; brand-new disk turns are prepended (disk-order, as before).
      const replacements = new Map<string, Turn>();
      const fresh: Turn[] = [];
      for (const d of disk) {
        const resident = residentByKey.get(dedupKey(d));
        if (resident === undefined) {
          fresh.push(d);
        } else if (
          d.role === "agent" &&
          resident.role === "agent" &&
          d.hydration_key !== undefined &&
          d.hydration_key === resident.hydration_key
        ) {
          replacements.set(dedupKey(d), resolveTurnCollision(resident, d, inFlightTurnId));
        }
        // else: turn_id-fallback collision → keep the resident, drop `d`.
      }
      const merged = turns.map((t) => replacements.get(dedupKey(t)) ?? t);
      return [...fresh, ...merged];
    }

    // Agent-scoped events (rate_limit_event, session_meta, agent_idle),
    // `liveness` (a content-free heartbeat — handled by the runtime, never a
    // transcript item), `heartbeat_timeout` (sets the runtime `quiet_since`,
    // not transcript state), and any future-added wire-format variants don't
    // modify transcripts. The wire-format enum is #[non_exhaustive] on the
    // Rust side specifically to make this graceful-degradation legal.
    default:
      return turns;
  }
}

function stopPendingTools(
  items: TurnItem[],
  stoppedAt: string,
  stopReason: "cancelled" | "failed",
): TurnItem[] {
  return items.map((item) => {
    if (item.item_kind !== "tool") return item;
    if (item.completed_at !== undefined) return item;
    return { ...item, stopped_at: stoppedAt, stop_reason: stopReason };
  });
}

type AgentTurn = Extract<Turn, { role: "agent" }>;

/// Resolve two agent turns that share a dedup identity into the single turn to
/// keep. **Precedence: a finished turn supersedes a still-`streaming` one.**
/// `incoming` wins only when `existing` is `streaming` and `incoming` is
/// terminal (complete/failed/cancelled); otherwise `existing` wins (ties and
/// "don't clobber a more-complete resident"). The survivor absorbs the loser's
/// **live-only fields** — the optional enrichments a disk parse can't recover
/// (cost/overage `spend`, the `usage` window, model/effort, failure detail) —
/// so superseding a partial never blanks a field the loser already had. Content
/// (`items`, `status`, `turn_id`, timestamps) always comes from the winner.
///
/// `protectedTurnId`, when given, is the turn_id of the **actively-streaming
/// live turn** — it is never the loser, even against a terminal incoming. This
/// is load-bearing once a live turn carries a `hydration_key` while streaming
/// (the early `turn_identity` stamp): without it, a terminal disk re-read could
/// key-match the active turn and supersede it here, changing its `turn_id` and
/// orphaning its still-arriving events (the unknown-`turn_id` guards drop them).
/// Status alone can't tell a *stranded disk partial* (safe to supersede) from an
/// *active live turn* (must not be) — both are `streaming` — so the caller
/// supplies the in-flight turn_id. Omit it (the hydrate path with no live turn,
/// or pre-stamp) for pure terminality precedence.
function resolveTurnCollision(
  existing: AgentTurn,
  incoming: AgentTurn,
  protectedTurnId?: TurnId,
): AgentTurn {
  let incomingSupersedes: boolean;
  if (existing.turn_id === protectedTurnId) {
    incomingSupersedes = false;
  } else if (incoming.turn_id === protectedTurnId) {
    incomingSupersedes = true;
  } else {
    incomingSupersedes = existing.status === "streaming" && incoming.status !== "streaming";
  }
  const [winner, loser] = incomingSupersedes ? [incoming, existing] : [existing, incoming];
  // Note: when a protected *streaming* live turn wins over a *terminal* loser,
  // it absorbs the loser's `ended_at` (and usage/spend) while keeping
  // `status: "streaming"` — so a streaming turn can briefly carry an `ended_at`.
  // This is load-bearing: completion must be read from `status`, never from
  // `ended_at` being set. (`turn_end` overwrites all of it moments later.)
  //
  // Fields fill at the **top level** (`usage`/`spend` are taken whole, not
  // sub-merged): a winner whose `usage` lacks a nested `context_window` does NOT
  // inherit the loser's. Safe today because the only superseded loser is a
  // *non-terminal* resident (terminal-over-streaming, or a protected live turn
  // that's the winner), which carries no end-of-turn `usage` yet. Revisit the
  // sub-field merge only if supersession ever broadens to terminal-over-terminal.
  return {
    ...winner,
    send_id: winner.send_id ?? loser.send_id,
    ended_at: winner.ended_at ?? loser.ended_at,
    usage: winner.usage ?? loser.usage,
    spend: winner.spend ?? loser.spend,
    model: winner.model ?? loser.model,
    effort: winner.effort ?? loser.effort,
    error: winner.error ?? loser.error,
    error_kind: winner.error_kind ?? loser.error_kind,
    hydration_key: winner.hydration_key ?? loser.hydration_key,
  };
}

function loadedTurnToTurn(t: LoadedTurn): Turn {
  if (t.role === "user") {
    return {
      role: "user",
      turn_id: t.turn_id,
      agent_id: t.agent_id,
      started_at: t.started_at,
      text: t.text,
    };
  }
  return {
    role: "agent",
    turn_id: t.turn_id,
    agent_id: t.agent_id,
    send_id: t.send_id ?? undefined,
    started_at: t.started_at,
    ended_at: t.ended_at ?? undefined,
    status: t.status,
    items: t.items.map(loadedItemToItem),
    usage: t.usage ?? undefined,
    spend: t.spend ?? undefined,
    model: t.model ?? undefined,
    effort: t.effort ?? undefined,
    hydration_key: t.hydration_key ?? undefined,
  };
}

function loadedItemToItem(item: LoadedTurnItem): TurnItem {
  if (item.item_kind === "text") {
    return { item_kind: "text", kind: item.kind, text: item.text };
  }
  return {
    item_kind: "tool",
    tool_use_id: item.tool_use_id,
    kind: item.kind,
    name: item.name,
    input: item.input,
    output: item.output ?? undefined,
    is_error: item.is_error ?? undefined,
    started_at: item.started_at,
    completed_at: item.completed_at ?? undefined,
  };
}

export function runtimeReducer(runtime: AgentRuntime, input: ReducerInput): AgentRuntime {
  switch (input.type) {
    case "turn_start":
      // Backend-confirmed dispatch: `starting → processing`. The
      // correlated `message_id` matches the `pending_message_id` recorded
      // when `send_message` resolved; we adopt the real `turn_id` here.
      // The user-clicked Send transition (`idle → starting`) is driven by
      // the `dispatchUserTurn` state-module action — caller-driven,
      // outside the reducer. The reducer handles only the backend
      // event side of the state machine. See `AgentRuntime.run_status`
      // docstring in `./types.ts` for the full diagram.
      //
      // Sets unconditionally (not guarded on prior state): a regression
      // that emitted `TurnStart` while the runtime was `idle` should
      // still land on `processing` rather than silently dropping the
      // event. `last_error` and `pending_message_id` are cleared so a
      // successful new dispatch doesn't surface stale state.
      return {
        ...runtime,
        run_status: "processing",
        in_flight_turn_id: input.turn_id,
        last_error: undefined,
        quiet_since: undefined,
        // Consume the pending-send entry this turn belongs to (by message_id,
        // else the front — see `pickPendingIndex`).
        pending_sends: removePending(
          runtime.pending_sends,
          pickPendingIndex(runtime.pending_sends, input.message_id),
        ),
      };

    case "message_failed": {
      // A send failed before any turn started (journal write failed, or the
      // adapter failed to launch pre-`TurnStart`). Prune its pending entry (by
      // message_id, else front) and surface the error. A truly stray event
      // (no pending sends) is ignored.
      const idx = pickPendingIndex(runtime.pending_sends, input.message_id);
      if (idx < 0) return runtime;
      const pending_sends = removePending(runtime.pending_sends, idx);
      const last_error: { message: string; kind: FailureKind } = {
        message: input.error,
        kind: "adapter_failure",
      };
      // Flip to idle only if this was the *starting* send. A queued send that
      // fails pre-start (the agent is `processing` another turn) must not stomp
      // the live turn — `AgentIdle` settles run_status when the backlog drains.
      return runtime.run_status === "starting"
        ? { ...runtime, run_status: "idle", pending_sends, last_error }
        : { ...runtime, pending_sends, last_error };
    }

    case "message_cancelled": {
      // A queued send was dropped before starting. Prune its pending entry
      // (exact `message_id` match — no front-fallback; the backend always
      // carries the real id). Cancellation is not an error, so no `last_error`.
      // Flip to idle only if this was the *starting* send (nothing is running);
      // a queued send cancelled while another turn runs leaves run_status alone.
      const idx = runtime.pending_sends?.findIndex((p) => p.message_id === input.message_id) ?? -1;
      if (idx < 0) return runtime;
      const pending_sends = removePending(runtime.pending_sends, idx);
      return runtime.run_status === "starting"
        ? { ...runtime, run_status: "idle", pending_sends }
        : { ...runtime, pending_sends };
    }

    case "turn_end":
      // **Do NOT flip run_status to "idle" here.** The dispatcher holds
      // `InFlight` past TurnEnd while post-terminal enrichment events flow
      // on the per-agent channel (Codex). `AgentIdle` is the signal for
      // "dispatcher will accept a new send." This is the load-bearing
      // distinction that makes the compose-bar gate correct for Codex.
      // Completed and cancelled are not errors — leave runtime untouched
      // (AgentIdle clears in_flight_turn_id). Only a real failure surfaces
      // last_error.
      if (input.outcome.status === "completed" || input.outcome.status === "cancelled") {
        return runtime.quiet_since !== undefined ? { ...runtime, quiet_since: undefined } : runtime;
      }
      return {
        ...runtime,
        quiet_since: undefined,
        last_error: {
          message: input.outcome.message,
          kind: input.outcome.kind,
        },
      };

    case "agent_idle":
      // Authoritative signal that the dispatcher's `AgentIdleGuard` has
      // dropped → the channel is fully drained and a new send won't
      // return `Busy`.
      //
      // **Guarded transition: processing → idle only.** A stray
      // `agent_idle` arriving while `run_status === "starting"` (out of
      // protocol; the dispatcher shouldn't emit `AgentIdle` for a turn
      // that never got a `TurnStart`) would race the sendability gate
      // back open before the legitimate `TurnStart` arrived. The only
      // legal path from `starting → idle` is `failSendStart`. See the
      // state-machine docstring on `AgentRuntime.run_status`.
      if (runtime.run_status !== "processing") return runtime;
      return {
        ...runtime,
        run_status: "idle",
        in_flight_turn_id: undefined,
        quiet_since: undefined,
      };

    case "heartbeat_timeout":
      // The turn has been silent but is still alive on the backend (it holds
      // the busy-lock). Surface the silence by recording when it went quiet —
      // do NOT fail it, do NOT clear `in_flight_turn_id`, do NOT set
      // `last_error`. A frontend "failed" would be a lie and would not release
      // the lock. `quiet_since` is cleared on the next activity for this turn
      // (or on turn end). Scoped to the in-flight turn so a stale timer for a
      // resolved turn can't paint a newer one.
      if (input.turn_id !== runtime.in_flight_turn_id || runtime.quiet_since !== undefined) {
        return runtime;
      }
      return { ...runtime, quiet_since: input.at };

    case "content_chunk":
    case "liveness":
    case "turn_identity":
    case "tool_started":
    case "tool_completed":
      // Any sign of life clears the quiet marker for the turn the heartbeat is
      // watching (the timer itself re-arms in `manageHeartbeat`). No-op when
      // not quiet or when the event is for a different turn.
      if (runtime.quiet_since === undefined || input.turn_id !== runtime.in_flight_turn_id) {
        return runtime;
      }
      return { ...runtime, quiet_since: undefined };

    case "session_meta":
      return {
        ...runtime,
        meta: {
          // Empty model means "no model info on this event," not "set the
          // model to blank." Antigravity only reports a model when the
          // selection changed (first turn, or an explicit switch); on a
          // plain resume it sends "" and must not erase the model an earlier
          // turn already showed. Claude/Codex/Gemini always send a non-empty
          // model, so this is a no-op for them.
          model: input.model !== "" ? input.model : (runtime.meta?.model ?? ""),
          harness_version: input.harness_version,
          tools: input.tools,
          mcp_servers: input.mcp_servers,
          skills: input.skills,
        },
      };

    case "rate_limit_event":
      // A live event overwrites the in-memory value; the `as_of` qualifier
      // (the on-disk snapshot's age) is meaningless once live data lands, so
      // clear it to null — never stamp `now`, which would spuriously age an
      // actively-streaming session past the staleness threshold.
      return { ...runtime, last_rate_limit: input.info, last_rate_limit_as_of: null };

    case "hydrate": {
      // **Fill-if-empty for scalars.** Live `session_meta` and
      // `rate_limit_event` always overwrite; `hydrate.meta` /
      // `hydrate.last_rate_limit` only fill when the runtime field is
      // currently absent. Naturally handles a slow hydrate resolving
      // after a live event already populated the same field — the late
      // hydrate sees `Some(_)` and no-ops. Pinned by the
      // `live_wins_over_subsequent_hydrate` test below.
      const next: AgentRuntime = {
        ...runtime,
        hydration_status: "complete",
      };
      if (next.meta === undefined && input.meta != null) {
        next.meta = {
          model: input.meta.model,
          harness_version: input.meta.harness_version,
          tools: input.meta.tools,
          mcp_servers: input.meta.mcp_servers,
          skills: input.meta.skills,
        };
      }
      if (next.last_rate_limit === undefined && input.last_rate_limit != null) {
        // Fill `last_rate_limit` and its `as_of` together — they're one unit.
        // Only when the runtime had no value: if a live event already
        // populated it (and cleared `as_of` to null), this no-ops and the
        // live value + its null `as_of` stay in place.
        next.last_rate_limit = input.last_rate_limit;
        next.last_rate_limit_as_of = input.last_rate_limit_as_of ?? null;
      }
      if (input.warnings !== undefined && input.warnings.length > 0) {
        next.parse_warnings = input.warnings;
      }
      return next;
    }

    default:
      return runtime;
  }
}

export function freshRuntime(agentId: AgentId): AgentRuntime {
  return {
    agent_id: agentId,
    run_status: "idle",
    // Default for newly-created agents (create_agent flow): nothing to
    // hydrate. The hydration flow flips this to "loading" on project open
    // / attach, then lands on "complete" or "failed" once the session-file
    // load finishes.
    hydration_status: "complete",
  };
}

function findTurn(turns: Turn[], turnId: TurnId): Turn | undefined {
  return turns.find((t) => t.turn_id === turnId);
}

/// The pending-send entry a backend event refers to: the one whose receipt
/// matches `messageId`, else the front (covers the race where `turn_start` /
/// `message_failed` beats the `send_message` IPC receipt, so the entry has no
/// `message_id` yet — the backend runs turns in dispatch order, so the front is
/// the next to start). `-1` when there are no pending sends (a stray event).
function pickPendingIndex(pending: PendingSend[] | undefined, messageId: MessageId): number {
  if (pending === undefined || pending.length === 0) return -1;
  const byMsg = pending.findIndex((p) => p.message_id === messageId);
  if (byMsg >= 0) return byMsg;
  // No receipt match. Fall back to the front *only* if it hasn't recorded its
  // receipt yet — the race where the event beat `recordSendAccepted`. If the
  // front already has a (different) receipt, this event doesn't correlate to
  // any pending send: it's stray/stale, so don't consume anything.
  return pending[0]?.message_id === undefined ? 0 : -1;
}

/// Remove `index` from the pending list, returning `undefined` (not `[]`) when
/// it empties so a fresh runtime and a fully-drained one compare equal.
function removePending(
  pending: PendingSend[] | undefined,
  index: number,
): PendingSend[] | undefined {
  if (pending === undefined || index < 0 || index >= pending.length) return pending;
  const next = [...pending.slice(0, index), ...pending.slice(index + 1)];
  return next.length === 0 ? undefined : next;
}

function updateTurn(turns: Turn[], turnId: TurnId, next: Turn): Turn[] {
  return turns.map((t) => (t.turn_id === turnId ? next : t));
}

/// Synchronously append a user-role turn to a transcript. Caller-driven
/// (compose-bar Send), not event-driven from the backend — kept separate
/// from the reducer so the reducer stays pure-eventful.
///
/// **Not a public API**. Exposed under `_internal` for the state module
/// (which adds runtime-state and ergonomic updates on top) and for
/// reducer-level unit tests.
function appendUserTurnImpl(
  turns: Turn[],
  agentId: AgentId,
  turnId: TurnId,
  text: string,
  startedAt: string,
  sendId?: SendId,
): Turn[] {
  return [
    ...turns,
    {
      role: "user",
      turn_id: turnId,
      agent_id: agentId,
      send_id: sendId,
      started_at: startedAt,
      text,
    },
  ];
}

/// Append a synthetic `failed` agent turn for a send that failed *before* any
/// `turn_start` arrived (adapter failed to launch, journal write failed, or the
/// `send_message` IPC rejected). Mirrors the pre-start `message_cancelled` row:
/// empty items, terminal status, attributed to its send. Idempotent on
/// `turnId`. Post-start failures are NOT routed here — they update the existing
/// streaming turn via `turn_end`, so there is no double-render.
function appendFailedTurnImpl(
  turns: Turn[],
  agentId: AgentId,
  turnId: TurnId,
  startedAt: string,
  error: string,
  sendId?: SendId,
): Turn[] {
  if (findTurn(turns, turnId) !== undefined) return turns;
  return [
    ...turns,
    {
      role: "agent",
      turn_id: turnId,
      agent_id: agentId,
      send_id: sendId,
      started_at: startedAt,
      status: "failed",
      items: [],
      error,
      error_kind: "adapter_failure",
    },
  ];
}

/// Internal helper namespace. The underscore prefix signals "not for
/// production callers" — both the state module (`index.svelte.ts`) and
/// the reducer tests import from here. Production callers go through
/// `index.svelte.ts::dispatchUserTurn`, which adds the pending-send
/// bookkeeping and the `run_status: "starting"` transition on top of this
/// raw helper.
export const _internal = {
  appendUserTurn: appendUserTurnImpl,
  appendFailedTurn: appendFailedTurnImpl,
};

// Re-export NormalizedEvent for downstream convenience.
export type { NormalizedEvent };
