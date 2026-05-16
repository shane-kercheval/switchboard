import { describe, expect, it } from "vitest";
import type { NormalizedEvent, ReducerInput } from "$lib/types";
import { _internal, freshRuntime, runtimeReducer, transcriptReducer } from "./reducers";
import type { AgentRuntime, TextChunk, ToolCall, Turn } from "./types";

const AGENT_A = "00000000-0000-7000-8000-000000000aaa";
const AGENT_B = "00000000-0000-7000-8000-000000000bbb";
const TURN_1 = "00000000-0000-7000-8000-000000000001";
const TURN_2 = "00000000-0000-7000-8000-000000000002";

// Fixed timestamp used as `receivedAt` across all transcriptReducer
// invocations in tests. Pinning a constant makes tool-event ordering and
// timestamp assertions deterministic.
const RECEIVED_AT = "2026-05-16T00:00:00Z";

function turnStart(turnId: string, startedAt = "2026-05-16T00:00:00Z"): NormalizedEvent {
  return { type: "turn_start", turn_id: turnId, started_at: startedAt };
}

function contentChunk(turnId: string, text: string): NormalizedEvent {
  return { type: "content_chunk", turn_id: turnId, kind: "text", text };
}

function turnEndCompleted(turnId: string, endedAt = "2026-05-16T00:00:05Z"): NormalizedEvent {
  return {
    type: "turn_end",
    turn_id: turnId,
    outcome: { status: "completed" },
    ended_at: endedAt,
  };
}

function turnEndFailed(turnId: string, message: string): NormalizedEvent {
  return {
    type: "turn_end",
    turn_id: turnId,
    outcome: { status: "failed", kind: "harness_error", message },
    ended_at: "2026-05-16T00:00:05Z",
  };
}

function reduce(turns: Turn[], input: ReducerInput, agentId: string = AGENT_A): Turn[] {
  return transcriptReducer(turns, input, agentId, RECEIVED_AT);
}

