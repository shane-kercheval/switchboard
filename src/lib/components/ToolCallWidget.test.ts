import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, fireEvent } from "@testing-library/svelte";
import type { ToolCall } from "$lib/state/types";
import type { ToolFacet } from "$lib/types";
import ToolCallWidget from "./ToolCallWidget.svelte";

// Wrap the real formatter in a spy so the eager-stringify regression test can
// assert it is never invoked for a collapsed row (the bug being replaced
// formatted every tool call's raw input whether or not it was expanded).
const formatToolInputSpy = vi.hoisted(() => vi.fn());
vi.mock("$lib/toolInput", async (importOriginal) => {
  const mod = await importOriginal<typeof import("$lib/toolInput")>();
  formatToolInputSpy.mockImplementation(mod.formatToolInput);
  return { ...mod, formatToolInput: formatToolInputSpy };
});

const running: ToolCall = {
  item_kind: "tool",
  facet: { facet_kind: "other" },
  tool_use_id: "t1",
  kind: "builtin",
  name: "Bash",
  input: { command: "sleep 1" },
  started_at: "2026-05-16T00:00:01Z",
};
const done: ToolCall = {
  ...running,
  output: "hi\n",
  is_error: false,
  completed_at: "2026-05-16T00:00:02Z",
};
const cancelled: ToolCall = {
  ...running,
  stopped_at: "2026-05-16T00:00:02Z",
  stop_reason: "cancelled",
};
const stoppedFailed: ToolCall = {
  ...running,
  stopped_at: "2026-05-16T00:00:02Z",
  stop_reason: "failed",
};

function withFacet(facet: ToolFacet, overrides: Partial<ToolCall> = {}): ToolCall {
  return { ...done, facet, ...overrides };
}

const EDIT_FACET: ToolFacet = {
  facet_kind: "edit",
  files: [
    {
      path: "/repo/src/a.ts",
      change: "modified",
      edits: [{ old: "line one\nline two\n", new: "line one\nline 2\n" }],
      truncated: false,
    },
  ],
};

describe("ToolCallWidget collapsed row", () => {
  // Detail is facet-derived and never repeats the verb or the raw tool name —
  // the raw name lives in the expanded raw-input label instead.
  const cases: { facet: ToolFacet; verb: string; detail: string | null }[] = [
    { facet: { facet_kind: "shell", command: "ls", cwd: null }, verb: "Command", detail: "ls" },
    { facet: EDIT_FACET, verb: "Edit", detail: null },
    {
      facet: { facet_kind: "write", path: "/repo/x", content: "c", truncated: false },
      verb: "Write",
      detail: null,
    },
    { facet: { facet_kind: "read", path: "/repo/x" }, verb: "Read", detail: "/repo/x" },
    {
      facet: { facet_kind: "search", pattern: "todo", path: null },
      verb: "Search",
      detail: "todo",
    },
    { facet: { facet_kind: "todo", items: [] }, verb: "Todos", detail: null },
    {
      facet: { facet_kind: "mcp", server: "linear", tool: "create_issue" },
      verb: "linear · create_issue",
      detail: "sleep 1",
    },
    { facet: { facet_kind: "other" }, verb: "Bash", detail: "sleep 1" },
  ];

  for (const { facet, verb, detail } of cases) {
    it(`shows verb, detail, and status for the ${facet.facet_kind} facet`, () => {
      const { getByTestId, queryByTestId } = render(ToolCallWidget, { tool: withFacet(facet) });
      expect(getByTestId("tool-verb")).toHaveTextContent(verb);
      if (detail === null) {
        expect(queryByTestId("tool-detail")).toBeNull();
      } else {
        expect(getByTestId("tool-detail")).toHaveTextContent(detail);
      }
      expect(getByTestId("tool-done")).toBeInTheDocument();
    });
  }

  it("shows the input preview as the detail without a raw-name prefix", () => {
    const { getByTestId } = render(ToolCallWidget, {
      tool: { ...done, input: { command: "git log --oneline -3" } },
    });
    expect(getByTestId("tool-detail")).toHaveTextContent("git log --oneline -3");
    expect(getByTestId("tool-detail")).not.toHaveTextContent("Bash:");
  });

  it("truncates a long detail rather than wrapping", () => {
    const { getByTestId } = render(ToolCallWidget, {
      tool: { ...done, input: { command: `echo ${"x".repeat(500)}` } },
    });
    // jsdom has no layout, so assert the truncation contract via classes: a
    // single-line ellipsis needs `truncate` and a shrinkable `min-w-0`.
    const detail = getByTestId("tool-detail");
    expect(detail.className).toContain("truncate");
    expect(detail.className).toContain("min-w-0");
  });

  it("degrades an unknown facet discriminant to the raw tool name", () => {
    const facet = { facet_kind: "hologram" } as unknown as ToolFacet;
    const { getByTestId } = render(ToolCallWidget, { tool: withFacet(facet) });
    expect(getByTestId("tool-verb")).toHaveTextContent("Bash");
  });

  it("gives an unknown facet the full generic treatment on expand", async () => {
    // Forward-compat contract: a facet kind this build doesn't know must
    // behave exactly like the generic path — raw input directly on expand,
    // no extra reveal — never a lesser degradation.
    const facet = { facet_kind: "hologram" } as unknown as ToolFacet;
    const { getByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: withFacet(facet, { input: { marker: "raw-envelope" } }),
    });
    await fireEvent.click(getByTestId("tool-row"));

    expect(getByTestId("tool-input")).toHaveTextContent('"marker": "raw-envelope"');
    expect(queryByTestId("tool-raw-toggle")).toBeNull();
  });
});

