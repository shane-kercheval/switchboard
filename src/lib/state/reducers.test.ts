import { describe, expect, it } from "vitest";
import type { NormalizedEvent, ReducerInput } from "$lib/types";
import { _internal, freshRuntime, runtimeReducer, transcriptReducer } from "./reducers";
import { buildUnifiedRows } from "./unified";
import type { AgentRuntime, TextChunk, ToolCall, Turn } from "./types";

const AGENT_A = "00000000-0000-7000-8000-000000000aaa";
const AGENT_B = "00000000-0000-7000-8000-000000000bbb";
const TURN_1 = "00000000-0000-7000-8000-000000000001";
const TURN_2 = "00000000-0000-7000-8000-000000000002";
const MESSAGE_1 = "00000000-0000-7000-8000-0000000000f1";
const MESSAGE_2 = "00000000-0000-7000-8000-0000000000f2";

// Fixed timestamp used as `receivedAt` across all transcriptReducer
// invocations in tests. Pinning a constant makes tool-event ordering and
// timestamp assertions deterministic.
const RECEIVED_AT = "2026-05-16T00:00:00Z";

function turnStart(
  turnId: string,
  messageId = MESSAGE_1,
  startedAt = "2026-05-16T00:00:00Z",
): NormalizedEvent {
  return {
    type: "turn_start",
    turn_id: turnId,
    message_id: messageId,
    send_id: messageId,
    started_at: startedAt,
  };
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

function reduce(
  turns: Turn[],
  input: ReducerInput,
  agentId: string = AGENT_A,
  inFlightTurnId?: string,
): Turn[] {
  return transcriptReducer(turns, input, agentId, RECEIVED_AT, undefined, inFlightTurnId);
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
    it("coalesces adjacent same-kind text chunks into one TextChunk item", () => {
      // Real Claude streaming arrives in many small chunks per content_chunk
      // event. Coalescing means the renderer produces one paragraph <div>,
      // not N separately-rendered single-line <div>s.
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, contentChunk(TURN_1, "hello "));
      turns = reduce(turns, contentChunk(TURN_1, "world"));
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.items).toEqual([{ item_kind: "text", kind: "text", text: "hello world" }]);
    });

    it("does NOT coalesce across a tool item — preserves text/tool/text ordering", () => {
      // The interleaving contract: a tool between two text runs sits at
      // its own index. The two text runs stay as separate items even
      // though they're the same kind, because they're not adjacent.
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, contentChunk(TURN_1, "before "));
      turns = reduce(turns, {
        type: "tool_started",
        turn_id: TURN_1,
        tool_use_id: "tool-1",
        kind: "builtin",
        name: "Bash",
        input: {},
      });
      turns = reduce(turns, contentChunk(TURN_1, "after"));
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.items).toHaveLength(3);
      expect(turn.items[0]).toMatchObject({ item_kind: "text", text: "before " });
      expect(turn.items[1]).toMatchObject({ item_kind: "tool", tool_use_id: "tool-1" });
      expect(turn.items[2]).toMatchObject({ item_kind: "text", text: "after" });
    });

    it("does NOT coalesce across different ContentKind (text vs thinking)", () => {
      // Future reasoning rendering will use kind: "thinking".
      // Coalescing would silently fold a thinking block into a
      // preceding text chunk, breaking reasoning-aware UI.
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, { type: "content_chunk", turn_id: TURN_1, kind: "text", text: "hi" });
      turns = reduce(turns, {
        type: "content_chunk",
        turn_id: TURN_1,
        kind: "thinking",
        text: "ponder",
      });
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.items).toHaveLength(2);
      expect(turn.items[0]).toMatchObject({ kind: "text" });
      expect(turn.items[1]).toMatchObject({ kind: "thinking" });
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

  describe("message_cancelled", () => {
    it("appends a cancelled agent turn stamped with the resolved send_id", () => {
      const turns = transcriptReducer(
        [],
        { type: "message_cancelled", message_id: MESSAGE_1, agent_id: AGENT_A, at: RECEIVED_AT },
        AGENT_A,
        RECEIVED_AT,
        "send-1",
      );
      expect(turns).toHaveLength(1);
      const t = turns[0];
      expect(t?.role).toBe("agent");
      if (t?.role !== "agent") throw new Error("unreachable");
      expect(t.status).toBe("cancelled");
      expect(t.send_id).toBe("send-1");
    });

    it("is a no-op when no send_id resolves (stray event)", () => {
      const turns = transcriptReducer(
        [],
        { type: "message_cancelled", message_id: MESSAGE_1, agent_id: AGENT_A, at: RECEIVED_AT },
        AGENT_A,
        RECEIVED_AT,
        undefined,
      );
      expect(turns).toEqual([]);
    });
  });

  describe("message_failed", () => {
    it("appends a failed agent turn carrying the error, stamped with the resolved send_id", () => {
      const turns = transcriptReducer(
        [],
        {
          type: "message_failed",
          message_id: MESSAGE_1,
          agent_id: AGENT_A,
          error: "adapter failed to launch",
          at: RECEIVED_AT,
        },
        AGENT_A,
        RECEIVED_AT,
        "send-1",
      );
      expect(turns).toHaveLength(1);
      const t = turns[0];
      if (t?.role !== "agent") throw new Error("unreachable");
      expect(t.status).toBe("failed");
      expect(t.error).toBe("adapter failed to launch");
      expect(t.error_kind).toBe("adapter_failure");
      expect(t.send_id).toBe("send-1");
    });

    it("is a no-op when no send_id resolves (post-start failure renders on the live turn)", () => {
      const turns = transcriptReducer(
        [],
        {
          type: "message_failed",
          message_id: MESSAGE_1,
          agent_id: AGENT_A,
          error: "boom",
          at: RECEIVED_AT,
        },
        AGENT_A,
        RECEIVED_AT,
        undefined,
      );
      expect(turns).toEqual([]);
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

    it("records the per-turn spend signal from turn_end", () => {
      let turns = reduce([], turnStart(TURN_1));
      const ev: NormalizedEvent = {
        type: "turn_end",
        turn_id: TURN_1,
        outcome: { status: "completed" },
        ended_at: "2026-05-16T00:00:05Z",
        usage: { input_tokens: 10, output_tokens: 5, total_cost_usd: 0.0125 },
        spend: { real_spend: true, is_overage: true, overage_resets_at: null },
      };
      turns = reduce(turns, ev);
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.spend?.real_spend).toBe(true);
      expect(turn.spend?.is_overage).toBe(true);
    });

    it("renders a synthesized truncation terminal as a failed turn", () => {
      // The dispatcher now synthesizes a `failed` turn_end (AdapterFailure) when
      // a stream ends with no terminal — for compose-bar sends too, not only
      // awaited ones. It is indistinguishable from any failed terminal at the
      // reducer, so the streaming turn transitions to `failed` and shows the
      // error rather than spinning forever as it did under the old warn-and-idle.
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, contentChunk(TURN_1, "partial output"));
      turns = reduce(turns, turnEndFailed(TURN_1, "turn stream ended without a terminal event"));
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.status).toBe("failed");
      expect(turn.error).toBe("turn stream ended without a terminal event");
    });

    it("stamps per-turn model + effort from turn_end (live carrier)", () => {
      // Proves the footer populates during streaming, not only on reopen.
      let turns = reduce([], turnStart(TURN_1));
      const ev: NormalizedEvent = {
        type: "turn_end",
        turn_id: TURN_1,
        outcome: { status: "completed" },
        ended_at: "2026-05-16T00:00:05Z",
        model: "gpt-5.5",
        effort: "high",
      };
      turns = reduce(turns, ev);
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.model).toBe("gpt-5.5");
      expect(turn.effort).toBe("high");
    });

    it("leaves model/effort undefined when turn_end omits them", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, {
        type: "turn_end",
        turn_id: TURN_1,
        outcome: { status: "completed" },
        ended_at: "2026-05-16T00:00:05Z",
      });
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.model).toBeUndefined();
      expect(turn.effort).toBeUndefined();
    });

    it("stamps model/effort on a FAILED turn (which model did it fail on)", () => {
      // The plan's motivating case: turn_context is written at turn start, so a
      // failed/interrupted turn still carries the model+effort it attempted.
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, {
        type: "turn_end",
        turn_id: TURN_1,
        outcome: { status: "failed", kind: "harness_error", message: "boom" },
        ended_at: "2026-05-16T00:00:05Z",
        model: "gpt-5.5",
        effort: "high",
      });
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.status).toBe("failed");
      expect(turn.model).toBe("gpt-5.5");
      expect(turn.effort).toBe("high");
    });

    it("stamps model/effort on a CANCELLED turn", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, {
        type: "turn_end",
        turn_id: TURN_1,
        outcome: { status: "cancelled", source: "user" },
        ended_at: "2026-05-16T00:00:05Z",
        model: "gpt-5.5",
        effort: "medium",
      });
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.status).toBe("cancelled");
      expect(turn.model).toBe("gpt-5.5");
      expect(turn.effort).toBe("medium");
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

    it("transitions to cancelled (distinct from failed, no error fields)", () => {
      let turns = reduce([], turnStart(TURN_1));
      const ev: NormalizedEvent = {
        type: "turn_end",
        turn_id: TURN_1,
        outcome: { status: "cancelled", source: "user" },
        ended_at: "2026-05-16T00:00:05Z",
      };
      turns = reduce(turns, ev);
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      expect(turn.status).toBe("cancelled");
      expect(turn.ended_at).toBe("2026-05-16T00:00:05Z");
      expect(turn.error).toBeUndefined();
      expect(turn.error_kind).toBeUndefined();
    });

    it("marks only still-running tools as cancelled when the turn is cancelled", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, {
        type: "tool_started",
        turn_id: TURN_1,
        tool_use_id: "tool-done",
        kind: "builtin",
        name: "Bash",
        input: { command: "true" },
      });
      turns = reduce(turns, {
        type: "tool_completed",
        turn_id: TURN_1,
        tool_use_id: "tool-done",
        output: "ok",
        is_error: false,
      });
      turns = reduce(turns, {
        type: "tool_started",
        turn_id: TURN_1,
        tool_use_id: "tool-pending",
        kind: "builtin",
        name: "Bash",
        input: { command: "sleep 10" },
      });
      turns = reduce(turns, {
        type: "turn_end",
        turn_id: TURN_1,
        outcome: { status: "cancelled", source: "user" },
        ended_at: "2026-05-16T00:00:05Z",
      });

      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      const done = turn.items.find(
        (item): item is ToolCall => item.item_kind === "tool" && item.tool_use_id === "tool-done",
      );
      const pending = turn.items.find(
        (item): item is ToolCall =>
          item.item_kind === "tool" && item.tool_use_id === "tool-pending",
      );
      expect(done?.completed_at).toBe(RECEIVED_AT);
      expect(done?.stopped_at).toBeUndefined();
      expect(done?.stop_reason).toBeUndefined();
      expect(pending?.completed_at).toBeUndefined();
      expect(pending?.stopped_at).toBe("2026-05-16T00:00:05Z");
      expect(pending?.stop_reason).toBe("cancelled");
    });

    it("marks only still-running tools as failed when the turn fails", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, {
        type: "tool_started",
        turn_id: TURN_1,
        tool_use_id: "tool-done",
        kind: "builtin",
        name: "Bash",
        input: { command: "true" },
      });
      turns = reduce(turns, {
        type: "tool_completed",
        turn_id: TURN_1,
        tool_use_id: "tool-done",
        output: "ok",
        is_error: false,
      });
      turns = reduce(turns, {
        type: "tool_started",
        turn_id: TURN_1,
        tool_use_id: "tool-pending",
        kind: "builtin",
        name: "Bash",
        input: { command: "sleep 10" },
      });
      turns = reduce(turns, turnEndFailed(TURN_1, "adapter crashed"));

      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      const done = turn.items.find(
        (item): item is ToolCall => item.item_kind === "tool" && item.tool_use_id === "tool-done",
      );
      const pending = turn.items.find(
        (item): item is ToolCall =>
          item.item_kind === "tool" && item.tool_use_id === "tool-pending",
      );
      expect(done?.completed_at).toBe(RECEIVED_AT);
      expect(done?.stopped_at).toBeUndefined();
      expect(done?.stop_reason).toBeUndefined();
      expect(pending?.completed_at).toBeUndefined();
      expect(pending?.stopped_at).toBe("2026-05-16T00:00:05Z");
      expect(pending?.stop_reason).toBe("failed");
    });
  });

  describe("heartbeat_timeout", () => {
    it("does NOT touch the transcript — a silent turn is not failed", () => {
      let turns = reduce([], turnStart(TURN_1));
      const ev: ReducerInput = {
        type: "heartbeat_timeout",
        turn_id: TURN_1,
        at: "2026-05-16T00:01:00Z",
      };
      turns = reduce(turns, ev);
      const turn = turns[0];
      if (turn?.role !== "agent") throw new Error("unreachable");
      // The turn stays streaming; silence is surfaced via the runtime `quiet`
      // flag (see runtimeReducer), not by failing the transcript turn.
      expect(turn.status).toBe("streaming");
      expect(turn.error).toBeUndefined();
      expect(turn.ended_at).toBeUndefined();
    });
  });

  describe("hydrate", () => {
    it("appends hydrated turns to an empty transcript in disk order", () => {
      const turns = reduce([], {
        type: "hydrate",
        agent_id: AGENT_A,
        turns: [
          {
            role: "user",
            turn_id: TURN_1,
            agent_id: AGENT_A,
            started_at: "2026-05-14T00:00:01Z",
            text: "say 1",
          },
          {
            role: "agent",
            turn_id: TURN_2,
            agent_id: AGENT_A,
            started_at: "2026-05-14T00:00:02Z",
            status: "complete",
            items: [{ item_kind: "text", kind: "text", text: "1" }],
          },
        ],
      });
      expect(turns).toHaveLength(2);
      expect(turns[0]?.role).toBe("user");
      expect(turns[1]?.role).toBe("agent");
    });

    it("carries a hydrated turn's persisted spend onto the state turn", () => {
      // Reopen join: a LoadedTurn re-joined from the turn-metadata sidecar
      // carries `spend`; the reducer must surface it so the inline cost +
      // overage marker re-render after reopen (not just live).
      const turns = reduce([], {
        type: "hydrate",
        agent_id: AGENT_A,
        turns: [
          {
            role: "agent",
            turn_id: TURN_1,
            agent_id: AGENT_A,
            started_at: "2026-05-14T00:00:02Z",
            status: "complete",
            items: [{ item_kind: "text", kind: "text", text: "1" }],
            usage: {
              input_tokens: 10,
              output_tokens: 5,
              total_cost_usd: 0.0125,
            },
            spend: { real_spend: true, is_overage: true, overage_resets_at: null },
          },
        ],
      });
      const turn = turns[0];
      expect(turn?.role).toBe("agent");
      if (turn?.role === "agent") {
        expect(turn.spend?.real_spend).toBe(true);
        expect(turn.spend?.is_overage).toBe(true);
        expect(turn.usage?.total_cost_usd).toBe(0.0125);
      }
    });

    it("leaves spend undefined for a hydrated turn with no persisted record", () => {
      // Pre-feature / normal-quota turn: no `spend` on the LoadedTurn → the
      // state turn renders neither cost nor marker.
      const turns = reduce([], {
        type: "hydrate",
        agent_id: AGENT_A,
        turns: [
          {
            role: "agent",
            turn_id: TURN_1,
            agent_id: AGENT_A,
            started_at: "2026-05-14T00:00:02Z",
            status: "complete",
            items: [{ item_kind: "text", kind: "text", text: "1" }],
          },
        ],
      });
      const turn = turns[0];
      expect(turn?.role).toBe("agent");
      if (turn?.role === "agent") {
        expect(turn.spend).toBeUndefined();
      }
    });

    it("preserves a live in-flight turn when hydrate carries the same turn_id", () => {
      // Live turn lands first.
      const livePrior = reduce([], turnStart(TURN_1));
      // Hydrate carries a different (older) value for the SAME turn_id.
      // The live state must win.
      const hydrated = reduce(livePrior, {
        type: "hydrate",
        agent_id: AGENT_A,
        turns: [
          {
            role: "agent",
            turn_id: TURN_1,
            agent_id: AGENT_A,
            started_at: "1999-01-01T00:00:00Z",
            status: "complete",
            items: [{ item_kind: "text", kind: "text", text: "stale" }],
          },
        ],
      });
      // The live turn keeps its "streaming" status and its (empty) items.
      const turn = hydrated.find((t) => t.turn_id === TURN_1);
      expect(turn?.role).toBe("agent");
      if (turn?.role === "agent") {
        expect(turn.status).toBe("streaming");
        expect(turn.items).toHaveLength(0);
      }
    });

    it("merges hydrated turns alongside live ones without duplicating", () => {
      // Two live turns already present.
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, turnStart(TURN_2));
      const hydrated = reduce(turns, {
        type: "hydrate",
        agent_id: AGENT_A,
        turns: [
          {
            role: "user",
            turn_id: "00000000-0000-7000-8000-000000000003",
            agent_id: AGENT_A,
            started_at: "2026-05-14T00:00:01Z",
            text: "older prompt",
          },
        ],
      });
      expect(hydrated).toHaveLength(3);
    });

    it("translates LoadedTurn::Tool items into ToolCall state items", () => {
      const turns = reduce([], {
        type: "hydrate",
        agent_id: AGENT_A,
        turns: [
          {
            role: "agent",
            turn_id: TURN_1,
            agent_id: AGENT_A,
            started_at: "2026-05-14T00:00:01Z",
            status: "complete",
            items: [
              {
                item_kind: "tool",
                tool_use_id: "t1",
                kind: "builtin",
                name: "Bash",
                input: { command: "ls" },
                output: "ok",
                is_error: false,
                started_at: "2026-05-14T00:00:01Z",
                completed_at: "2026-05-14T00:00:02Z",
              },
            ],
          },
        ],
      });
      const turn = turns[0];
      if (turn === undefined || turn.role !== "agent") throw new Error("expected agent turn");
      expect(turn.items).toHaveLength(1);
      const item = turn.items[0];
      if (item === undefined || item.item_kind !== "tool") throw new Error("expected tool item");
      expect(item.tool_use_id).toBe("t1");
      expect(item.output).toBe("ok");
      expect(item.is_error).toBe(false);
    });

    it("re-applying the same batch re-parsed (fresh turn_ids, same hydration_key) does not duplicate", () => {
      // Re-reading a session file mints fresh `turn_id`s but the stable
      // `hydration_key` is parse-invariant — the merge must recognize the turn
      // and not append a second copy. This is the core M2 idempotency guard.
      const diskTurn = (turnId: string) => ({
        type: "hydrate" as const,
        agent_id: AGENT_A,
        turns: [
          {
            role: "agent" as const,
            turn_id: turnId,
            agent_id: AGENT_A,
            started_at: "2026-05-14T00:00:02Z",
            status: "complete" as const,
            items: [{ item_kind: "text" as const, kind: "text" as const, text: "answer" }],
            hydration_key: "msg_stable01",
          },
        ],
      });
      let turns = reduce([], diskTurn(TURN_1));
      expect(turns).toHaveLength(1);
      // Second parse: different turn_id, SAME hydration_key.
      turns = reduce(turns, diskTurn(TURN_2));
      expect(turns).toHaveLength(1);
      expect(turns[0]?.turn_id).toBe(TURN_1); // the first-loaded turn is kept
    });

    it("a live completed turn and the same turn from disk (matching hydration_key) merge to one, live preserved", () => {
      // Live turn streams and completes, carrying the live-matched key.
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, {
        type: "turn_end",
        turn_id: TURN_1,
        outcome: { status: "completed" },
        ended_at: "2026-05-16T00:00:05Z",
        hydration_key: "msg_live01",
      });
      expect(turns).toHaveLength(1);
      // Disk re-read of the same turn: different turn_id, same hydration_key.
      const merged = reduce(turns, {
        type: "hydrate",
        agent_id: AGENT_A,
        turns: [
          {
            role: "agent",
            turn_id: TURN_2,
            agent_id: AGENT_A,
            started_at: "2026-05-16T00:00:00Z",
            status: "complete",
            items: [{ item_kind: "text", kind: "text", text: "from disk" }],
            hydration_key: "msg_live01",
          },
        ],
      });
      expect(merged).toHaveLength(1);
      // The live turn (its turn_id) is the one kept — disk never overwrites it.
      expect(merged[0]?.turn_id).toBe(TURN_1);
    });

    it("turns lacking a hydration_key still dedup via the turn_id fallback", () => {
      // Antigravity (and any keyless turn) carries no hydration_key — the merge
      // must fall back to turn_id so a live turn isn't duplicated by its disk copy.
      const livePrior = reduce([], turnStart(TURN_1));
      const merged = reduce(livePrior, {
        type: "hydrate",
        agent_id: AGENT_A,
        turns: [
          {
            role: "agent",
            turn_id: TURN_1,
            agent_id: AGENT_A,
            started_at: "2026-05-16T00:00:00Z",
            status: "complete",
            items: [{ item_kind: "text", kind: "text", text: "from disk" }],
            // no hydration_key
          },
        ],
      });
      expect(merged).toHaveLength(1);
      expect(merged[0]?.turn_id).toBe(TURN_1);
    });
  });

  describe("hydrate — completeness-ranked supersession", () => {
    const TURN_3 = "00000000-0000-7000-8000-000000000003";
    const streamingPartial = (turnId: string, key: string): ReducerInput => ({
      type: "hydrate",
      agent_id: AGENT_A,
      turns: [
        {
          role: "agent",
          turn_id: turnId,
          agent_id: AGENT_A,
          started_at: "2026-05-14T00:00:02Z",
          status: "streaming",
          items: [{ item_kind: "text", kind: "text", text: "working" }],
          hydration_key: key,
        },
      ],
    });
    const completed = (turnId: string, key: string): ReducerInput => ({
      type: "hydrate",
      agent_id: AGENT_A,
      turns: [
        {
          role: "agent",
          turn_id: turnId,
          agent_id: AGENT_A,
          started_at: "2026-05-14T00:00:02Z",
          status: "complete",
          items: [{ item_kind: "text", kind: "text", text: "done" }],
          hydration_key: key,
        },
      ],
    });

    it("a completed disk turn supersedes a stranded Streaming partial of the same key", () => {
      // The reload/switch-back bug: a re-read caught the turn mid-flight
      // (Streaming), then a later re-read sees it finished. The complete turn
      // must REPLACE the partial, not stack beside it as a stuck spinner.
      let turns = reduce([], streamingPartial(TURN_1, "msg_x"));
      expect(turns).toHaveLength(1);
      expect(turns[0]).toMatchObject({ role: "agent", status: "streaming" });

      turns = reduce(turns, completed(TURN_2, "msg_x"));
      expect(turns).toHaveLength(1);
      const turn = turns[0];
      expect(turn?.role).toBe("agent");
      if (turn?.role === "agent") {
        expect(turn.status).toBe("complete");
        expect(turn.turn_id).toBe(TURN_2);
      }
    });

    it("two Streaming turns with the same key collapse to one, resident kept", () => {
      // The non-supersession branch made explicit: neither side is terminal, so
      // the resident (the earlier snapshot) wins and the count stays one.
      let turns = reduce([], streamingPartial(TURN_1, "msg_x"));
      turns = reduce(turns, streamingPartial(TURN_2, "msg_x"));
      expect(turns).toHaveLength(1);
      const turn = turns[0];
      if (turn?.role === "agent") {
        expect(turn.status).toBe("streaming");
        expect(turn.turn_id).toBe(TURN_1); // resident kept, not replaced
      }
    });

    it("a Streaming disk turn does NOT supersede a Complete resident of the same key", () => {
      let turns = reduce([], completed(TURN_1, "msg_x"));
      turns = reduce(turns, streamingPartial(TURN_2, "msg_x"));
      expect(turns).toHaveLength(1);
      const turn = turns[0];
      if (turn?.role === "agent") {
        expect(turn.status).toBe("complete");
        expect(turn.turn_id).toBe(TURN_1); // resident kept
      }
    });

    it("superseding preserves the resident's live-only fields the disk turn lacks", () => {
      // Hand-built Streaming resident carrying a live-only `spend` (a disk parse
      // can't recover it). The completed disk turn that supersedes it has none;
      // the merge must carry the resident's spend forward, not blank it.
      const residentWithSpend: Turn = {
        role: "agent",
        turn_id: TURN_1,
        agent_id: AGENT_A,
        started_at: "2026-05-14T00:00:02Z",
        status: "streaming",
        items: [{ item_kind: "text", kind: "text", text: "working" }],
        spend: { real_spend: true, is_overage: true, overage_resets_at: null },
        hydration_key: "msg_x",
      };
      const merged = reduce([residentWithSpend], completed(TURN_2, "msg_x"));
      expect(merged).toHaveLength(1);
      const turn = merged[0];
      if (turn?.role === "agent") {
        expect(turn.status).toBe("complete");
        expect(turn.spend?.real_spend).toBe(true);
      }
    });

    it("repeated re-hydrations of an advancing in-flight turn stay one row (switch-back)", () => {
      // The maybeRefreshProject path: each switch-back re-reads the growing file
      // and catches a later Streaming snapshot of the SAME turn (same key, fresh
      // turn_id per parse). With first-id identity these all share a key, so
      // they collapse to one row instead of accumulating one block per switch.
      let turns = reduce([], streamingPartial(TURN_1, "msg_x"));
      turns = reduce(turns, streamingPartial(TURN_2, "msg_x"));
      turns = reduce(turns, streamingPartial(TURN_3, "msg_x"));
      expect(turns).toHaveLength(1);

      turns = reduce(turns, completed(TURN_2, "msg_x"));
      expect(turns).toHaveLength(1);
      if (turns[0]?.role === "agent") expect(turns[0].status).toBe("complete");
    });

    it("the superseded slice renders exactly one agent row, complete (no stuck spinner)", () => {
      // Bridge to the render layer: after the mid-flight→complete sequence, the
      // unified view must show one agent row in the `complete` state.
      let turns = reduce([], streamingPartial(TURN_1, "msg_x"));
      turns = reduce(turns, completed(TURN_2, "msg_x"));
      const rows = buildUnifiedRows(turns, []);
      const agentRows = rows.filter((r) => r.kind === "agent");
      expect(agentRows).toHaveLength(1);
      expect(agentRows[0]).toMatchObject({ kind: "agent" });
      if (agentRows[0]?.kind === "agent") {
        expect(agentRows[0].turn.status).toBe("complete");
      }
    });
  });

  describe("turn_identity — early live identity", () => {
    const identity = (turnId: string, key: string): NormalizedEvent => ({
      type: "turn_identity",
      turn_id: turnId,
      hydration_key: key,
    });
    const diskComplete = (turnId: string, key: string): ReducerInput => ({
      type: "hydrate",
      agent_id: AGENT_A,
      turns: [
        {
          role: "agent",
          turn_id: turnId,
          agent_id: AGENT_A,
          started_at: "2026-05-14T00:00:02Z",
          status: "complete",
          items: [{ item_kind: "text", kind: "text", text: "from disk" }],
          hydration_key: key,
        },
      ],
    });

    it("stamps hydration_key onto the live in-flight turn", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, identity(TURN_1, "msg_x"));
      const turn = turns.find((t) => t.turn_id === TURN_1);
      expect(turn?.role).toBe("agent");
      if (turn?.role === "agent") {
        expect(turn.hydration_key).toBe("msg_x");
        expect(turn.status).toBe("streaming"); // still live, just identified
      }
    });

    it("collapses a disk copy that raced ahead of the identity event into the live turn", () => {
      // A refresh inserted a disk copy (key msg_x) while the live turn was still
      // keyless, so the two briefly coexist (no key to match on).
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, diskComplete(TURN_2, "msg_x"));
      expect(turns).toHaveLength(2);
      // The anchor event stamps the live turn and collapses the disk copy in.
      turns = reduce(turns, identity(TURN_1, "msg_x"));
      expect(turns).toHaveLength(1);
      const turn = turns[0];
      if (turn?.role === "agent") {
        expect(turn.turn_id).toBe(TURN_1); // the live turn survives
        expect(turn.hydration_key).toBe("msg_x");
        expect(turn.status).toBe("streaming"); // live stream stays authoritative
      }
    });

    it("a terminal disk re-read does NOT supersede an actively-streaming live turn", () => {
      // The hazard early identity introduces: once the live turn carries an early key, a
      // refresh that catches the turn "complete" on disk in the brief window
      // before the live turn_end is processed must NOT replace it (that would
      // orphan the remaining live events).
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, identity(TURN_1, "msg_x")); // live, key msg_x, streaming
      turns = reduce(turns, diskComplete(TURN_2, "msg_x"), AGENT_A, TURN_1 /* in-flight */);
      expect(turns).toHaveLength(1);
      const turn = turns[0];
      if (turn?.role === "agent") {
        expect(turn.turn_id).toBe(TURN_1); // preserved, not superseded
        expect(turn.status).toBe("streaming");
      }
      // The subsequent live turn_end still lands on the live turn (not dropped).
      turns = reduce(turns, turnEndCompleted(TURN_1));
      const ended = turns.find((t) => t.turn_id === TURN_1);
      expect(ended?.role).toBe("agent");
      if (ended?.role === "agent") expect(ended.status).toBe("complete");
    });

    it("re-delivered turn_identity is a no-op (idempotent)", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, identity(TURN_1, "msg_x"));
      const afterFirst = turns;
      turns = reduce(turns, identity(TURN_1, "msg_x"));
      expect(turns).toBe(afterFirst); // same reference → unchanged
    });

    it("turn_identity for an unknown turn_id is a no-op", () => {
      const turns = reduce([], turnStart(TURN_1));
      const after = reduce(turns, identity(TURN_2, "msg_x"));
      expect(after).toBe(turns);
    });

    const turnEndCancelled = (turnId: string): NormalizedEvent => ({
      type: "turn_end",
      turn_id: turnId,
      outcome: { status: "cancelled", source: "user" },
      ended_at: "2026-05-16T00:00:05Z",
      // no hydration_key — a cancelled turn's TurnEnd is synthesized by the
      // dispatcher and carries none.
    });

    it("turn_end preserves the early key when the terminal carries none", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, identity(TURN_1, "msg_x"));
      turns = reduce(turns, turnEndCompleted(TURN_1)); // helper omits hydration_key
      const turn = turns.find((t) => t.turn_id === TURN_1);
      if (turn?.role === "agent") {
        expect(turn.status).toBe("complete");
        expect(turn.hydration_key).toBe("msg_x"); // not wiped by the keyless terminal
      }
    });

    it("a cancelled turn keeps its early key (synthesized terminal carries none)", () => {
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, identity(TURN_1, "msg_x"));
      turns = reduce(turns, turnEndCancelled(TURN_1));
      const turn = turns.find((t) => t.turn_id === TURN_1);
      if (turn?.role === "agent") {
        expect(turn.status).toBe("cancelled");
        expect(turn.hydration_key).toBe("msg_x");
      }
    });

    it("a cancelled turn does not duplicate against its keyed disk copy", () => {
      // End-to-end of the bug: the turn streams (identity), is cancelled, then a
      // switch-back refresh re-reads the keyed disk copy. The preserved key
      // dedups → one row. Without key preservation the cancelled row goes keyless
      // and the disk copy appends → two rows for one turn.
      let turns = reduce([], turnStart(TURN_1));
      turns = reduce(turns, identity(TURN_1, "msg_x"));
      turns = reduce(turns, turnEndCancelled(TURN_1));
      turns = reduce(turns, diskComplete(TURN_2, "msg_x"));
      expect(turns).toHaveLength(1);
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
    // Default for newly-created agents: nothing to hydrate. The hydration
    // flow flips this to "loading" on project open / attach.
    expect(r.hydration_status).toBe("complete");
    expect(r.in_flight_turn_id).toBeUndefined();
    expect(r.last_error).toBeUndefined();
  });

  it("turn_start → processing + sets in_flight_turn_id + clears last_error + consumes the pending send", () => {
    // Correlates the optimistic "starting" send (its pending entry) to its real
    // turn_id; the entry is consumed once the turn starts.
    const r = runtimeReducer(
      {
        ...fresh(),
        run_status: "starting",
        pending_sends: [{ send_id: "send-1", user_turn_id: "user-1", message_id: MESSAGE_1 }],
        last_error: { message: "old", kind: "harness_error" },
      },
      turnStart(TURN_1, MESSAGE_1),
    );
    expect(r.run_status).toBe("processing");
    expect(r.in_flight_turn_id).toBe(TURN_1);
    expect(r.last_error).toBeUndefined();
    expect(r.pending_sends).toBeUndefined();
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

  it("message_failed while starting → idle + records adapter_failure (event-driven failSendStart)", () => {
    // Pre-turn failure for the optimistic send: the dispatcher accepted it
    // (minted MESSAGE_1) but failed before any turn_start. Mirrors the
    // failSendStart action, but triggered by the event.
    const starting: AgentRuntime = {
      ...fresh(),
      run_status: "starting",
      pending_sends: [{ send_id: "send-1", user_turn_id: "user-1", message_id: MESSAGE_1 }],
    };
    const r = runtimeReducer(starting, {
      type: "message_failed",
      message_id: MESSAGE_1,
      agent_id: AGENT_A,
      error: "journal write failed",
      at: "2026-05-16T00:00:00Z",
    });
    expect(r.run_status).toBe("idle");
    expect(r.pending_sends).toBeUndefined();
    expect(r.last_error).toEqual({ message: "journal write failed", kind: "adapter_failure" });
  });

  it("message_failed for a different message_id is a no-op (correlation guard)", () => {
    // The pending send already recorded receipt MESSAGE_1, so a message_failed
    // for MESSAGE_2 correlates to nothing (not the front, since the front isn't
    // pre-receipt) — it must not consume the live pending send.
    const starting: AgentRuntime = {
      ...fresh(),
      run_status: "starting",
      pending_sends: [{ send_id: "send-1", user_turn_id: "user-1", message_id: MESSAGE_1 }],
    };
    const r = runtimeReducer(starting, {
      type: "message_failed",
      message_id: MESSAGE_2,
      agent_id: AGENT_A,
      error: "stale",
      at: "2026-05-16T00:00:00Z",
    });
    expect(r).toBe(starting); // no-op: same reference
  });

  it("message_cancelled while starting → idle, prunes pending, no error (cancel ≠ failure)", () => {
    const starting: AgentRuntime = {
      ...fresh(),
      run_status: "starting",
      pending_sends: [{ send_id: "send-1", user_turn_id: "user-1", message_id: MESSAGE_1 }],
    };
    const r = runtimeReducer(starting, {
      type: "message_cancelled",
      message_id: MESSAGE_1,
      agent_id: AGENT_A,
      at: "2026-05-16T00:00:00Z",
    });
    expect(r.run_status).toBe("idle");
    expect(r.pending_sends).toBeUndefined();
    expect(r.last_error).toBeUndefined();
  });

  it("message_cancelled for a queued send while processing leaves run_status, prunes the entry", () => {
    const processing: AgentRuntime = {
      ...fresh(),
      run_status: "processing",
      pending_sends: [{ send_id: "send-2", user_turn_id: "user-2", message_id: MESSAGE_2 }],
    };
    const r = runtimeReducer(processing, {
      type: "message_cancelled",
      message_id: MESSAGE_2,
      agent_id: AGENT_A,
      at: "2026-05-16T00:00:00Z",
    });
    expect(r.run_status).toBe("processing");
    expect(r.pending_sends).toBeUndefined();
  });

  it("message_cancelled for an unknown message_id is a no-op", () => {
    const starting: AgentRuntime = {
      ...fresh(),
      run_status: "starting",
      pending_sends: [{ send_id: "send-1", user_turn_id: "user-1", message_id: MESSAGE_1 }],
    };
    const r = runtimeReducer(starting, {
      type: "message_cancelled",
      message_id: MESSAGE_2,
      agent_id: AGENT_A,
      at: "2026-05-16T00:00:00Z",
    });
    expect(r).toBe(starting);
  });

  it("message_failed while processing is a no-op (turn already started; must not stomp)", () => {
    // turn_start raced ahead of the (out-of-protocol) message_failed; the
    // turn is live and the failure intuition is wrong.
    let r = runtimeReducer({ ...fresh(), run_status: "starting" }, turnStart(TURN_1, MESSAGE_1));
    expect(r.run_status).toBe("processing");
    r = runtimeReducer(r, {
      type: "message_failed",
      message_id: MESSAGE_1,
      agent_id: AGENT_A,
      error: "ignored",
      at: "2026-05-16T00:00:00Z",
    });
    expect(r.run_status).toBe("processing");
    expect(r.last_error).toBeUndefined();
  });

  it("heartbeat_timeout records quiet_since without failing or clearing in-flight turn", () => {
    let r = runtimeReducer(fresh(), turnStart(TURN_1));
    r = runtimeReducer(r, {
      type: "heartbeat_timeout",
      turn_id: TURN_1,
      at: "2026-05-16T00:01:00Z",
    });
    // quiet_since records when the silence was flagged (the event's `at`).
    expect(r.quiet_since).toBe("2026-05-16T00:01:00Z");
    // No failure: the turn is silent, not dead. in_flight_turn_id and
    // run_status are untouched (the backend still holds the busy-lock).
    expect(r.last_error).toBeUndefined();
    expect(r.in_flight_turn_id).toBe(TURN_1);
    expect(r.run_status).toBe("processing");
  });

  it("heartbeat_timeout for a non-in-flight turn is a no-op", () => {
    let r = runtimeReducer(fresh(), turnStart(TURN_1));
    r = runtimeReducer(r, {
      type: "heartbeat_timeout",
      turn_id: TURN_2,
      at: "2026-05-16T00:01:00Z",
    });
    // The mismatched timeout must not flag quiet (turn_start left it undefined).
    expect(r.quiet_since).toBeUndefined();
  });

  it("activity clears quiet_since for the in-flight turn", () => {
    let r = runtimeReducer(fresh(), turnStart(TURN_1));
    r = runtimeReducer(r, { type: "heartbeat_timeout", turn_id: TURN_1, at: "t" });
    expect(r.quiet_since).toBe("t");
    r = runtimeReducer(r, { type: "liveness", turn_id: TURN_1 });
    expect(r.quiet_since).toBeUndefined();
  });

  it("turn_end clears quiet_since", () => {
    let r = runtimeReducer(fresh(), turnStart(TURN_1));
    r = runtimeReducer(r, { type: "heartbeat_timeout", turn_id: TURN_1, at: "t" });
    expect(r.quiet_since).toBe("t");
    r = runtimeReducer(r, {
      type: "turn_end",
      turn_id: TURN_1,
      outcome: { status: "completed" },
      ended_at: "t2",
    });
    expect(r.quiet_since).toBeUndefined();
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

  it("session_meta with empty model keeps the previously-shown model", () => {
    // Antigravity resume turns emit model: "" (no settings-change record).
    // That must not blank a model an earlier turn already populated.
    let r = runtimeReducer(fresh(), {
      type: "session_meta",
      agent_id: AGENT_A,
      model: "gemini-3.5-flash",
      harness_version: "1.0.0",
      tools: [],
      mcp_servers: [],
      skills: [],
      raw: {},
    });
    expect(r.meta?.model).toBe("gemini-3.5-flash");

    r = runtimeReducer(r, {
      type: "session_meta",
      agent_id: AGENT_A,
      model: "",
      harness_version: "1.0.0",
      tools: [],
      mcp_servers: [],
      skills: [],
      raw: {},
    });
    expect(r.meta?.model).toBe("gemini-3.5-flash");
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

  it("hydrate fills meta when currently empty + flips hydration_status to complete", () => {
    const r = runtimeReducer(fresh(), {
      type: "hydrate",
      agent_id: AGENT_A,
      turns: [],
      meta: {
        model: "claude-sonnet-4-6",
        harness_version: "2.1.140",
        tools: ["Bash"],
        mcp_servers: [{ name: "srv", status: "configured" }],
        skills: ["debug"],
      },
    });
    expect(r.hydration_status).toBe("complete");
    expect(r.meta?.model).toBe("claude-sonnet-4-6");
  });

  it("hydrate does NOT overwrite meta that a prior live event already populated", () => {
    // Pre-populate meta via a live session_meta event.
    let r = runtimeReducer(fresh(), {
      type: "session_meta",
      agent_id: AGENT_A,
      model: "live-model",
      harness_version: "live-version",
      tools: [],
      mcp_servers: [],
      skills: [],
      raw: {},
    });
    // Subsequent hydrate carries a different model — must NOT overwrite.
    r = runtimeReducer(r, {
      type: "hydrate",
      agent_id: AGENT_A,
      turns: [],
      meta: {
        model: "disk-model",
        harness_version: "disk-version",
        tools: [],
        mcp_servers: [],
        skills: [],
      },
    });
    expect(r.meta?.model).toBe("live-model");
  });

  it("hydrate fills last_rate_limit + its as_of when currently empty", () => {
    const r = runtimeReducer(fresh(), {
      type: "hydrate",
      agent_id: AGENT_A,
      turns: [],
      last_rate_limit: { primary: { used_percent: 10.0 } },
      last_rate_limit_as_of: "2026-05-27T18:42:11Z",
    });
    expect(r.last_rate_limit).toEqual({ primary: { used_percent: 10.0 } });
    // The capture time rides along with the value it qualifies.
    expect(r.last_rate_limit_as_of).toBe("2026-05-27T18:42:11Z");
  });

  it("live rate_limit_event after a stale hydrate clears as_of to null (stale → live)", () => {
    // Hydrate restores a stale on-disk snapshot with its capture time.
    let r = runtimeReducer(fresh(), {
      type: "hydrate",
      agent_id: AGENT_A,
      turns: [],
      last_rate_limit: { primary: { used_percent: 10.0 } },
      last_rate_limit_as_of: "2026-05-27T18:42:11Z",
    });
    expect(r.last_rate_limit_as_of).toBe("2026-05-27T18:42:11Z");
    // A live event overwrites the value and must drop the staleness qualifier
    // — the in-memory value is now live, not an aged on-disk snapshot.
    r = runtimeReducer(r, {
      type: "rate_limit_event",
      agent_id: AGENT_A,
      info: { primary: { used_percent: 99.0 } },
    });
    expect(r.last_rate_limit).toEqual({ primary: { used_percent: 99.0 } });
    expect(r.last_rate_limit_as_of).toBeNull();
  });

  it("hydrate after a fresh live event leaves the live value + null as_of in place", () => {
    // Live event sets the value and clears as_of.
    let r = runtimeReducer(fresh(), {
      type: "rate_limit_event",
      agent_id: AGENT_A,
      info: { primary: { used_percent: 99.0 } },
    });
    expect(r.last_rate_limit_as_of).toBeNull();
    // A late hydrate carrying a stale snapshot must NOT override the live
    // value, and must not reintroduce a stale `as_of` (fill-if-empty).
    r = runtimeReducer(r, {
      type: "hydrate",
      agent_id: AGENT_A,
      turns: [],
      last_rate_limit: { primary: { used_percent: 10.0 } },
      last_rate_limit_as_of: "2026-05-27T18:42:11Z",
    });
    expect(r.last_rate_limit).toEqual({ primary: { used_percent: 99.0 } });
    expect(r.last_rate_limit_as_of).toBeNull();
  });

  it("hydrate sets hydration_status=complete even with no payload", () => {
    const r = runtimeReducer(
      { ...fresh(), hydration_status: "loading" as const },
      { type: "hydrate", agent_id: AGENT_A, turns: [] },
    );
    expect(r.hydration_status).toBe("complete");
  });

  it("hydrate threads warnings into parse_warnings", () => {
    const r = runtimeReducer(fresh(), {
      type: "hydrate",
      agent_id: AGENT_A,
      turns: [],
      warnings: [
        { line_number: 0, reason: "session file no longer at recorded path" },
        { line_number: 42, reason: "malformed JSON: expected `,`" },
      ],
    });
    expect(r.parse_warnings).toHaveLength(2);
    expect(r.parse_warnings?.[0]?.reason).toBe("session file no longer at recorded path");
  });

  it("hydrate without warnings leaves parse_warnings undefined", () => {
    const r = runtimeReducer(fresh(), { type: "hydrate", agent_id: AGENT_A, turns: [] });
    expect(r.parse_warnings).toBeUndefined();
  });
});

describe("_internal.appendUserTurn", () => {
  it("synthesizes a user-role turn with caller-provided id/text/timestamp", () => {
    const turns = _internal.appendUserTurn(
      [],
      AGENT_A,
      "user-1",
      "hi there",
      [],
      "2026-05-16T00:00:00Z",
    );
    expect(turns).toHaveLength(1);
    expect(turns[0]).toEqual({
      role: "user",
      turn_id: "user-1",
      agent_id: AGENT_A,
      started_at: "2026-05-16T00:00:00Z",
      text: "hi there",
      attachments: [],
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
      [],
      "2026-05-16T00:00:10Z",
    );
    expect(next).not.toBe(prev);
    expect(next).toHaveLength(2);
    expect(next[0]).toBe(prev[0]); // first element same reference
  });
});

describe("_internal.appendFailedTurn", () => {
  it("synthesizes a failed agent turn carrying the error + send_id", () => {
    const turns = _internal.appendFailedTurn(
      [],
      AGENT_A,
      "failed-user-1",
      "2026-05-16T00:00:00Z",
      "send failed before the turn started",
      "send-1",
    );
    expect(turns).toHaveLength(1);
    const t = turns[0];
    if (t?.role !== "agent") throw new Error("unreachable");
    expect(t.status).toBe("failed");
    expect(t.error).toBe("send failed before the turn started");
    expect(t.error_kind).toBe("adapter_failure");
    expect(t.send_id).toBe("send-1");
    expect(t.items).toEqual([]);
  });

  it("is idempotent on turn_id (no duplicate row on a re-fire)", () => {
    const first = _internal.appendFailedTurn([], AGENT_A, "failed-x", RECEIVED_AT, "boom");
    const second = _internal.appendFailedTurn(first, AGENT_A, "failed-x", RECEIVED_AT, "boom");
    expect(second).toBe(first);
    expect(second).toHaveLength(1);
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
