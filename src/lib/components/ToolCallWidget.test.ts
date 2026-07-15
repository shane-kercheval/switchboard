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

function mcpTextEdit(
  target = "note · note-example",
  overrides: Partial<{
    target_truncated: boolean;
    before: string;
    after: string;
    content_truncated: boolean;
  }> = {},
): ToolFacet {
  return {
    facet_kind: "mcp",
    server: "notes_alias",
    tool: "edit_content",
    mutation: {
      mutation_kind: "text_edit",
      target,
      target_truncated: false,
      before: "old line\n",
      after: "new line\n",
      content_truncated: false,
      ...overrides,
    },
  };
}

function mcpTextCreation(
  target = "note · New note",
  content = "# Heading\n\nCreated body\n",
  contentTruncated = false,
): ToolFacet {
  return {
    facet_kind: "mcp",
    server: "notes_alias",
    tool: "create_note",
    mutation: {
      mutation_kind: "text_creation",
      target,
      target_truncated: false,
      content,
      content_truncated: contentTruncated,
    },
  };
}

function mcpRecordCreation(
  fieldsTruncated = false,
  fields = [
    { label: "Title", value: "Example" },
    { label: "URL", value: "https://example.com" },
    { label: "Description", value: "Useful reference" },
    { label: "Tags", value: "research, example" },
  ],
): ToolFacet {
  return {
    facet_kind: "mcp",
    server: "notes_alias",
    tool: "create_bookmark",
    mutation: {
      mutation_kind: "record_creation",
      target: "bookmark · Example",
      target_truncated: false,
      fields,
      fields_truncated: fieldsTruncated,
    },
  };
}

function withMcpFacet(facet: ToolFacet, overrides: Partial<ToolCall> = {}): ToolCall {
  return withFacet(facet, {
    kind: "mcp",
    name: "mcp__notes_alias__mutation",
    input: { marker: "raw MCP input" },
    output: "minimal server output",
    ...overrides,
  });
}

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