describe("transcriptReducer", () => {
  describe("turn_start", () => {
    it("appends a streaming agent turn with empty items", () => {
      const turns = reduce([], turnStart(TURN_1));
      expect(turns).toHaveLength(1);
      const turn = turns[0];
      expect(turn).toMatchObject({
        role: "agent",
        turn_id: TURN_1,
        agent_id: AGENT_A,
        status: "streaming",
        items: [],
      });
    });

    it("is idempotent for duplicate turn_start with same turn_id", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, turnStart(TURN_1));
      expect(turns).toHaveLength(1);
    });
  });

  describe("content_chunk → items", () => {
    it("pushes a TextChunk to items on the matching streaming turn", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, contentChunk(TURN_1, "hello "));
      turns = reduce(turns, contentChunk(TURN_1, "world"));
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.items).toEqual([
        { item_kind: "text", kind: "text", text: "hello " },
        { item_kind: "text", kind: "text", text: "world" },
      ]);
    });

    it("preserves chunk boundaries (does not concatenate)", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, contentChunk(TURN_1, "alpha"));
      turns = reduce(turns, contentChunk(TURN_1, "beta"));
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.items).toHaveLength(2);
    });

    it("drops chunks for unknown turn_id", () => {
      const turns = reduce([], contentChunk(TURN_1, "orphan"));
      expect(turns).toHaveLength(0);
    });

    it("drops chunks for already-terminal turns", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, turnEndCompleted(TURN_1));
      turns = reduce(turns, contentChunk(TURN_1, "late"));
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.items).toEqual([]);
      expect(turn.status).toBe("complete");
    });
  });

  describe("tool_started / tool_completed → items", () => {
    function toolStarted(turnId: string, toolUseId: string, name = "Bash"): NormalizedEvent {
      return {
        type: "tool_started",
        turn_id: turnId,
        tool_use_id: toolUseId,
        kind: "builtin",
        name,
        input: { command: "echo hi" },
      };
    }
    function toolCompleted(
      turnId: string,
      toolUseId: string,
      output = "hi\n",
      isError = false,
    ): NormalizedEvent {
      return {
        type: "tool_completed",
        turn_id: turnId,
        tool_use_id: toolUseId,
        output,
        is_error: isError,
      };
    }

    it("appends a ToolCall to items with the listener-stamped started_at", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, toolStarted(TURN_1, "tool-1"));
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.items).toHaveLength(1);
      const item = turn.items[0];
      expect(item).toMatchObject({
        item_kind: "tool",
        tool_use_id: "tool-1",
        kind: "builtin",
        name: "Bash",
        started_at: RECEIVED_AT,
      });
      if (item?.item_kind !== "tool") throw new Error("unreachable");
      expect(item.output).toBeUndefined();
      expect(item.completed_at).toBeUndefined();
    });

    it("populates output/is_error/completed_at on tool_completed", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, toolStarted(TURN_1, "tool-1"));
      turns = reduce(turns, toolCompleted(TURN_1, "tool-1", "done", false));
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      const item = turn.items[0];
      if (item?.item_kind !== "tool") throw new Error("unreachable");
      expect(item).toMatchObject({
        output: "done",
        is_error: false,
        completed_at: RECEIVED_AT,
      });
    });

    it("preserves text/tool/text ORDERING in items (load-bearing for renderer)", () => {
      // The reason items exists as a single ordered array instead of two
      // separate arrays. Real Claude turns produce text → tool → text;
      // the renderer needs to know which side of the tool each text chunk
      // belongs on.
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, contentChunk(TURN_1, "Running… "));
      turns = reduce(turns, toolStarted(TURN_1, "tool-1"));
      turns = reduce(turns, toolCompleted(TURN_1, "tool-1"));
      turns = reduce(turns, contentChunk(TURN_1, "Done."));
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");

      expect(turn.items).toHaveLength(3);
      expect(turn.items[0]).toMatchObject({ item_kind: "text", text: "Running… " });
      expect(turn.items[1]).toMatchObject({ item_kind: "tool", tool_use_id: "tool-1" });
      expect(turn.items[2]).toMatchObject({ item_kind: "text", text: "Done." });
    });

    it("preserves ordering for multi-tool interleaving (text/tool/text/tool/text)", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, contentChunk(TURN_1, "before "));
      turns = reduce(turns, toolStarted(TURN_1, "tool-1", "First"));
      turns = reduce(turns, toolCompleted(TURN_1, "tool-1"));
      turns = reduce(turns, contentChunk(TURN_1, "between "));
      turns = reduce(turns, toolStarted(TURN_1, "tool-2", "Second"));
      turns = reduce(turns, toolCompleted(TURN_1, "tool-2"));
      turns = reduce(turns, contentChunk(TURN_1, "after"));

      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      const sequence = turn.items.map((item) =>
        item.item_kind === "text" ? `t:${item.text}` : `T:${item.tool_use_id}`,
      );
      expect(sequence).toEqual(["t:before ", "T:tool-1", "t:between ", "T:tool-2", "t:after"]);
    });

    it("ignores tool_completed with no matching tool_use_id", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, toolCompleted(TURN_1, "no-such-tool"));
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.items).toEqual([]);
    });

    it("ignores duplicate tool_started for the same tool_use_id", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, toolStarted(TURN_1, "tool-1"));
      turns = reduce(turns, toolStarted(TURN_1, "tool-1", "Different"));
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      const toolItems = turn.items.filter((item): item is ToolCall => item.item_kind === "tool");
      expect(toolItems).toHaveLength(1);
      expect(toolItems[0]?.name).toBe("Bash"); // first one wins
    });

    it("tool_completed only mutates the matching item — others untouched", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, toolStarted(TURN_1, "tool-1"));
      turns = reduce(turns, contentChunk(TURN_1, "mid"));
      turns = reduce(turns, toolStarted(TURN_1, "tool-2", "Other"));
      turns = reduce(turns, toolCompleted(TURN_1, "tool-1", "first-output"));

      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      const tool1 = turn.items.find(
        (item): item is ToolCall => item.item_kind === "tool" && item.tool_use_id === "tool-1",
      );
      const tool2 = turn.items.find(
        (item): item is ToolCall => item.item_kind === "tool" && item.tool_use_id === "tool-2",
      );
      expect(tool1?.output).toBe("first-output");
      expect(tool1?.completed_at).toBe(RECEIVED_AT);
      // tool-2 still in-flight — completion fields undefined.
      expect(tool2?.output).toBeUndefined();
      expect(tool2?.completed_at).toBeUndefined();
      // Text chunk in between is untouched.
      const text = turn.items.find((item): item is TextChunk => item.item_kind === "text");
      expect(text?.text).toBe("mid");
    });
  });

  describe("turn_end", () => {
    it("transitions a streaming turn to complete + records usage", () => {
      let turns = reduce([], turnStart(TURN_1));
      const ev: NormalizedEvent = {
        type: "turn_end",
        turn_id: TURN_1,
        outcome: { status: "completed" },
        ended_at: "2026-05-16T00:00:05Z",
        usage: {
          input_tokens: 100,
          output_tokens: 20,
          context_window: 200_000,
          total_cost_usd: 0.012,
        },
      };
      turns = reduce(turns, ev);
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.status).toBe("complete");
      expect(turn.ended_at).toBe("2026-05-16T00:00:05Z");
      expect(turn.usage?.input_tokens).toBe(100);
      expect(turn.usage?.total_cost_usd).toBe(0.012);
    });

    it("transitions to failed with error fields populated", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, turnEndFailed(TURN_1, "rate limited"));
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.status).toBe("failed");
      expect(turn.error).toBe("rate limited");
      expect(turn.error_kind).toBe("harness_error");
    });

    it("does NOT re-terminalize an already-complete turn", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, turnEndCompleted(TURN_1));
      turns = reduce(turns, turnEndFailed(TURN_1, "late failure"));
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.status).toBe("complete");
      expect(turn.error).toBeUndefined();
    });
  });

  describe("heartbeat_timeout", () => {
    it("transitions a streaming turn to failed with adapter_failure kind", () => {
      let turns = reduce([], turnStart(TURN_1));
      const ev: ReducerInput = {
        type: "heartbeat_timeout",
        turn_id: TURN_1,
        at: "2026-05-16T00:01:00Z",
      };
      turns = reduce(turns, ev);
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.status).toBe("failed");
      expect(turn.error).toBe("no response from harness — retry?");
      expect(turn.error_kind).toBe("adapter_failure");
      expect(turn.ended_at).toBe("2026-05-16T00:01:00Z");
    });
  });

  describe("agent-scoped + unknown events", () => {
    it("ignores rate_limit_event (transcript doesn't change)", () => {
      const prev = reduce([], turnStart(TURN_1));
      const next = reduce(prev, {
        type: "rate_limit_event",
        agent_id: AGENT_A,
        info: { primary: { used_percent: 42 } },
      });
      expect(next).toBe(prev);
    });

    it("ignores agent_idle", () => {
      const prev = reduce([], turnStart(TURN_1));
      const next = reduce(prev, { type: "agent_idle", agent_id: AGENT_A });
      expect(next).toBe(prev);
    });

    it("ignores unknown wire-format variants without crashing", () => {
      const prev = reduce([], turnStart(TURN_1));
      // Cast to bypass TS exhaustiveness — simulating a future Rust release
      // adding a variant the frontend hasn't been rebuilt for.
      const future = { type: "future_variant", payload: {} } as unknown as NormalizedEvent;
      const next = reduce(prev, future);
      expect(next).toBe(prev);
    });
  });

  describe("purity — no new Date() inside reducer", () => {
    it("uses the threaded receivedAt for tool started_at (deterministic)", () => {
      const fixed = "2026-05-16T12:34:56.789Z";
      let turns = transcriptReducer([], turnStart(TURN_1), AGENT_A, fixed);
      turns = transcriptReducer(
        turns,
        {
          type: "tool_started",
          turn_id: TURN_1,
          tool_use_id: "tool-1",
          kind: "builtin",
          name: "Bash",
          input: {},
        },
        AGENT_A,
        fixed,
      );
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      const tool = turn.items[0];
      if (tool?.item_kind !== "tool") throw new Error("unreachable");
      // Exact match — reducer must not have called new Date() internally.
      expect(tool.started_at).toBe(fixed);
    });
  });
});

