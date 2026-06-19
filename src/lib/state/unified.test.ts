import { describe, expect, it } from "vitest";
import type { ConversationItem } from "$lib/types";
import {
  answerTextOf,
  buildUnifiedRows,
  copyTextOf,
  groupRenderBlocks,
  lastAnswerTextOf,
  type RenderBlock,
  type UnifiedRow,
} from "./unified";
import type { Turn } from "./types";

const AGENT_A = "00000000-0000-7000-8000-000000000aaa";
const AGENT_B = "00000000-0000-7000-8000-000000000bbb";
const TURN_1 = "00000000-0000-7000-8000-000000000001";
const SEND_1 = "00000000-0000-7000-8000-0000000000d1";

function userTurn(
  turnId: string,
  agentId: string,
  startedAt: string,
  text = "hi",
  sendId?: string,
): Turn {
  return {
    role: "user",
    turn_id: turnId,
    agent_id: agentId,
    send_id: sendId,
    started_at: startedAt,
    text,
    attachments: [],
  };
}

function agentTurn(turnId: string, agentId: string, startedAt: string, sendId?: string): Turn {
  return {
    role: "agent",
    turn_id: turnId,
    agent_id: agentId,
    send_id: sendId,
    started_at: startedAt,
    status: "complete",
    items: [],
  };
}

describe("answerTextOf", () => {
  it("joins answer text and excludes reasoning + tool calls", () => {
    const turn: Extract<Turn, { role: "agent" }> = {
      role: "agent",
      turn_id: TURN_1,
      agent_id: AGENT_A,
      started_at: "2026-05-16T00:00:00Z",
      status: "complete",
      items: [
        { item_kind: "text", kind: "thinking", text: "private reasoning" },
        { item_kind: "text", kind: "text", text: "Step one." },
        {
          item_kind: "tool",
          tool_use_id: "t1",
          kind: "builtin",
          name: "Bash",
          input: {},
          output: "tool output",
          is_error: false,
          started_at: "2026-05-16T00:00:01Z",
          completed_at: "2026-05-16T00:00:02Z",
        },
        { item_kind: "text", kind: "text", text: "Step two." },
      ],
    };
    // Only the answer prose, joined — no reasoning, no tool output.
    expect(answerTextOf(turn)).toBe("Step one.\n\nStep two.");
  });

  it("removes blank outer lines from each answer block before joining", () => {
    const turn: Extract<Turn, { role: "agent" }> = {
      role: "agent",
      turn_id: TURN_1,
      agent_id: AGENT_A,
      started_at: "2026-05-16T00:00:00Z",
      status: "complete",
      items: [
        { item_kind: "text", kind: "text", text: "\n\nStep one.\n\n" },
        { item_kind: "text", kind: "thinking", text: "private reasoning" },
        { item_kind: "text", kind: "text", text: "\n\nStep two.\n\n" },
      ],
    };
    expect(answerTextOf(turn)).toBe("Step one.\n\nStep two.");
  });

  it("preserves indentation and trailing spaces on meaningful lines", () => {
    const turn: Extract<Turn, { role: "agent" }> = {
      role: "agent",
      turn_id: TURN_1,
      agent_id: AGENT_A,
      started_at: "2026-05-16T00:00:00Z",
      status: "complete",
      items: [
        {
          item_kind: "text",
          kind: "text",
          text: "\n\n    indented code  \n    still indented\n\n",
        },
      ],
    };
    expect(answerTextOf(turn)).toBe("    indented code  \n    still indented");
  });

  it("returns empty string for a reasoning-only / tool-only turn", () => {
    const turn: Extract<Turn, { role: "agent" }> = {
      role: "agent",
      turn_id: TURN_1,
      agent_id: AGENT_A,
      started_at: "2026-05-16T00:00:00Z",
      status: "complete",
      items: [{ item_kind: "text", kind: "thinking", text: "just thinking" }],
    };
    expect(answerTextOf(turn)).toBe("");
  });
});

