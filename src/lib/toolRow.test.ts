import { describe, expect, it } from "vitest";
import { FilePen, Wrench } from "@lucide/svelte";
import type { ToolCall } from "$lib/state/types";
import type { ToolFacet } from "$lib/types";
import { toolDetail, toolIcon, toolRowState, toolVerb } from "$lib/toolRow";

const BASE: ToolCall = {
  item_kind: "tool",
  facet: { facet_kind: "other" },
  tool_use_id: "t1",
  kind: "builtin",
  name: "Bash",
  input: { command: "ls" },
  started_at: "2026-05-16T00:00:01Z",
};

describe("toolRowState", () => {
  it("is running with neither completed_at nor stopped_at", () => {
    expect(toolRowState(BASE)).toBe("running");
  });

  it("is done on clean completion", () => {
    expect(toolRowState({ ...BASE, completed_at: "2026-05-16T00:00:02Z", is_error: false })).toBe(
      "done",
    );
  });

  it("is failed on is_error completion", () => {
    expect(toolRowState({ ...BASE, completed_at: "2026-05-16T00:00:02Z", is_error: true })).toBe(
      "failed",
    );
  });

  it("is failed when stopped with stop_reason failed", () => {
    expect(
      toolRowState({ ...BASE, stopped_at: "2026-05-16T00:00:02Z", stop_reason: "failed" }),
    ).toBe("failed");
  });

  it("is cancelled when stopped with stop_reason cancelled", () => {
    expect(
      toolRowState({ ...BASE, stopped_at: "2026-05-16T00:00:02Z", stop_reason: "cancelled" }),
    ).toBe("cancelled");
  });
});

describe("toolVerb", () => {
  it("maps each facet to its fixed state-invariant label", () => {
    expect(toolVerb({ facet_kind: "shell", command: "ls", cwd: null }, "x")).toBe("Command");
    expect(toolVerb({ facet_kind: "edit", files: [] }, "x")).toBe("Edit");
    expect(toolVerb({ facet_kind: "write", path: "/x", content: "", truncated: false }, "x")).toBe(
      "Write",
    );
    expect(toolVerb({ facet_kind: "read", path: "/x" }, "x")).toBe("Read");
    expect(toolVerb({ facet_kind: "search", pattern: "p", path: null }, "x")).toBe("Search");
    expect(toolVerb({ facet_kind: "todo", items: [] }, "x")).toBe("Todos");
  });

  it("renders the mcp facet as the server/tool pair", () => {
    expect(toolVerb({ facet_kind: "mcp", server: "linear", tool: "create_issue" }, "x")).toBe(
      "linear · create_issue",
    );
  });

  it("uses the raw tool name for the generic facet", () => {
    expect(toolVerb({ facet_kind: "other" }, "Task")).toBe("Task");
  });

  it("degrades an unknown facet discriminant to the raw tool name", () => {
    const unknown = { facet_kind: "hologram" } as unknown as ToolFacet;
    expect(toolVerb(unknown, "FutureTool")).toBe("FutureTool");
  });
});

describe("toolDetail", () => {
  it("shows the command for a shell facet, collapsed to one line", () => {
    const facet: ToolFacet = { facet_kind: "shell", command: "git log \\\n  --oneline", cwd: null };
    expect(toolDetail(facet, {})).toBe("git log \\ --oneline");
  });

  it("redacts secrets in a shell command", () => {
    const facet: ToolFacet = {
      facet_kind: "shell",
      command: "curl -H 'Authorization: Bearer abc123secret' https://api.example.com",
      cwd: null,
    };
    expect(toolDetail(facet, {})).toContain("[redacted]");
    expect(toolDetail(facet, {})).not.toContain("abc123secret");
  });

  it("joins file paths for an edit facet and is undefined with no files", () => {
    const one: ToolFacet = {
      facet_kind: "edit",
      files: [{ path: "/repo/a.ts", change: "modified", edits: [], truncated: false }],
    };
    const two: ToolFacet = {
      facet_kind: "edit",
      files: [
        { path: "/repo/a.ts", change: "modified", edits: [], truncated: false },
        { path: "/repo/b.ts", change: "added", edits: [], truncated: false },
      ],
    };
    expect(toolDetail(one, {})).toBe("/repo/a.ts");
    expect(toolDetail(two, {})).toBe("/repo/a.ts, /repo/b.ts");
    expect(toolDetail({ facet_kind: "edit", files: [] }, {})).toBeUndefined();
  });

  it("shows the path for write and read facets", () => {
    expect(
      toolDetail({ facet_kind: "write", path: "/repo/x.txt", content: "", truncated: false }, {}),
    ).toBe("/repo/x.txt");
    expect(toolDetail({ facet_kind: "read", path: "/repo/y.txt" }, {})).toBe("/repo/y.txt");
  });

  it("shows pattern and optional scope for a search facet", () => {
    expect(toolDetail({ facet_kind: "search", pattern: "TODO", path: "/repo/src" }, {})).toBe(
      "TODO in /repo/src",
    );
    expect(toolDetail({ facet_kind: "search", pattern: "TODO", path: null }, {})).toBe("TODO");
  });

  it("summarizes todos as the single item's content or a count", () => {
    expect(
      toolDetail({ facet_kind: "todo", items: [{ content: "ship it", status: "pending" }] }, {}),
    ).toBe("ship it");
    expect(
      toolDetail(
        {
          facet_kind: "todo",
          items: [
            { content: "a", status: "pending" },
            { content: "b", status: "completed" },
          ],
        },
        {},
      ),
    ).toBe("2 items");
    expect(toolDetail({ facet_kind: "todo", items: [] }, {})).toBeUndefined();
  });

  it("falls back to the redacted input preview for mcp, other, and unknown facets", () => {
    expect(toolDetail({ facet_kind: "other" }, { command: "echo hi" })).toBe("echo hi");
    expect(
      toolDetail({ facet_kind: "mcp", server: "s", tool: "t" }, { query: "find things" }),
    ).toBe("find things");
    const unknown = { facet_kind: "hologram" } as unknown as ToolFacet;
    expect(toolDetail(unknown, { command: "echo hi" })).toBe("echo hi");
  });
});

describe("toolIcon", () => {
  it("maps known facets and degrades unknown discriminants to the generic icon", () => {
    expect(toolIcon({ facet_kind: "edit", files: [] })).toBe(FilePen);
    expect(toolIcon({ facet_kind: "other" })).toBe(Wrench);
    expect(toolIcon({ facet_kind: "hologram" } as unknown as ToolFacet)).toBe(Wrench);
  });
});