describe("runtimeReducer", () => {
  function fresh(): AgentRuntime {
    return freshRuntime(AGENT_A);
  }

  it("freshRuntime initializes with run_status=idle, hydration_status=complete", () => {
    const r = fresh();
    expect(r.run_status).toBe("idle");
    // M2.5 default: nothing to hydrate. M2.6 introduces "loading" at project open.
    expect(r.hydration_status).toBe("complete");
    expect(r.in_flight_turn_id).toBeUndefined();
    expect(r.last_error).toBeUndefined();
  });

  it("turn_start → processing + sets in_flight_turn_id + clears last_error", () => {
    const r = runtimeReducer(
      { ...fresh(), last_error: { message: "old", kind: "harness_error" } },
      turnStart(TURN_1),
    );
    expect(r.run_status).toBe("processing");
    expect(r.in_flight_turn_id).toBe(TURN_1);
    expect(r.last_error).toBeUndefined();
  });

  it("turn_end (completed) does NOT flip run_status to idle (waits for AgentIdle)", () => {
    // Load-bearing for Codex: post-TurnEnd enrichment events flow on the
    // channel and the dispatcher is still InFlight. Send must remain
    // gated until AgentIdle arrives.
    let r = runtimeReducer(fresh(), turnStart(TURN_1));
    r = runtimeReducer(r, turnEndCompleted(TURN_1));
    expect(r.run_status).toBe("processing");
  });

  it("turn_end (failed) records last_error but keeps run_status=processing", () => {
    let r = runtimeReducer(fresh(), turnStart(TURN_1));
    r = runtimeReducer(r, turnEndFailed(TURN_1, "boom"));
    expect(r.run_status).toBe("processing");
    expect(r.last_error).toEqual({ message: "boom", kind: "harness_error" });
  });

  it("agent_idle → run_status=idle + clears in_flight_turn_id", () => {
    let r = runtimeReducer(fresh(), turnStart(TURN_1));
    r = runtimeReducer(r, { type: "agent_idle", agent_id: AGENT_A });
    expect(r.run_status).toBe("idle");
    expect(r.in_flight_turn_id).toBeUndefined();
  });

  it("agent_idle after failed turn preserves last_error (sendability ≠ health)", () => {
    let r = runtimeReducer(fresh(), turnStart(TURN_1));
    r = runtimeReducer(r, turnEndFailed(TURN_1, "boom"));
    r = runtimeReducer(r, { type: "agent_idle", agent_id: AGENT_A });
    expect(r.run_status).toBe("idle"); // sendable
    expect(r.last_error?.message).toBe("boom"); // last failure still surfaced
  });

  it("agent_idle while starting is a no-op (guarded transition)", () => {
    // A stray agent_idle in the starting window would race the sendability
    // gate back open before the legitimate TurnStart arrived. The only
    // legal path out of "starting" without going through "processing" is
    // failSendStart (a state-module action, not an event). The reducer's
    // agent_idle handler must guard against this.
    const starting: AgentRuntime = { ...fresh(), run_status: "starting" };
    const r = runtimeReducer(starting, { type: "agent_idle", agent_id: AGENT_A });
    expect(r).toBe(starting); // no-op: same reference returned
    expect(r.run_status).toBe("starting");
  });

  it("agent_idle while idle is a no-op (no spurious state churn)", () => {
    const idle = fresh();
    const r = runtimeReducer(idle, { type: "agent_idle", agent_id: AGENT_A });
    expect(r).toBe(idle);
  });

  it("heartbeat_timeout records adapter_failure + clears in_flight_turn_id", () => {
    let r = runtimeReducer(fresh(), turnStart(TURN_1));
    r = runtimeReducer(r, {
      type: "heartbeat_timeout",
      turn_id: TURN_1,
      at: "2026-05-16T00:01:00Z",
    });
    expect(r.last_error?.kind).toBe("adapter_failure");
    expect(r.in_flight_turn_id).toBeUndefined();
    // run_status stays "processing" — the backend hasn't released the
    // guard yet; AgentIdle arrives later when the drain task ends.
    expect(r.run_status).toBe("processing");
  });

  it("session_meta populates meta", () => {
    const ev: NormalizedEvent = {
      type: "session_meta",
      agent_id: AGENT_A,
      model: "claude-sonnet-4-6",
      harness_version: "2.1.140",
      tools: ["Bash", "Read"],
      mcp_servers: [{ name: "tiddly", status: "connected" }],
      skills: ["debug"],
      raw: {},
    };
    const r = runtimeReducer(fresh(), ev);
    expect(r.meta?.model).toBe("claude-sonnet-4-6");
    expect(r.meta?.tools).toEqual(["Bash", "Read"]);
    expect(r.meta?.mcp_servers).toEqual([{ name: "tiddly", status: "connected" }]);
  });

  it("rate_limit_event populates last_rate_limit", () => {
    const r = runtimeReducer(fresh(), {
      type: "rate_limit_event",
      agent_id: AGENT_A,
      info: { primary: { used_percent: 42.0 } },
    });
    expect(r.last_rate_limit).toEqual({ primary: { used_percent: 42.0 } });
  });

  it("ignores unknown wire-format variants without crashing", () => {
    const prev = fresh();
    const future = { type: "future_variant" } as unknown as NormalizedEvent;
    expect(runtimeReducer(prev, future)).toBe(prev);
  });
});