describe("lastAnswerTextOf", () => {
  it("returns only the final non-empty answer text block", () => {
    const turn: Extract<Turn, { role: "agent" }> = {
      role: "agent",
      turn_id: TURN_1,
      agent_id: AGENT_A,
      started_at: "2026-05-16T00:00:00Z",
      status: "complete",
      items: [
        { item_kind: "text", kind: "text", text: "Step one." },
        {
          item_kind: "tool",
          tool_use_id: "t1",
          kind: "builtin",
          name: "Bash",
          input: {},
          output: "tool output",
          is_error: false,
          started_at: "2026-05-16T00:00:01Z",
          completed_at: "2026-05-16T00:00:02Z",
        },
        { item_kind: "text", kind: "text", text: "Step two." },
      ],
    };
    expect(lastAnswerTextOf(turn)).toBe("Step two.");
  });

  it("scans backward past thinking, tools, and blank answer blocks", () => {
    const turn: Extract<Turn, { role: "agent" }> = {
      role: "agent",
      turn_id: TURN_1,
      agent_id: AGENT_A,
      started_at: "2026-05-16T00:00:00Z",
      status: "complete",
      items: [
        { item_kind: "text", kind: "text", text: "Useful answer" },
        { item_kind: "text", kind: "text", text: " \n " },
        { item_kind: "text", kind: "thinking", text: "private reasoning" },
        {
          item_kind: "tool",
          tool_use_id: "t1",
          kind: "builtin",
          name: "Bash",
          input: {},
          started_at: "2026-05-16T00:00:01Z",
        },
      ],
    };
    expect(lastAnswerTextOf(turn)).toBe("Useful answer");
  });
});

describe("copyTextOf", () => {
  it("copyTextOf dispatches by copy mode", () => {
    const turn: Extract<Turn, { role: "agent" }> = {
      role: "agent",
      turn_id: TURN_1,
      agent_id: AGENT_A,
      started_at: "2026-05-16T00:00:00Z",
      status: "complete",
      items: [
        { item_kind: "text", kind: "text", text: "Step one." },
        { item_kind: "text", kind: "text", text: "Step two." },
      ],
    };
    expect(copyTextOf(turn, "full_answer")).toBe("Step one.\n\nStep two.");
    expect(copyTextOf(turn, "last_answer_block")).toBe("Step two.");
  });
});