describe("ToolCallWidget status glyphs", () => {
  it("shows a spinner while running", () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, { tool: running });
    expect(getByTestId("tool-running")).toBeInTheDocument();
    expect(queryByTestId("tool-done")).toBeNull();
  });

  it("shows the failed glyph for an is_error completion", () => {
    const { getByTestId } = render(ToolCallWidget, {
      tool: { ...done, is_error: true, output: "boom" },
    });
    expect(getByTestId("tool-error")).toBeInTheDocument();
  });

  it("renders a cancelled tool with the in-progress verb and cancelled glyph", () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: { ...cancelled, facet: { facet_kind: "shell", command: "ls", cwd: null } },
    });
    expect(queryByTestId("tool-running")).toBeNull();
    expect(getByTestId("tool-cancelled")).toBeInTheDocument();
    // The label is state-invariant; the cancelled glyph carries the outcome.
    expect(getByTestId("tool-verb")).toHaveTextContent("Command");
  });

  it("renders a stopped-failed tool with the failed glyph and unchanged label", () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: { ...stoppedFailed, facet: { facet_kind: "shell", command: "ls", cwd: null } },
    });
    expect(queryByTestId("tool-running")).toBeNull();
    expect(getByTestId("tool-error")).toBeInTheDocument();
    expect(getByTestId("tool-verb")).toHaveTextContent("Command");
  });
});

describe("ToolCallWidget expansion", () => {
  it("starts collapsed with no body and stays collapsed across completion", async () => {
    const { getByTestId, queryByTestId, rerender } = render(ToolCallWidget, { tool: running });
    expect(queryByTestId("tool-body")).toBeNull();

    await rerender({ tool: done });
    expect(queryByTestId("tool-body")).toBeNull();
    expect(getByTestId("tool-row")).toHaveAttribute("aria-expanded", "false");
  });

  it("hides the detail line while expanded and restores it on collapse", async () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: withFacet({ facet_kind: "shell", command: "git status", cwd: null }),
    });
    expect(getByTestId("tool-detail")).toHaveTextContent("git status");

    // The body shows the full untruncated command, so keeping the detail
    // would duplicate it on adjacent lines.
    await fireEvent.click(getByTestId("tool-row"));
    expect(queryByTestId("tool-detail")).toBeNull();
    expect(getByTestId("tool-command")).toHaveTextContent("git status");

    await fireEvent.click(getByTestId("tool-row"));
    expect(getByTestId("tool-detail")).toHaveTextContent("git status");
  });

  it("keeps a user-opened panel open across completion", async () => {
    const { getByTestId, rerender } = render(ToolCallWidget, { tool: running });
    await fireEvent.click(getByTestId("tool-row"));
    expect(getByTestId("tool-body")).toBeInTheDocument();

    await rerender({ tool: done });
    expect(getByTestId("tool-body")).toBeInTheDocument();
  });

  it("shows raw input directly on expand for the generic facet", async () => {
    const { getByTestId } = render(ToolCallWidget, {
      tool: { ...done, input: { file_path: "/tmp/file.txt", old_string: "before" } },
    });
    await fireEvent.click(getByTestId("tool-row"));

    expect(getByTestId("tool-input")).toHaveTextContent('"file_path": "/tmp/file.txt"');
    expect(getByTestId("tool-output")).toHaveTextContent("hi");
  });

  it("reaches raw input behind the toggle for every specialized facet", async () => {
    const facets: ToolFacet[] = [
      { facet_kind: "shell", command: "ls", cwd: null },
      EDIT_FACET,
      { facet_kind: "write", path: "/repo/x", content: "c", truncated: false },
      { facet_kind: "read", path: "/repo/x" },
      { facet_kind: "search", pattern: "todo", path: null },
      { facet_kind: "todo", items: [] },
      { facet_kind: "mcp", server: "linear", tool: "create_issue" },
    ];
    for (const facet of facets) {
      const { getByTestId, queryByTestId, unmount } = render(ToolCallWidget, {
        tool: withFacet(facet, { input: { marker: "raw-envelope" } }),
      });
      await fireEvent.click(getByTestId("tool-row"));
      expect(queryByTestId("tool-input")).toBeNull();

      await fireEvent.click(getByTestId("tool-raw-toggle"));
      expect(getByTestId("tool-input")).toHaveTextContent('"marker": "raw-envelope"');
      expect(getByTestId("tool-raw-name")).toHaveTextContent("Bash");
      unmount();
    }
  });

  it("suppresses the output section when output is empty", async () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: { ...done, output: "" },
    });
    await fireEvent.click(getByTestId("tool-row"));
    expect(queryByTestId("tool-output")).toBeNull();
  });
});