describe("_internal.appendUserTurn", () => {
  it("synthesizes a user-role turn with caller-provided id/text/timestamp", () => {
    const turns = _internal.appendUserTurn(
      [],
      AGENT_A,
      "user-1",
      "hi there",
      "2026-05-16T00:00:00Z",
    );
    expect(turns).toHaveLength(1);
    expect(turns[0]).toEqual({
      role: "user",
      turn_id: "user-1",
      agent_id: AGENT_A,
      started_at: "2026-05-16T00:00:00Z",
      text: "hi there",
    });
  });

  it("preserves prior turns (immutable append)", () => {
    const prev: Turn[] = [
      {
        role: "agent",
        turn_id: TURN_1,
        agent_id: AGENT_A,
        started_at: "2026-05-16T00:00:00Z",
        status: "complete",
        ended_at: "2026-05-16T00:00:05Z",
        items: [{ item_kind: "text", kind: "text", text: "ack" }],
      },
    ];
    const next = _internal.appendUserTurn(
      prev,
      AGENT_A,
      "user-1",
      "follow-up",
      "2026-05-16T00:00:10Z",
    );
    expect(next).not.toBe(prev);
    expect(next).toHaveLength(2);
    expect(next[0]).toBe(prev[0]); // first element same reference
  });
});

describe("cross-agent isolation", () => {
  it("turn_start for one agent does not affect another agent's turns", () => {
    const turnsA = reduce([], turnStart(TURN_1), AGENT_A);
    const turnsB = reduce([], turnStart(TURN_2), AGENT_B);
    expect(turnsA).toHaveLength(1);
    expect(turnsB).toHaveLength(1);
    expect((turnsA[0] as Extract<Turn, { role: "agent" }>).agent_id).toBe(AGENT_A);
    expect((turnsB[0] as Extract<Turn, { role: "agent" }>).agent_id).toBe(AGENT_B);
  });
});
