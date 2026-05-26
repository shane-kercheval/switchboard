import { describe, expect, it } from "vitest";
import type { ConversationItem } from "$lib/types";
import { buildUnifiedRows, groupRenderBlocks, type RenderBlock, type UnifiedRow } from "./unified";
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
      { kind: "user_message", send_id: SEND_1, agent_ids: [AGENT_A], text: "go", at },
    ];
    const rows = buildUnifiedRows([], overlay);
    expect(rows.map((r) => r.kind)).toEqual(["user", "outcome"]);
  });

  it("sorts a user message before an agent turn at an identical timestamp", () => {
    const at = "2026-05-16T00:00:00Z";
    const overlay: ConversationItem[] = [
      { kind: "user_message", send_id: SEND_1, agent_ids: [AGENT_A], text: "go", at },
    ];
    const rows = buildUnifiedRows([agentTurn(TURN_1, AGENT_A, at)], overlay);
    expect(rows.map((r) => r.kind)).toEqual(["user", "agent"]);
  });

  it("merges the two sources chronologically", () => {
    const overlay: ConversationItem[] = [
      {
        kind: "user_message",
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