describe("buildUnifiedRows", () => {
  it("renders a live user turn as a length-1 agent_ids row (M4.7-ready shape)", () => {
    const rows = buildUnifiedRows([userTurn(TURN_1, AGENT_A, "2026-05-16T00:00:00Z")], []);
    expect(rows).toHaveLength(1);
    const row = rows[0] as Extract<UnifiedRow, { kind: "user" }>;
    expect(row.kind).toBe("user");
    expect(row.agent_ids).toEqual([AGENT_A]);
  });

  it("preserves the recipient set for a historical (grouped) user message", () => {
    const overlay: ConversationItem[] = [
      {
        kind: "user_message",
        id: SEND_1,
        send_id: SEND_1,
        agent_ids: [AGENT_A, AGENT_B],
        text: "fan out",
        at: "2026-05-16T00:00:00Z",
      },
    ];
    const rows = buildUnifiedRows([], overlay);
    const row = rows[0] as Extract<UnifiedRow, { kind: "user" }>;
    expect(row.agent_ids).toEqual([AGENT_A, AGENT_B]);
    expect(row.text).toBe("fan out");
  });

  it("renders an imported user message (send_id null) as a standalone, ungrouped row", () => {
    // The backend surfaces a pre-journaling/imported prompt with `send_id: null`,
    // keyed by `id` (the harness turn_id). It must key off `id` (not send_id),
    // coerce null→undefined so the grouping/anchor `=== undefined` guards hold,
    // and never be pulled into a fan-out.
    const overlay: ConversationItem[] = [
      {
        kind: "user_message",
        id: "imported-turn-1",
        send_id: null,
        agent_ids: [AGENT_A],
        text: "imported prompt",
        at: "2026-05-16T00:00:00Z",
      },
    ];
    const rows = buildUnifiedRows([], overlay);
    expect(rows).toHaveLength(1);
    const row = rows[0] as Extract<UnifiedRow, { kind: "user" }>;
    expect(row.key).toBe("u:imported-turn-1");
    expect(row.send_id).toBeUndefined();
    const blocks = groupRenderBlocks(rows);
    expect(blocks.filter((b) => b.kind === "fanout")).toHaveLength(0);
    expect(blocks.map((b) => (b.kind === "row" ? b.row.kind : "fanout"))).toEqual(["user"]);
  });

  it("prunes a removed agent from a historical fan-out's recipient set", () => {
    // A removed agent (AGENT_B) lingers in the journal overlay's recipient set;
    // filtering against the live roster keeps the message but drops the orphan
    // column that would otherwise render "unknown / queued".
    const overlay: ConversationItem[] = [
      {
        kind: "user_message",
        id: SEND_1,
        send_id: SEND_1,
        agent_ids: [AGENT_A, AGENT_B],
        text: "fan out",
        at: "2026-05-16T00:00:00Z",
      },
    ];
    const rows = buildUnifiedRows([], overlay, new Set([AGENT_A]));
    expect(rows).toHaveLength(1);
    const row = rows[0] as Extract<UnifiedRow, { kind: "user" }>;
    expect(row.agent_ids).toEqual([AGENT_A]);
  });

  it("drops a user message whose only recipient was removed", () => {
    const overlay: ConversationItem[] = [
      {
        kind: "user_message",
        id: SEND_1,
        send_id: SEND_1,
        agent_ids: [AGENT_B],
        text: "gone",
        at: "2026-05-16T00:00:00Z",
      },
    ];
    expect(buildUnifiedRows([], overlay, new Set([AGENT_A]))).toHaveLength(0);
  });

  it("drops an outcome marker for a removed agent", () => {
    const overlay: ConversationItem[] = [
      {
        kind: "outcome",
        send_id: SEND_1,
        turn_id: TURN_1,
        agent_id: AGENT_B,
        status: "failed",
        reason: "boom",
        at: "2026-05-16T00:00:00Z",
      },
    ];
    expect(buildUnifiedRows([], overlay, new Set([AGENT_A]))).toHaveLength(0);
  });

  it("sorts a user message before its outcome marker at an identical timestamp", () => {
    // Real data: a failed-to-start / cancelled turn has Send.at ==
    // Outcome.at, so a timestamp-only sort would float the marker above its
    // own prompt. The kind_rank tiebreak (user < outcome) prevents that.
    const at = "2026-05-16T00:00:00Z";
    const overlay: ConversationItem[] = [
      {
        kind: "outcome",
        turn_id: TURN_1,
        send_id: SEND_1,
        agent_id: AGENT_A,
        status: "failed",
        reason: "boom",
        at,
      },
      { kind: "user_message", id: SEND_1, send_id: SEND_1, agent_ids: [AGENT_A], text: "go", at },
    ];
    const rows = buildUnifiedRows([], overlay);
    expect(rows.map((r) => r.kind)).toEqual(["user", "outcome"]);
  });

  it("sorts a user message before an agent turn at an identical timestamp", () => {
    const at = "2026-05-16T00:00:00Z";
    const overlay: ConversationItem[] = [
      { kind: "user_message", id: SEND_1, send_id: SEND_1, agent_ids: [AGENT_A], text: "go", at },
    ];
    const rows = buildUnifiedRows([agentTurn(TURN_1, AGENT_A, at)], overlay);
    expect(rows.map((r) => r.kind)).toEqual(["user", "agent"]);
  });

  it("merges the two sources chronologically", () => {
    const overlay: ConversationItem[] = [
      {
        kind: "user_message",
        id: SEND_1,
        send_id: SEND_1,
        agent_ids: [AGENT_A],
        text: "first",
        at: "2026-05-16T00:00:00Z",
      },
    ];
    const turns: Turn[] = [
      agentTurn(TURN_1, AGENT_A, "2026-05-16T00:00:01Z"),
      userTurn("00000000-0000-7000-8000-000000000002", AGENT_A, "2026-05-16T00:00:02Z", "second"),
    ];
    const rows = buildUnifiedRows(turns, overlay);
    expect(rows.map((r) => r.at)).toEqual([
      "2026-05-16T00:00:00Z",
      "2026-05-16T00:00:01Z",
      "2026-05-16T00:00:02Z",
    ]);
  });

  it("anchors a queued send's response under its own prompt, not by run-time", () => {
    // Two sequential single-recipient sends: prompts stamped near submit
    // (00, 01), but send-2 is queued so its response only runs at 20 — after
    // BOTH prompts. A raw-timestamp sort would float both responses to the
    // bottom (detached from their prompts); send-anchored ordering keeps each
    // response under its own prompt.
    const rows = buildUnifiedRows(
      [
        userTurn(TURN_1, AGENT_A, "2026-05-16T00:00:00Z", "first", "send-1"),
        userTurn("u2", AGENT_A, "2026-05-16T00:00:01Z", "second", "send-2"),
        agentTurn("a1", AGENT_A, "2026-05-16T00:00:10Z", "send-1"),
        agentTurn("a2", AGENT_A, "2026-05-16T00:00:20Z", "send-2"),
      ],
      [],
    );
    expect(rows.map((r) => (r.kind === "user" ? `u:${r.text}` : `a:${r.send_id ?? "?"}`))).toEqual([
      "u:first",
      "a:send-1",
      "u:second",
      "a:send-2",
    ]);
  });

  it("ignores an agent_turn item that strays into the overlay (no double-render)", () => {
    const overlay: ConversationItem[] = [
      {
        kind: "agent_turn",
        turn_id: TURN_1,
        agent_id: AGENT_A,
        started_at: "2026-05-16T00:00:00Z",
        status: "complete",
        items: [],
      },
    ];
    expect(buildUnifiedRows([], overlay)).toHaveLength(0);
  });

  it("collapses a live fan-out's user turns (shared send_id) into one row", () => {
    // Two recipients of one Send each get an optimistic user turn with the same
    // send_id; the unified row renders the user's message once.
    const rows = buildUnifiedRows(
      [
        userTurn(TURN_1, AGENT_A, "2026-05-16T00:00:00Z", "fan out", SEND_1),
        userTurn(
          "00000000-0000-7000-8000-000000000002",
          AGENT_B,
          "2026-05-16T00:00:00Z",
          "fan out",
          SEND_1,
        ),
      ],
      [],
    );
    const users = rows.filter((r) => r.kind === "user");
    expect(users).toHaveLength(1);
    expect((users[0] as Extract<UnifiedRow, { kind: "user" }>).agent_ids).toEqual([
      AGENT_A,
      AGENT_B,
    ]);
  });
});

