import { describe, expect, it } from "vitest";
import type { ConversationItem } from "$lib/types";
import { buildUnifiedRows, type UnifiedRow } from "./unified";
import type { Turn } from "./types";

const AGENT_A = "00000000-0000-7000-8000-000000000aaa";
const AGENT_B = "00000000-0000-7000-8000-000000000bbb";
const TURN_1 = "00000000-0000-7000-8000-000000000001";
const SEND_1 = "00000000-0000-7000-8000-0000000000d1";

function userTurn(turnId: string, agentId: string, startedAt: string, text = "hi"): Turn {
  return { role: "user", turn_id: turnId, agent_id: agentId, started_at: startedAt, text };
}

function agentTurn(turnId: string, agentId: string, startedAt: string): Turn {
  return {
    role: "agent",
    turn_id: turnId,
    agent_id: agentId,
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
});
