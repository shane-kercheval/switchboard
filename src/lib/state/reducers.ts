// Pure reducers for the M2.5 unified-stream model.
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
// **Late-event drop semantics** (carried over from M1.5):
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
  NormalizedEvent,
  ReducerInput,
  TurnId,
} from "$lib/types";
import type { AgentRuntime, ToolCall, Turn, TurnItem } from "./types";

export function transcriptReducer(
  turns: Turn[],
  input: ReducerInput,
  agentId: AgentId,
  receivedAt: string,
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
          started_at: input.started_at,
          status: "streaming",
          items: [],
        },
      ];
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
      // separate so M3+ reasoning rendering doesn't accidentally fold
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
      if (input.outcome.status === "completed") {
        return updateTurn(turns, input.turn_id, {
          ...existing,
          status: "complete",
          ended_at: input.ended_at,
          usage: input.usage ?? undefined,
        });
      }
      return updateTurn(turns, input.turn_id, {
        ...existing,
        status: "failed",
        ended_at: input.ended_at,
        error: input.outcome.message,
        error_kind: input.outcome.kind,
        usage: input.usage ?? undefined,
      });
    }

    case "heartbeat_timeout": {
      const existing = findTurn(turns, input.turn_id);
      if (existing === undefined || existing.role !== "agent") return turns;
      if (existing.status !== "streaming") return turns;
      return updateTurn(turns, input.turn_id, {
        ...existing,
        status: "failed",
        ended_at: input.at,
        error: "no response from harness — retry?",
        // Heartbeat timeouts are frontend-synthesized adapter failures —
        // same retry semantics as a parser-emitted AdapterFailure.
        error_kind: "adapter_failure",
      });
    }

    case "hydrate": {
      // Per-agent scope. Live in-flight turns take precedence: any turn_id
      // already in the slice is preserved verbatim. New (disk-derived) turns
      // append. Order: hydrated turns appear in the order the parser
      // produced them, then any pre-existing live turns the listener
      // appended (which is the typical case — hydrate fires AFTER project
      // open, in-flight live turns may have already arrived).
      const existingIds = new Set(turns.map((t) => t.turn_id));
      const fromDisk = input.turns.filter((t) => !existingIds.has(t.turn_id)).map(loadedTurnToTurn);
      return [...fromDisk, ...turns];
    }

    // Agent-scoped events (rate_limit_event, session_meta, agent_idle) and
    // any future-added wire-format variants don't modify transcripts. The
    // wire-format enum is #[non_exhaustive] on the Rust side specifically
    // to make this graceful-degradation legal.
    default:
      return turns;
  }
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
    started_at: t.started_at,
    ended_at: t.ended_at ?? undefined,
    status: t.status,
    items: t.items.map(loadedItemToItem),
    usage: t.usage ?? undefined,
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
      // user-clicked Send transition (`idle → starting`) is driven by
      // the `dispatchUserTurn` state-module action — caller-driven,
      // outside the reducer. The reducer handles only the backend
      // event side of the state machine. See `AgentRuntime.run_status`
      // docstring in `./types.ts` for the full diagram.
      //
      // Sets unconditionally (not guarded on prior state): a regression
      // that emitted `TurnStart` while the runtime was `idle` should
      // still land on `processing` rather than silently dropping the
      // event. `last_error` is cleared so a successful new dispatch
      // doesn't surface stale failure state.
      return {
        ...runtime,
        run_status: "processing",
        in_flight_turn_id: input.turn_id,
        last_error: undefined,
      };

    case "turn_end":
      // **Do NOT flip run_status to "idle" here.** The dispatcher holds
      // `InFlight` past TurnEnd while post-terminal enrichment events flow
      // on the per-agent channel (Codex). `AgentIdle` is the signal for
      // "dispatcher will accept a new send." This is the load-bearing
      // distinction that makes the compose-bar gate correct for Codex.
      if (input.outcome.status === "completed") {
        return runtime;
      }
      return {
        ...runtime,
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
      };

    case "heartbeat_timeout":
      // The transcript reducer marks the turn failed; runtime mirrors that
      // by surfacing the error and clearing in_flight_turn_id. AgentIdle
      // arrives later when the dispatcher's drain task notices the stream
      // end — until then run_status stays "processing" (which is correct;
      // the backend hasn't actually released the guard yet).
      return {
        ...runtime,
        in_flight_turn_id: undefined,
        last_error: {
          message: "no response from harness — retry?",
          kind: "adapter_failure" satisfies FailureKind,
        },
      };

    case "session_meta":
      return {
        ...runtime,
        meta: {
          model: input.model,
          harness_version: input.harness_version,
          tools: input.tools,
          mcp_servers: input.mcp_servers,
          skills: input.skills,
        },
      };

    case "rate_limit_event":
      return { ...runtime, last_rate_limit: input.info };

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
        next.last_rate_limit = input.last_rate_limit;
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
    // M2.5 default: nothing to hydrate. M2.6 flips to "loading" at project
    // open and lands on "complete" or "failed" once the session-file load
    // finishes.
    hydration_status: "complete",
  };
}

function findTurn(turns: Turn[], turnId: TurnId): Turn | undefined {
  return turns.find((t) => t.turn_id === turnId);
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
): Turn[] {
  return [
    ...turns,
    {
      role: "user",
      turn_id: turnId,
      agent_id: agentId,
      started_at: startedAt,
      text,
    },
  ];
}

/// Internal helper namespace. The underscore prefix signals "not for
/// production callers" — both the state module (`index.svelte.ts`) and
/// the reducer tests import from here. Production callers go through
/// `index.svelte.ts::dispatchUserTurn`, which adds the recipient-picker
/// ergonomic (`ui.lastRecipientId`) and the `run_status: "starting"`
/// transition on top of this raw helper.
export const _internal = {
  appendUserTurn: appendUserTurnImpl,
};

// Re-export NormalizedEvent for downstream convenience.
export type { NormalizedEvent };
