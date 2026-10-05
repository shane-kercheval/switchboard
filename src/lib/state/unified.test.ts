import { describe, expect, it } from "vitest";
import type { ConversationItem } from "$lib/types";
import {
  answerTextOf,
  buildUnifiedRows,
  copyTextOf,
  groupRenderBlocks,
  lastAnswerTextOf,
  recentSendsStartIndex,
  type RenderBlock,
  type UnifiedRow,
} from "./unified";
import type { Turn } from "./types";

const AGENT_A = "00000000-0000-7000-8000-000000000aaa";
const AGENT_B = "00000000-0000-7000-8000-000000000bbb";
const AGENT_C = "00000000-0000-7000-8000-000000000ccc";
const TURN_1 = "00000000-0000-7000-8000-000000000001";
const SEND_1 = "00000000-0000-7000-8000-0000000000d1";

function userTurn(
  turnId: string,
  agentId: string,
  startedAt: string,
  text = "hi",
  sendId?: string,
  pending?: true,
): Turn {
  return {
    role: "user",
    turn_id: turnId,
    agent_id: agentId,
    send_id: sendId,
    started_at: startedAt,
    text,
    attachments: [],
    ...(pending === undefined ? {} : { pending }),
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
          facet: { facet_kind: "other" },
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
          facet: { facet_kind: "other" },
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
          facet: { facet_kind: "other" },
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
  it("renders a live user turn as a length-1 agent_ids row (multi-recipient-ready shape)", () => {
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

  it("orders same-second timestamps with different fractional precision chronologically", () => {
    const overlay: ConversationItem[] = [
      {
        kind: "user_message",
        id: "later",
        send_id: "later",
        agent_ids: [AGENT_A],
        text: "later",
        at: "2026-05-16T00:00:00.500Z",
      },
      {
        kind: "user_message",
        id: "earlier",
        send_id: "earlier",
        agent_ids: [AGENT_A],
        text: "earlier",
        at: "2026-05-16T00:00:00Z",
      },
    ];

    const rows = buildUnifiedRows([], overlay);

    expect(rows.map((row) => row.at)).toEqual(["2026-05-16T00:00:00Z", "2026-05-16T00:00:00.500Z"]);
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

  it("anchors a same-second live fan-out at its earliest fractional timestamp", () => {
    const rows = buildUnifiedRows(
      [
        userTurn(TURN_1, AGENT_A, "2026-05-16T00:00:00.500Z", "fan out", SEND_1),
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

    expect(rows.filter((row) => row.kind === "user")).toMatchObject([
      { at: "2026-05-16T00:00:00Z" },
    ]);
  });
});

describe("buildUnifiedRows: system markers (compaction)", () => {
  function compactionItem(agentId: string, at: string, summary = "recap"): ConversationItem {
    return {
      kind: "system_marker",
      id: `marker:${agentId}:${at}`,
      agent_id: agentId,
      marker: { marker_kind: "compaction", summary },
      at,
    };
  }

  it("renders a compaction marker as an agent-attributed row, between turns", () => {
    const rows = buildUnifiedRows(
      [
        userTurn(TURN_1, AGENT_A, "2026-05-16T00:00:00Z", "go", SEND_1),
        agentTurn("a1", AGENT_A, "2026-05-16T00:00:01Z", SEND_1),
      ],
      [compactionItem(AGENT_A, "2026-05-16T00:00:02Z", "the recap text")],
    );
    expect(rows.map((r) => r.kind)).toEqual(["user", "agent", "system_marker"]);
    const marker = rows.find((r) => r.kind === "system_marker")!;
    expect(marker).toMatchObject({
      agent_id: AGENT_A,
      marker: { marker_kind: "compaction", summary: "the recap text" },
    });
    // It carries no send_id, so it never groups into a fan-out.
    expect((marker as Extract<UnifiedRow, { kind: "system_marker" }>).send_id).toBeUndefined();
  });

  it("gives a context-report marker no row at all", () => {
    // Not "renders an empty body": the row snippet draws the agent name and the
    // hover timestamp *around* the marker, so an empty body would still leave a
    // bare labelled row in the transcript. A breakdown is not conversation, and
    // its content reaches the user through the panel instead.
    const rows = buildUnifiedRows(
      [
        userTurn(TURN_1, AGENT_A, "2026-05-16T00:00:00Z", "go", SEND_1),
        agentTurn("a1", AGENT_A, "2026-05-16T00:00:01Z", SEND_1),
      ],
      [
        {
          kind: "system_marker",
          id: `marker:${AGENT_A}:report`,
          agent_id: AGENT_A,
          marker: { marker_kind: "context_report", report: { raw: "## Context Usage" } },
          at: "2026-05-16T00:00:02Z",
        },
      ],
    );

    expect(rows.map((r) => r.kind)).toEqual(["user", "agent"]);
  });

  it("attributes a marker to its own agent and prunes it when that agent leaves the roster", () => {
    // Agent B's marker must not leak into a roster that no longer includes B —
    // the marker is per-agent, not project-wide.
    const overlay: ConversationItem[] = [
      compactionItem(AGENT_A, "2026-05-16T00:00:00Z"),
      compactionItem(AGENT_B, "2026-05-16T00:00:01Z"),
    ];
    const rows = buildUnifiedRows([], overlay, new Set([AGENT_A]));
    const markers = rows.filter((r) => r.kind === "system_marker");
    expect(markers).toHaveLength(1);
    expect((markers[0] as Extract<UnifiedRow, { kind: "system_marker" }>).agent_id).toBe(AGENT_A);
  });

  it("keys the marker row parse-stably so a re-parse doesn't churn its expanded state", () => {
    // The marker's `turn_id` (the overlay item `id`) is regenerated on every
    // parse; the rendered row must NOT key off it, or a project refresh would
    // destroy/recreate the row and collapse a recap the user expanded. Same
    // (agent, at) → same key, even with a different `id`.
    const at = "2026-05-16T00:00:02Z";
    const keyOf = (id: string): string => {
      const item: ConversationItem = {
        kind: "system_marker",
        id,
        agent_id: AGENT_A,
        marker: { marker_kind: "compaction", summary: "recap" },
        at,
      };
      const rows = buildUnifiedRows([], [item]);
      return rows.find((r) => r.kind === "system_marker")!.key;
    };
    expect(keyOf("parse-1-uuid")).toBe(keyOf("parse-2-uuid"));
  });
});

describe("recentSendsStartIndex", () => {
  function exchange(sendId: string, second: number, pending?: true): Turn[] {
    const at = `2026-05-16T00:00:${String(second).padStart(2, "0")}Z`;
    const prompt = userTurn(`u-${sendId}`, AGENT_A, at, "hi", sendId, pending);
    return pending ? [prompt] : [prompt, agentTurn(`a-${sendId}`, AGENT_A, at, sendId)];
  }
  function indexOfUser(blocks: RenderBlock[], sendId: string): number {
    return blocks.findIndex((b) =>
      b.kind === "fanout" ? b.send_id === sendId : b.row.key === `u:${sendId}`,
    );
  }

  it("starts at the block holding the count-th most recent send", () => {
    const blocks = groupRenderBlocks(
      buildUnifiedRows(
        [...exchange("s1", 0), ...exchange("s2", 1), ...exchange("s3", 2), ...exchange("s4", 3)],
        [],
      ),
    );
    expect(recentSendsStartIndex(blocks, 3)).toBe(indexOfUser(blocks, "s2"));
    expect(recentSendsStartIndex(blocks, 1)).toBe(indexOfUser(blocks, "s4"));
  });

  it("starts at the oldest send when there are fewer sends than the count", () => {
    const blocks = groupRenderBlocks(
      buildUnifiedRows(
        // A response with no prompt of its own (imported history) precedes the send.
        [agentTurn("a-orphan", AGENT_A, "2026-05-16T00:00:00Z"), ...exchange("s1", 1)],
        [],
      ),
    );
    expect(recentSendsStartIndex(blocks, 3)).toBe(indexOfUser(blocks, "s1"));
    expect(indexOfUser(blocks, "s1")).toBeGreaterThan(0);
  });

  it("is empty when no send has started", () => {
    const blocks = groupRenderBlocks(
      buildUnifiedRows([agentTurn("a-orphan", AGENT_A, "2026-05-16T00:00:00Z")], []),
    );
    expect(recentSendsStartIndex(blocks, 3)).toBe(blocks.length);
  });

  it("skips a send nothing has started but counts a partially started fan-out", () => {
    const blocks = groupRenderBlocks(
      buildUnifiedRows(
        [
          ...exchange("s1", 0),
          ...exchange("s2", 1),
          // Fan-out: A has started, B is still queued behind other work.
          userTurn("u-s3a", AGENT_A, "2026-05-16T00:00:02Z", "fan", "s3"),
          userTurn("u-s3b", AGENT_B, "2026-05-16T00:00:02Z", "fan", "s3", true),
          agentTurn("a-s3", AGENT_A, "2026-05-16T00:00:02Z", "s3"),
          ...exchange("s4", 3, true),
        ],
        [],
      ),
      [AGENT_A, AGENT_B],
    );
    expect(recentSendsStartIndex(blocks, 2)).toBe(indexOfUser(blocks, "s2"));
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

describe("queued compactions", () => {
  const COMPACT_SEND = "00000000-0000-7000-8000-00000000c001";

  it("interleaves chronologically instead of pinning to either end", () => {
    // A compaction has no user message to anchor to, so `queued_at` is the only
    // thing that can place it — and placement is the whole point: it must sit
    // behind work already queued and ahead of work queued after, matching the
    // order the backend will actually run them in.
    const rows = buildUnifiedRows(
      [
        userTurn(TURN_1, AGENT_A, "2026-05-15T00:00:00Z", "first", SEND_1),
        agentTurn("t-1", AGENT_A, "2026-05-15T00:00:01Z", SEND_1),
      ],
      [],
      undefined,
      [{ agent_id: AGENT_A, send_id: COMPACT_SEND, queued_at: "2026-05-15T00:00:02Z" }],
    );

    expect(rows.map((r) => r.kind)).toEqual(["user", "agent", "queued_compaction"]);
    const queued = rows.at(-1);
    expect(queued?.kind === "queued_compaction" && queued.cancel_send_id).toBe(COMPACT_SEND);
    // Never grouped into a fan-out: a compaction belongs to no send.
    expect(queued?.send_id).toBeUndefined();
  });

  it("sorts after a turn on its own agent that started while it waited", () => {
    // Same agent: a turn that started at :09 is work queued ahead of the
    // compaction (its prompt re-stamped to turn-start), and the compaction
    // cannot run before it — so it renders below, not above by its earlier
    // queued_at.
    const rows = buildUnifiedRows(
      [agentTurn("t-late", AGENT_A, "2026-05-15T00:00:09Z")],
      [],
      undefined,
      [{ agent_id: AGENT_A, send_id: COMPACT_SEND, queued_at: "2026-05-15T00:00:02Z" }],
    );
    expect(rows.map((r) => r.kind)).toEqual(["agent", "queued_compaction"]);
  });

  it("keeps chronological order against another agent's later turn", () => {
    // Nothing on its own agent has run since it was queued, so it is not
    // lifted: another agent's later work stays below it.
    const rows = buildUnifiedRows(
      [agentTurn("t-late", AGENT_B, "2026-05-15T00:00:09Z")],
      [],
      undefined,
      [{ agent_id: AGENT_A, send_id: COMPACT_SEND, queued_at: "2026-05-15T00:00:02Z" }],
    );
    expect(rows.map((r) => r.kind)).toEqual(["queued_compaction", "agent"]);
  });

  it("is filtered out with its agent when that agent is removed", () => {
    // Same rule every other row follows — a removed agent leaves no orphan.
    const rows = buildUnifiedRows([], [], new Set([AGENT_B]), [
      { agent_id: AGENT_A, send_id: COMPACT_SEND, queued_at: "2026-05-15T00:00:02Z" },
    ]);
    expect(rows).toEqual([]);
  });
});

describe("buildUnifiedRows: pending sends", () => {
  const at = (seconds: string): string => `2026-05-16T00:00:${seconds}Z`;
  const label = (r: UnifiedRow): string =>
    r.kind === "user" ? `u:${r.text}` : r.kind === "agent" ? `a:${r.send_id ?? "?"}` : r.kind;

  it("renders a still-queued sibling after the send that just started, not above it", () => {
    // A queued at :00, B at :01, both behind a busy agent. A starts at :31 and
    // its prompt is re-stamped there; B is still waiting at :01. Anchored on
    // stamps alone, B would sit above A's exchange until B started too.
    const rows = buildUnifiedRows(
      [
        userTurn("u-a", AGENT_A, at("31"), "a", "send-a"),
        userTurn("u-b", AGENT_A, at("01"), "b", "send-b", true),
        agentTurn("t-a", AGENT_A, at("31"), "send-a"),
      ],
      [],
    );
    expect(rows.map(label)).toEqual(["u:a", "a:send-a", "u:b"]);
  });

  it("keeps submit order among pending rows, queued compactions included", () => {
    const rows = buildUnifiedRows(
      [
        userTurn("u-x", AGENT_A, at("31"), "x", "send-x"),
        agentTurn("t-x", AGENT_A, at("31"), "send-x"),
        userTurn("u-p2", AGENT_A, at("06"), "p2", "send-p2", true),
        userTurn("u-p1", AGENT_A, at("05"), "p1", "send-p1", true),
      ],
      [],
      undefined,
      [{ agent_id: AGENT_A, send_id: "compact", queued_at: at("05.500") }],
    );
    expect(rows.map(label)).toEqual(["u:x", "a:send-x", "u:p1", "queued_compaction", "u:p2"]);
  });

  it("does not lift a pending send past another agent's later work", () => {
    // Nothing on A has run since the send was queued, so it keeps chronological
    // order against B rather than sinking to the bottom of the transcript.
    const rows = buildUnifiedRows(
      [
        userTurn("u-a", AGENT_A, at("02"), "waiting", "send-a", true),
        userTurn("u-b", AGENT_B, at("09"), "b", "send-b"),
        agentTurn("t-b", AGENT_B, at("09"), "send-b"),
      ],
      [],
    );
    expect(rows.map(label)).toEqual(["u:waiting", "u:b", "a:send-b"]);
  });

  it("leaves a hydrated prompt with no matched response at its own time", () => {
    // History whose response could not be correlated also has "no agent row"
    // — but it carries no pending flag, so it is never mistaken for queued
    // work and dragged below later activity.
    const rows = buildUnifiedRows(
      [
        userTurn("u-old", AGENT_A, at("00"), "old", "send-old"),
        agentTurn("t-later", AGENT_A, at("10")),
      ],
      [],
    );
    expect(rows.map(label)).toEqual(["u:old", "a:?"]);
  });

  it("anchors a partially started fan-out at the recipient that started", () => {
    // Fan-out submitted at :00; A started at :10 (re-stamped), B still waits at
    // :00. A recap A wrote at :05 belongs above the exchange, which it is only
    // if the group anchors at A's start rather than B's submit.
    const recap: ConversationItem = {
      kind: "system_marker",
      id: "m",
      agent_id: AGENT_A,
      marker: { marker_kind: "compaction", summary: "recap" },
      at: at("05"),
    };
    const rows = buildUnifiedRows(
      [
        userTurn("u-a", AGENT_A, at("10"), "fan", SEND_1),
        userTurn("u-b", AGENT_B, at("00"), "fan", SEND_1, true),
        agentTurn("t-a", AGENT_A, at("10"), SEND_1),
      ],
      [recap],
    );
    expect(rows.map((r) => r.kind)).toEqual(["system_marker", "user", "agent"]);
    expect(rows.find((r) => r.kind === "user")).toMatchObject({
      at: at("10"),
      queued_at: at("00"),
      agent_ids: [AGENT_A, AGENT_B],
      pending_agent_ids: [AGENT_B],
    });
  });

  it("marks a fan-out pending only while no recipient has started", () => {
    const rows = buildUnifiedRows(
      [
        userTurn("u-a", AGENT_A, at("00"), "fan", SEND_1, true),
        userTurn("u-b", AGENT_B, at("00"), "fan", SEND_1, true),
      ],
      [],
    );
    expect(rows[0]).toMatchObject({
      kind: "user",
      at: at("00"),
      queued_at: at("00"),
      pending_agent_ids: [AGENT_A, AGENT_B],
    });
  });

  it("lifts a pending fan-out when one recipient runs work queued ahead of it", () => {
    // B is mid-turn (T). X was queued on A at :00, fan-out F to A and B at :01,
    // S to B at :02. X starts on A at :31: F lifts behind X, and S — queued
    // behind F on B — must lift behind F, not stay at :02 above it. Same for a
    // compaction queued on B behind F.
    const rows = buildUnifiedRows(
      [
        userTurn("u-t", AGENT_B, at("00"), "t", "send-t"),
        agentTurn("a-t", AGENT_B, at("00"), "send-t"),
        userTurn("u-x", AGENT_A, at("31"), "x", "send-x"),
        agentTurn("a-x", AGENT_A, at("31"), "send-x"),
        userTurn("u-fa", AGENT_A, at("01"), "f", "send-f", true),
        userTurn("u-fb", AGENT_B, at("01"), "f", "send-f", true),
        userTurn("u-s", AGENT_B, at("02"), "s", "send-s", true),
      ],
      [],
      undefined,
      [{ agent_id: AGENT_B, send_id: "compact", queued_at: at("03") }],
    );
    expect(rows.map(label)).toEqual([
      "u:t",
      "a:send-t",
      "u:x",
      "a:send-x",
      "u:f",
      "u:s",
      "queued_compaction",
    ]);
  });

  it("keeps a pending row queued ahead of a lifted fan-out above it", () => {
    // P1 was queued on B before F, P2 after. F lifts behind X on A; P1 stays
    // where B's queue has it (before F), P2 follows F.
    const rows = buildUnifiedRows(
      [
        userTurn("u-t", AGENT_B, at("00"), "t", "send-t"),
        agentTurn("a-t", AGENT_B, at("00"), "send-t"),
        userTurn("u-x", AGENT_A, at("31"), "x", "send-x"),
        agentTurn("a-x", AGENT_A, at("31"), "send-x"),
        userTurn("u-p1", AGENT_B, at("00.500"), "p1", "send-p1", true),
        userTurn("u-fa", AGENT_A, at("01"), "f", "send-f", true),
        userTurn("u-fb", AGENT_B, at("01"), "f", "send-f", true),
        userTurn("u-p2", AGENT_B, at("02"), "p2", "send-p2", true),
      ],
      [],
    );
    expect(rows.map(label)).toEqual(["u:t", "a:send-t", "u:p1", "u:x", "a:send-x", "u:f", "u:p2"]);
  });

  it("a fan-out started on one recipient holds its queue position on the other", () => {
    // B's queue is E, F, G. A (idle) starts F at once at :31; B is still on T.
    // F's row anchors at A's start. E, queued on B ahead of F, must stay above
    // it; G, queued behind F, must follow it. Counting F as started work on B
    // would drag E below F — a send B runs after E.
    const rows = buildUnifiedRows(
      [
        userTurn("u-t", AGENT_B, at("00"), "t", "send-t"),
        agentTurn("a-t", AGENT_B, at("00"), "send-t"),
        userTurn("u-e", AGENT_B, at("05"), "e", "send-e", true),
        userTurn("u-fa", AGENT_A, at("31"), "f", "send-f"),
        userTurn("u-fb", AGENT_B, at("06"), "f", "send-f", true),
        agentTurn("a-fa", AGENT_A, at("31"), "send-f"),
        userTurn("u-g", AGENT_B, at("07"), "g", "send-g", true),
      ],
      [],
    );
    expect(rows.map(label)).toEqual(["u:t", "a:send-t", "u:e", "u:f", "a:send-f", "u:g"]);
  });

  it("a fan-out started on one recipient still lifts later work on the other past B's own runs", () => {
    // Same shape, but B also finished an exchange at :40 after F started on A.
    // G is behind F on B *and* behind B's :40 run, so it lands after both;
    // F's row itself is not moved by B's later run — it sits where A started
    // it, as a reload would show.
    const rows = buildUnifiedRows(
      [
        userTurn("u-fa", AGENT_A, at("31"), "f", "send-f"),
        userTurn("u-fb", AGENT_B, at("06"), "f", "send-f", true),
        agentTurn("a-fa", AGENT_A, at("31"), "send-f"),
        userTurn("u-b", AGENT_B, at("40"), "b", "send-b"),
        agentTurn("a-b", AGENT_B, at("40"), "send-b"),
        userTurn("u-g", AGENT_B, at("07"), "g", "send-g", true),
      ],
      [],
    );
    expect(rows.map(label)).toEqual(["u:f", "a:send-f", "u:b", "a:send-b", "u:g"]);
  });

  it("a fan-out placed at its first recipient's start stays there after the other recipient runs older work", () => {
    // The documented boundary, not a defect. E was queued on B and C at :00; F
    // to A and B at :01. A starts F at :31; C runs an older job X at :40, which
    // lifts E (still fully pending) behind X. B's queue is E then F, but F sits
    // at A's start and is never lifted — so the display reads F, X, E.
    const during = buildUnifiedRows(
      [
        userTurn("u-eb", AGENT_B, at("00"), "e", "send-e", true),
        userTurn("u-ec", AGENT_C, at("00"), "e", "send-e", true),
        userTurn("u-fa", AGENT_A, at("31"), "f", "send-f"),
        userTurn("u-fb", AGENT_B, at("01"), "f", "send-f", true),
        agentTurn("a-fa", AGENT_A, at("31"), "send-f"),
        userTurn("u-x", AGENT_C, at("40"), "x", "send-x"),
        agentTurn("a-x", AGENT_C, at("40"), "send-x"),
      ],
      [],
    );
    expect(during.map(label)).toEqual(["u:f", "a:send-f", "u:x", "a:send-x", "u:e"]);

    // B then runs E at :50 and F at :60 (C ran E at :55). F keeps A's :31, so
    // the order is unchanged — and a reload, which groups the journal's
    // per-recipient records at the earliest, shows the same. Chronology of
    // start times holds; B's execution order is what the fan-out columns show.
    const after = buildUnifiedRows(
      [
        userTurn("u-eb", AGENT_B, at("50"), "e", "send-e"),
        userTurn("u-ec", AGENT_C, at("55"), "e", "send-e"),
        agentTurn("a-eb", AGENT_B, at("50"), "send-e"),
        agentTurn("a-ec", AGENT_C, at("55"), "send-e"),
        userTurn("u-fa", AGENT_A, at("31"), "f", "send-f"),
        userTurn("u-fb", AGENT_B, at("60"), "f", "send-f"),
        agentTurn("a-fa", AGENT_A, at("31"), "send-f"),
        agentTurn("a-fb", AGENT_B, at("60"), "send-f"),
        userTurn("u-x", AGENT_C, at("40"), "x", "send-x"),
        agentTurn("a-x", AGENT_C, at("40"), "send-x"),
      ],
      [],
    );
    expect(after.map(label)).toEqual([
      "u:f",
      "a:send-f",
      "a:send-f",
      "u:x",
      "a:send-x",
      "u:e",
      "a:send-e",
      "a:send-e",
    ]);
  });
});