describe("ToolCallWidget failed and cancelled output", () => {
  it("shows a one-line failed-output preview and reveals the full error on expand", async () => {
    const { getByTestId, queryByTestId, getByLabelText } = render(ToolCallWidget, {
      tool: {
        ...done,
        is_error: true,
        output: "first failure line\n  second detail line",
      },
    });

    const preview = getByTestId("tool-status-preview");
    expect(preview).toHaveTextContent("first failure line second detail line");
    expect(preview.className).toContain("truncate");
    expect(queryByTestId("tool-output")).toBeNull();

    await fireEvent.click(getByTestId("tool-row"));
    expect(queryByTestId("tool-status-preview")).toBeNull();
    expect(getByLabelText("Tool error")).toHaveTextContent("Error");
    expect(getByTestId("tool-output")).toHaveTextContent("first failure line");
    expect(getByTestId("tool-output")).toHaveTextContent("second detail line");
  });

  it("shows a fallback when a failed tool has no error output", async () => {
    const { getByTestId } = render(ToolCallWidget, { tool: stoppedFailed });

    expect(getByTestId("tool-status-preview")).toHaveTextContent(
      "Tool failed without error details.",
    );

    await fireEvent.click(getByTestId("tool-row"));
    expect(getByTestId("tool-status-preview")).toHaveTextContent(
      "Tool failed without error details.",
    );
  });

  it("bounds a large failed-output preview while collapsed", () => {
    const output = `first failure line\n${"x".repeat(2_000_000)}`;
    const { getByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: { ...done, is_error: true, output },
    });

    const preview = getByTestId("tool-status-preview").textContent ?? "";
    expect(preview).toContain("first failure line");
    expect(preview).toHaveLength(241);
    expect(preview.endsWith("…")).toBe(true);
    expect(queryByTestId("tool-output")).toBeNull();
  });

  it("treats whitespace-only failed output as missing when collapsed and expanded", async () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: { ...done, is_error: true, output: " \n\t ".repeat(500_000) },
    });

    expect(getByTestId("tool-status-preview")).toHaveTextContent(
      "Tool failed without error details.",
    );
    expect(queryByTestId("tool-output")).toBeNull();

    await fireEvent.click(getByTestId("tool-row"));
    expect(getByTestId("tool-status-preview")).toHaveTextContent(
      "Tool failed without error details.",
    );
    expect(queryByTestId("tool-output")).toBeNull();
  });

  it("reveals meaningful output beyond the collapsed inspection cap when expanded", async () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: { ...done, is_error: true, output: `${" ".repeat(3_000)}late failure detail` },
    });

    expect(getByTestId("tool-status-preview")).toHaveTextContent(
      "Tool failed without error details.",
    );

    await fireEvent.click(getByTestId("tool-row"));
    expect(queryByTestId("tool-status-preview")).toBeNull();
    expect(getByTestId("tool-output")).toHaveTextContent("late failure detail");
  });

  for (const facet of [
    EDIT_FACET,
    {
      facet_kind: "write",
      path: "/repo/new.txt",
      content: "content that was not written",
      truncated: false,
    } satisfies ToolFacet,
  ]) {
    it(`suppresses a failed ${facet.facet_kind} diff while retaining error and raw input`, async () => {
      const { getByTestId, queryByTestId } = render(ToolCallWidget, {
        tool: withFacet(facet, {
          is_error: true,
          output: "permission denied",
          input: { attempted: true },
        }),
      });

      expect(getByTestId("tool-status-preview")).toHaveTextContent("permission denied");
      expect(queryByTestId("tool-edit-file")).toBeNull();
      expect(queryByTestId("tool-write-file")).toBeNull();

      await fireEvent.click(getByTestId("tool-row"));
      expect(getByTestId("tool-output")).toHaveTextContent("permission denied");
      expect(getByTestId("tool-raw-toggle")).toBeInTheDocument();
      expect(queryByTestId("tool-edit-file")).toBeNull();
      expect(queryByTestId("tool-write-file")).toBeNull();

      await fireEvent.click(getByTestId("tool-raw-toggle"));
      expect(getByTestId("tool-input")).toHaveTextContent('"attempted": true');
    });
  }

  it("suppresses a cancelled edit diff and shows a cancelled message", () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: { ...cancelled, facet: EDIT_FACET },
    });

    expect(getByTestId("tool-status-preview")).toHaveTextContent(
      "Tool cancelled before completion.",
    );
    expect(queryByTestId("tool-edit-file")).toBeNull();
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

  it("infers a dedicated write is a creation and renders every line as added", () => {
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
    expect(getByTestId("tool-write-content")).toHaveTextContent("first line");
    expect(getByTestId("tool-write-content")).toHaveTextContent("second line");
    expect(container.querySelectorAll('[data-origin="added"]')).toHaveLength(2);
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
    expect(getByTestId("diff-truncated")).toHaveTextContent("Diff truncated");
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

describe("ToolCallWidget MCP mutation bodies", () => {
  it.each([
    ["note", mcpTextEdit("note · note-example"), "note · note-example"],
    ["bookmark", mcpTextEdit("bookmark · bookmark-example"), "bookmark · bookmark-example"],
    [
      "prompt",
      {
        facet_kind: "mcp",
        server: "prompts_alias",
        tool: "edit_prompt_content",
        mutation: {
          mutation_kind: "text_edit",
          target: "prompt · review-code",
          target_truncated: false,
          before: "Review {{ old }}\n",
          after: "Review {{ changes }}\n",
          content_truncated: false,
        },
      } satisfies ToolFacet,
      "prompt · review-code",
    ],
  ])("renders a %s text edit as an inline compact diff", (_kind, facet, target) => {
    const { getByTestId, queryByTestId, container } = render(ToolCallWidget, {
      tool: withMcpFacet(facet),
    });

    expect(getByTestId("tool-mcp-edit")).toBeInTheDocument();
    expect(getByTestId("tool-detail")).toHaveTextContent(target);
    expect(queryByTestId("tool-output")).toBeNull();
    expect(container.querySelector('[data-origin="removed"]')).not.toBeNull();
    expect(container.querySelector('[data-origin="added"]')).not.toBeNull();
    expect(getByTestId("tool-body")).not.toHaveTextContent("snippet-relative");
  });

  it.each([
    ["note", mcpTextCreation("note · Release notes", "# Release\n\nReady.\n")],
    [
      "prompt",
      {
        facet_kind: "mcp",
        server: "prompts_alias",
        tool: "create_prompt",
        mutation: {
          mutation_kind: "text_creation",
          target: "prompt · summarize",
          target_truncated: false,
          content: "Summarize {{ context }}\n",
          content_truncated: false,
        },
      } satisfies ToolFacet,
    ],
  ])("renders a %s creation as all-added Markdown content", (kind, facet) => {
    const { getByTestId, container } = render(ToolCallWidget, {
      tool: withMcpFacet(facet),
    });

    expect(getByTestId("tool-mcp-creation-content")).toBeInTheDocument();
    expect(container.querySelectorAll('[data-origin="added"]')).not.toHaveLength(0);
    expect(container.querySelector('[data-origin="removed"]')).toBeNull();
    if (kind === "note") {
      expect(container.querySelector(".token")).not.toBeNull();
    } else {
      expect(getByTestId("tool-mcp-creation-content")).toHaveTextContent("{{ context }}");
    }
  });

  it("renders bookmark creation as structured added fields, not a diff", () => {
    const { getByTestId, getAllByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: withMcpFacet(mcpRecordCreation()),
    });

    const record = getByTestId("tool-mcp-record-creation");
    expect(record).toHaveTextContent("Title");
    expect(record).toHaveTextContent("https://example.com");
    expect(getAllByTestId("tool-mcp-record-field")).toHaveLength(4);
    expect(queryByTestId("diff-view")).toBeNull();
  });

  it("keeps an empty text creation legible without mounting an empty diff", () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: withMcpFacet(mcpTextCreation("note · Empty note", "")),
    });

    expect(getByTestId("tool-detail")).toHaveTextContent("note · Empty note");
    expect(getByTestId("tool-mcp-empty-creation")).toHaveTextContent(
      "Created without body content.",
    );
    expect(queryByTestId("diff-view")).toBeNull();
  });

  it("mounts only the bounded target plus an ellipsis and never puts it in a title", async () => {
    const boundedTarget = `note · ${"é".repeat(233)}`;
    const fullTarget = `${boundedTarget}${"é".repeat(2000)}`;
    const facet = mcpTextEdit(boundedTarget, { target_truncated: true });
    const { getByTestId, queryByTestId, container } = render(ToolCallWidget, {
      tool: withMcpFacet(facet, { input: { title: fullTarget } }),
    });

    const detail = getByTestId("tool-detail");
    expect(detail).toHaveTextContent(`${boundedTarget}…`);
    expect(detail).not.toHaveAttribute("title");
    expect(container.textContent).not.toContain(fullTarget);
    expect(queryByTestId("diff-truncated")).toBeNull();

    await fireEvent.click(getByTestId("tool-row"));
    expect(getByTestId("tool-mcp-target")).toHaveTextContent(`${boundedTarget}…`);
    expect(getByTestId("tool-mcp-target")).not.toHaveAttribute("title");
    expect(container.textContent).not.toContain(fullTarget);
  });

  it("uses content truncation, but not target truncation, for the diff warning", () => {
    const targetOnly = render(ToolCallWidget, {
      tool: withMcpFacet(mcpTextEdit("note · bounded", { target_truncated: true })),
    });
    expect(targetOnly.queryByTestId("diff-truncated")).toBeNull();
    targetOnly.unmount();

    const content = render(ToolCallWidget, {
      tool: withMcpFacet(mcpTextEdit("note · normal", { content_truncated: true })),
    });
    expect(content.getByTestId("diff-truncated")).toHaveTextContent("Diff truncated");
  });

  it("uses record-specific copy for capped bookmark fields", () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: withMcpFacet(mcpRecordCreation(true)),
    });
    expect(getByTestId("tool-mcp-record-truncated")).toHaveTextContent(
      "Bookmark details truncated.",
    );
    expect(queryByTestId("diff-truncated")).toBeNull();
  });

  it("bounds bookmark field text while collapsed and reveals captured text on expansion", async () => {
    const longValue = `begin ${"x".repeat(20_000)} end`;
    const { getByTestId } = render(ToolCallWidget, {
      tool: withMcpFacet(mcpRecordCreation(false, [{ label: "Description", value: longValue }])),
    });
    const collapsed = getByTestId("tool-mcp-record-field").textContent ?? "";
    expect(collapsed.length).toBeLessThan(600);
    expect(collapsed.endsWith("…")).toBe(true);

    await fireEvent.click(getByTestId("tool-row"));
    expect(getByTestId("tool-mcp-record-field").textContent).toBe(longValue);
  });

  it("caps a long text mutation at 25 lines and reveals captured content on expansion", async () => {
    const content = Array.from({ length: 60 }, (_, index) => `line ${index}`).join("\n") + "\n";
    const { getByTestId, queryByTestId, container } = render(ToolCallWidget, {
      tool: withMcpFacet(mcpTextCreation("prompt · long", content)),
    });

    expect(getByTestId("tool-mcp-creation-expand")).toHaveTextContent("Show 35 more lines");
    expect(container.querySelectorAll('[data-origin="added"]')).toHaveLength(25);
    await fireEvent.click(getByTestId("tool-mcp-creation-expand"));
    expect(queryByTestId("tool-mcp-creation-expand")).toBeNull();
    expect(container.querySelectorAll('[data-origin="added"]')).toHaveLength(60);
  });

  for (const [mutationKind, facet] of [
    ["edit", mcpTextEdit()],
    ["creation", mcpTextCreation()],
  ] as const) {
    for (const [status, overrides] of [
      ["failed", { is_error: true, output: "mutation failed" }],
      [
        "cancelled",
        {
          completed_at: undefined,
          is_error: undefined,
          output: undefined,
          stopped_at: "2026-05-16T00:00:02Z",
          stop_reason: "cancelled" as const,
        },
      ],
    ] as const) {
      it(`suppresses a ${status} MCP ${mutationKind} body and retains raw input`, async () => {
        const { getByTestId, queryByTestId } = render(ToolCallWidget, {
          tool: withMcpFacet(facet, overrides),
        });
        expect(queryByTestId("tool-mcp-edit")).toBeNull();
        expect(queryByTestId("tool-mcp-creation")).toBeNull();
        expect(getByTestId("tool-status-preview")).toBeInTheDocument();

        await fireEvent.click(getByTestId("tool-row"));
        expect(getByTestId("tool-raw-toggle")).toBeInTheDocument();
        await fireEvent.click(getByTestId("tool-raw-toggle"));
        expect(getByTestId("tool-input")).toHaveTextContent("raw MCP input");
      });
    }
  }

  it("keeps successful output collapsed and reveals output plus provenance on expansion", async () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, {
      tool: withMcpFacet(mcpTextEdit()),
    });
    expect(queryByTestId("tool-output")).toBeNull();
    expect(getByTestId("tool-mcp-edit")).toBeInTheDocument();

    await fireEvent.click(getByTestId("tool-row"));
    expect(getByTestId("tool-output")).toHaveTextContent("minimal server output");
    expect(getByTestId("tool-mcp-edit")).toBeInTheDocument();
    expect(getByTestId("tool-mcp-target")).toHaveTextContent("note · note-example");
    await fireEvent.click(getByTestId("tool-raw-toggle"));
    expect(getByTestId("tool-input")).toHaveTextContent("raw MCP input");
  });

  it("does not eagerly format a large raw input for an inline mutation", () => {
    formatToolInputSpy.mockClear();
    const { queryByTestId } = render(ToolCallWidget, {
      tool: withMcpFacet(mcpTextEdit(), { input: { blob: "x".repeat(2_000_000) } }),
    });
    expect(queryByTestId("tool-input")).toBeNull();
    expect(formatToolInputSpy).not.toHaveBeenCalled();
  });

  it("degrades basic and unknown mutation facets to the existing MCP body", async () => {
    const facets = [
      { facet_kind: "mcp", server: "notes_alias", tool: "get_context" } satisfies ToolFacet,
      {
        facet_kind: "mcp",
        server: "notes_alias",
        tool: "future_mutation",
        mutation: { mutation_kind: "future_shape", target: "future" },
      } as unknown as ToolFacet,
    ];
    for (const facet of facets) {
      const view = render(ToolCallWidget, {
        tool: withMcpFacet(facet, { input: { query: "generic preview" } }),
      });
      expect(view.getByTestId("tool-detail")).toHaveTextContent("generic preview");
      expect(view.queryByTestId("tool-body")).toBeNull();
      await fireEvent.click(view.getByTestId("tool-row"));
      expect(view.getByTestId("tool-raw-toggle")).toBeInTheDocument();
      expect(view.queryByTestId("tool-mcp-edit")).toBeNull();
      expect(view.queryByTestId("tool-mcp-creation")).toBeNull();
      expect(view.queryByTestId("tool-mcp-record-creation")).toBeNull();
      view.unmount();
    }
  });
});
