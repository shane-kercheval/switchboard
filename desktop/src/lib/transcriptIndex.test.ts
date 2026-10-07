import { describe, expect, it } from "vitest";
import { buildNavigatorEntries, filterEntries } from "./transcriptIndex";
import { buildUnifiedRows, type UnifiedRow } from "./state/unified";
import type { Turn } from "./state/index.svelte";

const A = "00000000-0000-7000-8000-000000000aaa";
const B = "00000000-0000-7000-8000-000000000bbb";
const NAMES = new Map([
  [A, "alice"],
  [B, "bob"],
]);

function userTurn(agentId: string, text: string, sendId: string, at: string): Turn {
  return {
    role: "user",
    turn_id: `u-${agentId}-${sendId}`,
    agent_id: agentId,
    started_at: at,
    text,
    send_id: sendId,
  };
}

function agentTurn(
  agentId: string,
  id: string,
  at: string,
  items: Extract<Turn, { role: "agent" }>["items"],
): Turn {
  return {
    role: "agent",
    turn_id: id,
    agent_id: agentId,
    started_at: at,
    status: "complete",
    items,
  };
}

describe("buildNavigatorEntries", () => {
  it("emits one entry per fan-out send and one attributed entry per agent turn, in order", () => {
    const rows = buildUnifiedRows(
      [
        userTurn(A, "review this", "send-1", "2026-05-16T00:00:00Z"),
        userTurn(B, "review this", "send-1", "2026-05-16T00:00:00Z"),
        agentTurn(A, "turn-a", "2026-05-16T00:00:01Z", [
          { item_kind: "text", kind: "text", text: "alice's answer" },
        ]),
        agentTurn(B, "turn-b", "2026-05-16T00:00:02Z", [
          { item_kind: "text", kind: "text", text: "bob's answer" },
        ]),
      ],
      [],
    );
    const entries = buildNavigatorEntries(rows, NAMES);

    expect(entries.map((e) => [e.role, e.attribution, e.preview])).toEqual([
      ["user", "You", "review this"],
      ["agent", "alice", "alice's answer"],
      ["agent", "bob", "bob's answer"],
    ]);
    // The user entry carries the full recipient set for pane resolution.
    expect(entries[0]!.agentIds).toEqual([A, B]);
    expect(entries[1]!.agentIds).toEqual([A]);
  });

  it("previews an attachment-only send by its attachment label", () => {
    const turn: Turn = {
      role: "user",
      turn_id: "u-1",
      agent_id: A,
      started_at: "2026-05-16T00:00:00Z",
      text: "",
      send_id: "send-1",
      attachments: [{ label: "diagram.png", kind: "image", path: "/x", original_name: "d.png" }],
    };
    const entries = buildNavigatorEntries(buildUnifiedRows([turn], []), NAMES);
    expect(entries[0]!.preview).toBe("diagram.png");
  });

  it("previews a tool-only turn via the tool-row vocabulary and a thinking-only turn via its first line", () => {
    const rows = buildUnifiedRows(
      [
        agentTurn(A, "turn-tools", "2026-05-16T00:00:01Z", [
          {
            item_kind: "tool",
            facet: { facet_kind: "shell", command: "cargo test" },
            tool_use_id: "t1",
            kind: "builtin",
            name: "Bash",
            input: { command: "cargo test" },
            is_error: false,
            started_at: "2026-05-16T00:00:01Z",
          },
        ]),
        agentTurn(A, "turn-think", "2026-05-16T00:00:02Z", [
          { item_kind: "text", kind: "thinking", text: "## pondering the plan\nmore" },
        ]),
      ],
      [],
    );
    const entries = buildNavigatorEntries(rows, NAMES);
    expect(entries[0]!.preview).toBe("Command · cargo test");
    expect(entries[1]!.preview).toBe("Thinking · pondering the plan");
    // Neither contributes searchable prose — search is prose-only by design.
    expect(entries[0]!.searchText).toBe("");
    expect(entries[1]!.searchText).toBe("");
  });

  it("skips outcome and system-marker rows — they aren't messages", () => {
    const outcome: UnifiedRow = {
      kind: "outcome",
      at: "2026-05-16T00:00:03Z",
      rank: 3,
      key: "o:turn-x",
      turn_id: "turn-x",
      agent_id: A,
      status: "cancelled",
    };
    const marker: UnifiedRow = {
      kind: "system_marker",
      at: "2026-05-16T00:00:04Z",
      rank: 2,
      key: "s:mark",
      agent_id: A,
      marker: { marker_kind: "compaction", summary: "recap" },
    };
    expect(buildNavigatorEntries([outcome, marker], NAMES)).toEqual([]);
  });
});

describe("filterEntries", () => {
  const entries = buildNavigatorEntries(
    buildUnifiedRows(
      [
        userTurn(A, "Fix the   Login\nBug please", "send-1", "2026-05-16T00:00:00Z"),
        agentTurn(A, "turn-a", "2026-05-16T00:00:01Z", [
          { item_kind: "text", kind: "text", text: "Deployed the fix" },
        ]),
      ],
      [],
    ),
    NAMES,
  );

  it("matches case-insensitively across collapsed whitespace, including line breaks", () => {
    expect(filterEntries(entries, "login bug", "all")).toHaveLength(1);
    expect(filterEntries(entries, "  LOGIN   BUG  ", "all")).toHaveLength(1);
    expect(filterEntries(entries, "login bugx", "all")).toHaveLength(0);
  });

  it("restricts by role, composing with the query", () => {
    expect(filterEntries(entries, "", "user")).toHaveLength(1);
    expect(filterEntries(entries, "", "agent")).toHaveLength(1);
    expect(filterEntries(entries, "fix", "all")).toHaveLength(2);
    expect(filterEntries(entries, "fix", "agent").map((e) => e.attribution)).toEqual(["alice"]);
  });
});