describe("groupRenderBlocks", () => {
  function fanoutOf(blocks: RenderBlock[]): Extract<RenderBlock, { kind: "fanout" }> {
    const f = blocks.find((b) => b.kind === "fanout");
    if (f === undefined || f.kind !== "fanout") throw new Error("no fanout block");
    return f;
  }

  it("groups a fan-out's responses into per-recipient columns in recipient order", () => {
    const rows = buildUnifiedRows(
      [
        userTurn(TURN_1, AGENT_A, "2026-05-16T00:00:00Z", "fan out", SEND_1),
        userTurn("u2", AGENT_B, "2026-05-16T00:00:00Z", "fan out", SEND_1),
        // B's response streams in before A's — columns must NOT reshuffle.
        agentTurn("tb", AGENT_B, "2026-05-16T00:00:01Z", SEND_1),
        agentTurn("ta", AGENT_A, "2026-05-16T00:00:02Z", SEND_1),
      ],
      [],
    );
    const blocks = groupRenderBlocks(rows);
    // One fan-out block, and the agent rows are NOT also standalone.
    expect(blocks.filter((b) => b.kind === "fanout")).toHaveLength(1);
    expect(blocks.filter((b) => b.kind === "row" && b.row.kind === "agent")).toHaveLength(0);
    const fan = fanoutOf(blocks);
    expect(fan.columns.map((c) => c.agent_id)).toEqual([AGENT_A, AGENT_B]);
    expect(fan.columns[0]?.rows.map((r) => r.key)).toEqual(["a:ta"]);
    expect(fan.columns[1]?.rows.map((r) => r.key)).toEqual(["a:tb"]);
  });

  it("emits one fan-out block when a send appears as both a live and a journal-overlay user row", () => {
    // In flight, a send surfaces twice: the live user turns (this session) AND
    // the journal-overlay `user_message` written at turn-start — both carry
    // SEND_1 but have distinct row keys (`u:<send_id>` vs `u:<journal_id>`), so
    // they pass every upstream check. Without block-level dedup this minted two
    // fan-out blocks with the same `f:SEND_1` key, which breaks the transcript's
    // keyed `{#each}` (orphaned, never-updated DOM — a stuck hover footer).
    const rows = buildUnifiedRows(
      [
        userTurn(TURN_1, AGENT_A, "2026-05-16T00:00:00Z", "fan out", SEND_1),
        userTurn("u2", AGENT_B, "2026-05-16T00:00:00Z", "fan out", SEND_1),
        agentTurn("ta", AGENT_A, "2026-05-16T00:00:01Z", SEND_1),
      ],
      [
        {
          kind: "user_message",
          id: "journal-1",
          send_id: SEND_1,
          agent_ids: [AGENT_A, AGENT_B],
          text: "fan out",
          at: "2026-05-16T00:00:00Z",
        },
      ],
    );
    const blocks = groupRenderBlocks(rows, [AGENT_A, AGENT_B]);
    expect(blocks.filter((b) => b.kind === "fanout")).toHaveLength(1);
    // Block keys must be unique — duplicate keys are what corrupt the keyed each.
    const keys = blocks.map((b) => (b.kind === "fanout" ? b.key : b.row.key));
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("routes a per-recipient outcome marker into that recipient's column", () => {
    const rows = buildUnifiedRows(
      [
        userTurn(TURN_1, AGENT_A, "2026-05-16T00:00:00Z", "fan out", SEND_1),
        userTurn("u2", AGENT_B, "2026-05-16T00:00:00Z", "fan out", SEND_1),
        agentTurn("ta", AGENT_A, "2026-05-16T00:00:01Z", SEND_1),
      ],
      [
        {
          kind: "outcome",
          turn_id: "tb",
          send_id: SEND_1,
          agent_id: AGENT_B,
          status: "cancelled",
          reason: "user",
          at: "2026-05-16T00:00:01Z",
        },
      ],
    );
    const fan = fanoutOf(groupRenderBlocks(rows));
    expect(fan.columns[0]?.rows.map((r) => r.kind)).toEqual(["agent"]);
    expect(fan.columns[1]?.rows.map((r) => r.kind)).toEqual(["outcome"]);
  });

  it("renders a historical multi-recipient message with no correlated responses as a plain row", () => {
    // Historical/uncorrelated: the user message comes from the journal overlay
    // (live=false) and went to two agents, but the agent turns carry no matching
    // send_id (couldn't be correlated). It must render as a plain user message +
    // standalone responses — NOT a group of empty "queued" columns.
    const rows = buildUnifiedRows(
      [
        // Responses with NO send_id (uncorrelated).
        agentTurn("ta", AGENT_A, "2026-05-16T00:00:01Z"),
        agentTurn("tb", AGENT_B, "2026-05-16T00:00:02Z"),
      ],
      [
        {
          kind: "user_message",
          id: SEND_1,
          send_id: SEND_1,
          agent_ids: [AGENT_A, AGENT_B],
          text: "fan out",
          at: "2026-05-16T00:00:00Z",
        },
      ],
    );
    const blocks = groupRenderBlocks(rows);
    expect(blocks.filter((b) => b.kind === "fanout")).toHaveLength(0);
    // One collapsed user row (earliest) + the two responses, all standalone.
    expect(blocks.map((b) => (b.kind === "row" ? b.row.kind : "fanout"))).toEqual([
      "user",
      "agent",
      "agent",
    ]);
  });

  it("groups a LIVE all-busy fan-out with no responses yet (queued columns)", () => {
    // Every recipient is busy, so the send queues and no response has streamed
    // in. A live fan-out (user turns from this session) must still group, so the
    // per-recipient queued columns and cancel-send affordance show immediately —
    // unlike the historical uncorrelated case, which degrades to a plain row.
    const rows = buildUnifiedRows(
      [
        userTurn(TURN_1, AGENT_A, "2026-05-16T00:00:00Z", "fan out", SEND_1),
        userTurn("u2", AGENT_B, "2026-05-16T00:00:00Z", "fan out", SEND_1),
      ],
      [],
    );
    const blocks = groupRenderBlocks(rows);
    expect(blocks.filter((b) => b.kind === "fanout")).toHaveLength(1);
    const fan = fanoutOf(blocks);
    // Both recipient columns present (in recipient order), each still empty.
    expect(fan.columns.map((c) => c.agent_id)).toEqual([AGENT_A, AGENT_B]);
    expect(fan.columns.every((c) => c.rows.length === 0)).toBe(true);
  });

  it("orders fan-out columns by the canonical roster, not the recipient set's order", () => {
    // The restored user message lists recipients [B, A] (e.g. journal order),
    // but the roster (sidebar/chips) order is [A, B]. Columns must follow the
    // roster so the layout is identical live and after restart — and will track
    // user-defined reordering later.
    const rows = buildUnifiedRows(
      [
        agentTurn("ta", AGENT_A, "2026-05-16T00:00:01Z", SEND_1),
        agentTurn("tb", AGENT_B, "2026-05-16T00:00:02Z", SEND_1),
      ],
      [
        {
          kind: "user_message",
          id: SEND_1,
          send_id: SEND_1,
          agent_ids: [AGENT_B, AGENT_A],
          text: "fan out",
          at: "2026-05-16T00:00:00Z",
        },
      ],
    );
    const fan = fanoutOf(groupRenderBlocks(rows, [AGENT_A, AGENT_B]));
    expect(fan.columns.map((c) => c.agent_id)).toEqual([AGENT_A, AGENT_B]);
  });

  it("leaves a single-recipient send as standalone rows (no fan-out block)", () => {
    const rows = buildUnifiedRows(
      [
        userTurn(TURN_1, AGENT_A, "2026-05-16T00:00:00Z", "solo", SEND_1),
        agentTurn("ta", AGENT_A, "2026-05-16T00:00:01Z", SEND_1),
      ],
      [],
    );
    const blocks = groupRenderBlocks(rows);
    expect(blocks.every((b) => b.kind === "row")).toBe(true);
    expect(blocks.map((b) => (b.kind === "row" ? b.row.kind : "fanout"))).toEqual([
      "user",
      "agent",
    ]);
  });
});
