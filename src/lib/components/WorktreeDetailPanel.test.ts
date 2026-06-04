import { afterEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import WorktreeDetailPanel from "./WorktreeDetailPanel.svelte";
import type { ChangedFile, FileDiff } from "$lib/types";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

// Real preferences store (its `diff_style` drives the layout toggle); reset between tests.
const { _testing } = await import("$lib/preferences.svelte");

afterEach(() => {
  _testing.reset();
  invokeMock.mockReset();
});

const diffFixture = (over: Partial<FileDiff> = {}): FileDiff => ({
  path: "code.ts",
  binary: false,
  truncated: false,
  hunks: [
    {
      header: "@@ -1,2 +1,2 @@",
      lines: [
        { origin: "context", old_lineno: 1, new_lineno: 1, content: "const a = 1;" },
        { origin: "removed", old_lineno: 2, new_lineno: null, content: "const OLD = 2;" },
        { origin: "added", old_lineno: null, new_lineno: 2, content: "const NEW = 3;" },
      ],
    },
  ],
  ...over,
});

function wire(opts: { files?: ChangedFile[]; diff?: FileDiff } = {}) {
  invokeMock.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "changed_files")
      return Promise.resolve(opts.files ?? [{ path: "code.ts", change: "modified" }]);
    if (cmd === "file_diff")
      return Promise.resolve(opts.diff ?? diffFixture({ path: String(args?.file) }));
    if (cmd === "set_preferences") return Promise.resolve(null);
    if (cmd === "reveal_in_finder") return Promise.resolve(null);
    return Promise.resolve(null);
  });
}

const noop = (): void => {};

describe("WorktreeDetailPanel", () => {
  it("auto-selects the first changed file and renders its diff", async () => {
    wire({ files: [{ path: "code.ts", change: "modified" }] });
    const { container } = render(WorktreeDetailPanel, {
      path: "/wt",
      label: "feature",
      onClose: noop,
    });

    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    const text = screen.getByTestId("diff-view").textContent ?? "";
    expect(text).toContain("const OLD = 2;");
    expect(text).toContain("const NEW = 3;");
    // Removed/added line backgrounds are present (origin attributes).
    expect(container.querySelector('[data-origin="removed"]')).not.toBeNull();
    expect(container.querySelector('[data-origin="added"]')).not.toBeNull();
  });

  it("switching the layout toggle changes the diff style and persists it", async () => {
    wire();
    render(WorktreeDetailPanel, { path: "/wt", label: "feature", onClose: noop });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());

    // Default is side-by-side.
    expect(screen.getByTestId("diff-view")).toHaveAttribute("data-style", "side_by_side");

    await fireEvent.click(screen.getByTestId("diff-style-unified"));

    await waitFor(() =>
      expect(screen.getByTestId("diff-view")).toHaveAttribute("data-style", "unified"),
    );
    // The choice is persisted via set_preferences.
    const saved = invokeMock.mock.calls.find((c) => c[0] === "set_preferences");
    expect((saved?.[1] as { preferences: { diff_style: string } }).preferences.diff_style).toBe(
      "unified",
    );
  });

  it("shows an empty state for a clean worktree", async () => {
    wire({ files: [] });
    render(WorktreeDetailPanel, { path: "/wt", label: "feature", onClose: noop });
    await waitFor(() => expect(screen.getByTestId("detail-no-changes")).toBeInTheDocument());
    expect(screen.queryByTestId("diff-view")).not.toBeInTheDocument();
  });

  it("shows a placeholder for a binary file, not a rendered diff body", async () => {
    wire({
      files: [{ path: "logo.png", change: "modified" }],
      diff: diffFixture({ path: "logo.png", binary: true, hunks: [] }),
    });
    render(WorktreeDetailPanel, { path: "/wt", label: "feature", onClose: noop });
    await waitFor(() => expect(screen.getByTestId("diff-binary")).toBeInTheDocument());
    expect(screen.queryByTestId("diff-line")).not.toBeInTheDocument();
  });

  it("renders malicious file content as inert text (no executable HTML)", async () => {
    wire({
      files: [{ path: "evil.txt", change: "added" }],
      diff: diffFixture({
        path: "evil.txt",
        hunks: [
          {
            header: "@@ -0,0 +1 @@",
            lines: [
              {
                origin: "added",
                old_lineno: null,
                new_lineno: 1,
                content: "<script>alert(1)</script>",
              },
            ],
          },
        ],
      }),
    });
    const { container } = render(WorktreeDetailPanel, {
      path: "/wt",
      label: "feature",
      onClose: noop,
    });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    // The content is shown (as text) but no live <script> element exists.
    expect(container.querySelector("script")).toBeNull();
    expect(screen.getByTestId("diff-view").textContent).toContain("alert(1)");
  });

  it("close button invokes onClose", async () => {
    wire();
    const onClose = vi.fn();
    render(WorktreeDetailPanel, { path: "/wt", label: "feature", onClose });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("detail-close"));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("reloads files when the worktree path changes", async () => {
    wire({ files: [{ path: "code.ts", change: "modified" }] });
    const { rerender } = render(WorktreeDetailPanel, {
      path: "/wt-a",
      label: "a",
      onClose: noop,
    });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());

    invokeMock.mockClear();
    wire({ files: [{ path: "other.ts", change: "added" }] });
    await rerender({ path: "/wt-b", label: "b", onClose: noop });

    await waitFor(() =>
      expect(
        invokeMock.mock.calls.some(
          (c) => c[0] === "changed_files" && (c[1] as { path: string }).path === "/wt-b",
        ),
      ).toBe(true),
    );
  });
});