describe("ToolCallWidget lazy raw input", () => {
  it("does not format a huge input while collapsed", () => {
    formatToolInputSpy.mockClear();
    const huge = { blob: "x".repeat(2_000_000) };
    const { queryByTestId } = render(ToolCallWidget, { tool: { ...done, input: huge } });

    expect(queryByTestId("tool-input")).toBeNull();
    expect(formatToolInputSpy).not.toHaveBeenCalled();
  });

  it("caps the displayed raw input with a truncation notice when expanded", async () => {
    const huge = { blob: "x".repeat(2_000_000) };
    const { getByTestId } = render(ToolCallWidget, { tool: { ...done, input: huge } });
    await fireEvent.click(getByTestId("tool-row"));

    const rendered = getByTestId("tool-input").textContent ?? "";
    expect(rendered.length).toBeLessThan(100_000);
    expect(getByTestId("tool-input-truncated")).toBeInTheDocument();
  });

  it("shows no truncation notice for a small input", async () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, { tool: done });
    await fireEvent.click(getByTestId("tool-row"));
    expect(getByTestId("tool-input")).toHaveTextContent('"command": "sleep 1"');
    expect(queryByTestId("tool-input-truncated")).toBeNull();
  });
});

describe("ToolCallWidget facet bodies", () => {
  it("renders a shell body with the full command and cwd", async () => {
    const { getByTestId } = render(ToolCallWidget, {
      tool: withFacet({ facet_kind: "shell", command: "git status", cwd: "/repo" }),
    });
    await fireEvent.click(getByTestId("tool-row"));
    expect(getByTestId("tool-command")).toHaveTextContent("git status");
    expect(getByTestId("tool-body")).toHaveTextContent("in /repo");
  });

  it("redacts secrets in the displayed shell command", async () => {
    const { getByTestId } = render(ToolCallWidget, {
      tool: withFacet({
        facet_kind: "shell",
        command: "curl -H 'Authorization: Bearer abc123secret' https://api.example.com",
        cwd: null,
      }),
    });
    await fireEvent.click(getByTestId("tool-row"));
    expect(getByTestId("tool-command")).toHaveTextContent("[redacted]");
    expect(getByTestId("tool-command")).not.toHaveTextContent("abc123secret");
  });

  it("renders an edit as a diff inline, without expanding", () => {
    const { getByTestId, queryByTestId, container } = render(ToolCallWidget, {
      tool: withFacet(EDIT_FACET),
    });

    // The diff is ambient; output and raw input stay behind expansion.
    expect(queryByTestId("tool-output")).toBeNull();
    expect(queryByTestId("tool-raw-toggle")).toBeNull();

    const removed = Array.from(container.querySelectorAll('[data-origin="removed"]'));
    const added = Array.from(container.querySelectorAll('[data-origin="added"]'));
    expect(removed.map((el) => el.textContent)).toEqual(
      expect.arrayContaining([expect.stringContaining("line two")]),
    );
    expect(added.map((el) => el.textContent)).toEqual(
      expect.arrayContaining([expect.stringContaining("line 2")]),
    );
    // Compact diff: no hunk-header bar, no line-number gutters — snippet-
    // relative numbers would read as file positions.
    expect(getByTestId("tool-body")).not.toHaveTextContent("@@");
    expect(getByTestId("tool-body")).not.toHaveTextContent("snippet-relative");
  });

  it("labels a single-file creation as Write without a change marker", () => {
    const facet: ToolFacet = {
      facet_kind: "edit",
      files: [
        {
          path: "/repo/new.txt",
          change: "added",
          edits: [{ old: "", new: "hi\n" }],
          truncated: false,
        },
      ],
    };
    const { getByTestId } = render(ToolCallWidget, { tool: withFacet(facet) });
    expect(getByTestId("tool-verb")).toHaveTextContent("Write");
    expect(getByTestId("tool-body")).not.toHaveTextContent("(added)");
  });

  it("labels a single-file deletion as Delete", () => {
    const facet: ToolFacet = {
      facet_kind: "edit",
      files: [
        {
          path: "/repo/old.txt",
          change: "deleted",
          edits: [{ old: "bye\n", new: "" }],
          truncated: false,
        },
      ],
    };
    const { getByTestId } = render(ToolCallWidget, { tool: withFacet(facet) });
    expect(getByTestId("tool-verb")).toHaveTextContent("Delete");
    expect(getByTestId("tool-body")).not.toHaveTextContent("(deleted)");
  });

  it("caps a large edit diff to a preview and reveals the rest on expand", async () => {
    // A 60-line insertion is past the 25-line inline preview cap.
    const big = Array.from({ length: 60 }, (_, i) => `line ${i}`).join("\n") + "\n";
    const facet: ToolFacet = {
      facet_kind: "edit",
      files: [
        { path: "/repo/big.ts", change: "added", edits: [{ old: "", new: big }], truncated: false },
      ],
    };
    const { getByTestId, queryByTestId, container } = render(ToolCallWidget, {
      tool: withFacet(facet),
    });

    // Collapsed: capped preview shows a "Show N more lines" affordance and only
    // the first 25 diff lines are rendered.
    const expand = getByTestId("tool-edit-expand");
    expect(expand).toHaveTextContent("Show 35 more lines");
    expect(container.querySelectorAll('[data-origin="added"]')).toHaveLength(25);

    // Expanding via the hint opens the row and renders the full diff.
    await fireEvent.click(expand);
    expect(queryByTestId("tool-edit-expand")).toBeNull();
    expect(container.querySelectorAll('[data-origin="added"]')).toHaveLength(60);
  });

  it("does not cap an edit diff that fits under the preview limit", () => {
    const { queryByTestId, container } = render(ToolCallWidget, { tool: withFacet(EDIT_FACET) });
    expect(queryByTestId("tool-edit-expand")).toBeNull();
    expect(container.querySelector('[data-origin="added"]')).not.toBeNull();
  });

  it("reveals output and raw input on an edit row only when expanded", async () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: withFacet(EDIT_FACET, { output: "edit applied" }),
    });
    expect(queryByTestId("tool-output")).toBeNull();

    await fireEvent.click(getByTestId("tool-row"));
    expect(getByTestId("tool-output")).toHaveTextContent("edit applied");
    expect(getByTestId("tool-raw-toggle")).toBeInTheDocument();
    // The diff stays visible in both states.
    expect(getByTestId("tool-edit-file")).toBeInTheDocument();
  });

  it("renders one diff section per file for a multi-file edit", async () => {
    const facet: ToolFacet = {
      facet_kind: "edit",
      files: [
        {
          path: "/repo/a.ts",
          change: "modified",
          edits: [{ old: "a\n", new: "b\n" }],
          truncated: false,
        },
        {
          path: "/repo/b.ts",
          change: "added",
          edits: [{ old: "", new: "new file\n" }],
          truncated: false,
        },
      ],
    };
    const { getByTestId, getAllByTestId } = render(ToolCallWidget, { tool: withFacet(facet) });

    const sections = getAllByTestId("tool-edit-file");
    expect(sections).toHaveLength(2);
    expect(sections[1]).toHaveTextContent("(added)");
    expect(getByTestId("tool-verb")).toHaveTextContent("Edit");
  });

  it("shows a pending placeholder for a content-less edit on a live turn", async () => {
    const facet: ToolFacet = {
      facet_kind: "edit",
      files: [{ path: "/repo/a.ts", change: "modified", edits: [], truncated: false }],
    };
    const { getByTestId } = render(ToolCallWidget, {
      tool: withFacet(facet),
      turnSettled: false,
    });
    expect(getByTestId("tool-edit-pending")).toHaveTextContent(
      "Diff will appear when the turn completes.",
    );
  });

  it("shows an unavailable notice for a content-less edit on a settled turn", async () => {
    const facet: ToolFacet = {
      facet_kind: "edit",
      files: [{ path: "/repo/a.ts", change: "modified", edits: [], truncated: false }],
    };
    const { getByTestId } = render(ToolCallWidget, { tool: withFacet(facet), turnSettled: true });
    expect(getByTestId("tool-edit-pending")).toHaveTextContent(
      "Diff content unavailable for this edit.",
    );
  });

  it("renders write content inline without claiming the file was created", () => {
    const { getByTestId, queryByTestId, container } = render(ToolCallWidget, {
      tool: withFacet({
        facet_kind: "write",
        path: "/repo/new.txt",
        content: "first line\nsecond line\n",
        truncated: false,
      }),
    });
    expect(getByTestId("tool-write-file")).toHaveTextContent("/repo/new.txt");
    expect(queryByTestId("tool-detail")).toBeNull();
    expect(getByTestId("tool-write-content").textContent).toBe("first line\nsecond line\n");
    expect(container.querySelector('[data-origin="added"]')).toBeNull();
    expect(container.querySelector('[data-origin="removed"]')).toBeNull();
  });

  it("caps a large write preview and reveals all lines on expand", async () => {
    const content = Array.from({ length: 60 }, (_, i) => `line ${i}`).join("\n") + "\n";
    const { getByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: withFacet({ facet_kind: "write", path: "/repo/big.txt", content, truncated: false }),
    });

    expect(getByTestId("tool-write-expand")).toHaveTextContent("Show 35 more lines");
    expect(getByTestId("tool-write-content")).toHaveTextContent("line 24");
    expect(getByTestId("tool-write-content")).not.toHaveTextContent("line 25");

    await fireEvent.click(getByTestId("tool-write-expand"));
    expect(queryByTestId("tool-write-expand")).toBeNull();
    expect(getByTestId("tool-write-content")).toHaveTextContent("line 59");
  });

  it("shows the facet truncation notice on inline write content", () => {
    const { getByTestId } = render(ToolCallWidget, {
      tool: withFacet({
        facet_kind: "write",
        path: "/repo/big.txt",
        content: "prefix of the file",
        truncated: true,
      }),
    });
    expect(getByTestId("tool-write-truncated")).toHaveTextContent("Content truncated");
  });

  it("renders a todo facet as a checklist", async () => {
    const facet: ToolFacet = {
      facet_kind: "todo",
      items: [
        { content: "ship it", status: "completed" },
        { content: "test it", status: "in_progress" },
        { content: "doc it", status: "pending" },
      ],
    };
    const { getByTestId } = render(ToolCallWidget, { tool: withFacet(facet) });
    await fireEvent.click(getByTestId("tool-row"));

    const list = getByTestId("tool-todo");
    expect(list.querySelectorAll("li")).toHaveLength(3);
    expect(list).toHaveTextContent("ship it");
    expect(list).toHaveTextContent("test it");
    expect(list).toHaveTextContent("doc it");
  });

  it("renders read and search facet details", async () => {
    const read = render(ToolCallWidget, {
      tool: withFacet({ facet_kind: "read", path: "/repo/src/main.rs" }),
    });
    await fireEvent.click(read.getByTestId("tool-row"));
    expect(read.getByTestId("tool-read-path")).toHaveTextContent("/repo/src/main.rs");
    read.unmount();

    const search = render(ToolCallWidget, {
      tool: withFacet({ facet_kind: "search", pattern: "TODO", path: "/repo/src" }),
    });
    await fireEvent.click(search.getByTestId("tool-row"));
    expect(search.getByTestId("tool-search-detail")).toHaveTextContent("TODO in /repo/src");
  });
});
