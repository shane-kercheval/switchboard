import { afterEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import DiffPanel from "./DiffPanel.svelte";
import type { ChangedFile, FileDiff } from "$lib/types";
import type { DiffTarget } from "$lib/state/gitView.svelte";

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

const wtTarget = (
  over: Partial<Extract<DiffTarget, { kind: "uncommitted" }>> = {},
): DiffTarget => ({
  kind: "uncommitted",
  repoRoot: "/repo",
  worktreePath: "/wt",
  title: "Uncommitted changes",
  subtitle: "~/wt",
  ...over,
});

const commitTarget = (over: Partial<Extract<DiffTarget, { kind: "commit" }>> = {}): DiffTarget => ({
  kind: "commit",
  repoRoot: "/repo",
  oid: "abc123def456",
  shortOid: "abc123d",
  title: "my commit",
  subtitle: "abc123d · T",
  ...over,
});

// Both the worktree reads (changed_files/file_diff) and the commit reads
// (commit_changed_files/commit_file_diff) are wired, so a test can assert which
// pair the panel used for a given target kind.
function wire(opts: { files?: ChangedFile[]; diff?: FileDiff } = {}) {
  invokeMock.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "changed_files" || cmd === "commit_changed_files")
      return Promise.resolve(opts.files ?? [{ path: "code.ts", change: "modified" }]);
    if (cmd === "file_diff" || cmd === "commit_file_diff")
      return Promise.resolve(opts.diff ?? diffFixture({ path: String(args?.file) }));
    if (cmd === "set_preferences") return Promise.resolve(null);
    if (cmd === "reveal_in_finder") return Promise.resolve(null);
    return Promise.resolve(null);
  });
}

const noop = (): void => {};

describe("DiffPanel (uncommitted target)", () => {
  it("auto-selects the first changed file and renders its diff", async () => {
    wire({ files: [{ path: "code.ts", change: "modified" }] });
    const { container } = render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });

    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    expect(screen.getByTestId("changed-files-resizer")).toBeInTheDocument();
    const text = screen.getByTestId("diff-view").textContent ?? "";
    expect(text).toContain("const OLD = 2;");
    expect(text).toContain("const NEW = 3;");
    expect(container.querySelector('[data-origin="removed"]')).not.toBeNull();
    expect(container.querySelector('[data-origin="added"]')).not.toBeNull();
  });

  it("reads via the worktree commands, not the commit commands", async () => {
    wire();
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    const used = invokeMock.mock.calls.map((c) => c[0]);
    expect(used).toContain("changed_files");
    expect(used).toContain("file_diff");
    expect(used).not.toContain("commit_changed_files");
    expect(used).not.toContain("commit_file_diff");
  });

  it("switching the layout toggle changes the diff style and persists it", async () => {
    wire();
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    expect(screen.getByTestId("diff-view")).toHaveAttribute("data-style", "side_by_side");

    await fireEvent.click(screen.getByTestId("diff-style-unified"));

    await waitFor(() =>
      expect(screen.getByTestId("diff-view")).toHaveAttribute("data-style", "unified"),
    );
    const saved = invokeMock.mock.calls.find((c) => c[0] === "set_preferences");
    expect((saved?.[1] as { preferences: { diff_style: string } }).preferences.diff_style).toBe(
      "unified",
    );
  });

  it("shows a calm empty state for a clean worktree", async () => {
    wire({ files: [] });
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("detail-no-changes")).toBeInTheDocument());
    expect(screen.getByTestId("detail-no-changes")).toHaveTextContent("No uncommitted changes");
    expect(screen.queryByTestId("diff-view")).not.toBeInTheDocument();
  });

  it("shows a placeholder for a binary file, not a rendered diff body", async () => {
    wire({
      files: [{ path: "logo.png", change: "modified" }],
      diff: diffFixture({ path: "logo.png", binary: true, hunks: [] }),
    });
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
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
    const { container } = render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    expect(container.querySelector("script")).toBeNull();
    expect(screen.getByTestId("diff-view").textContent).toContain("alert(1)");
  });

  it("close button invokes onClose", async () => {
    wire();
    const onClose = vi.fn();
    render(DiffPanel, { props: { target: wtTarget(), onClose } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("detail-close"));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("reloads files when the target's worktree path changes", async () => {
    wire({ files: [{ path: "code.ts", change: "modified" }] });
    const { rerender } = render(DiffPanel, {
      props: { target: wtTarget({ worktreePath: "/wt-a" }), onClose: noop },
    });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());

    invokeMock.mockClear();
    wire({ files: [{ path: "other.ts", change: "added" }] });
    await rerender({ target: wtTarget({ worktreePath: "/wt-b" }), onClose: noop });

    await waitFor(() =>
      expect(
        invokeMock.mock.calls.some(
          (c) => c[0] === "changed_files" && (c[1] as { path: string }).path === "/wt-b",
        ),
      ).toBe(true),
    );
  });

  it("refreshes the selected file in place when the refresh revision changes", async () => {
    let body = "initial";
    let changedFilesResolve: ((files: ChangedFile[]) => void) | undefined;
    let diffResolve: ((diff: FileDiff) => void) | undefined;
    invokeMock.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "changed_files") {
        if (body === "refreshed") {
          return new Promise<ChangedFile[]>((resolve) => {
            changedFilesResolve = resolve;
          });
        }
        return Promise.resolve([
          { path: "a.ts", change: "modified" },
          { path: "b.ts", change: "modified" },
        ]);
      }
      if (cmd === "file_diff") {
        if (body === "refreshed") {
          return new Promise<FileDiff>((resolve) => {
            diffResolve = resolve;
          });
        }
        return Promise.resolve(
          diffFixture({
            path: String(args?.file),
            hunks: [
              {
                header: "@@ -1 +1 @@",
                lines: [
                  {
                    origin: "added",
                    old_lineno: null,
                    new_lineno: 1,
                    content: `${args?.file}:${body}`,
                  },
                ],
              },
            ],
          }),
        );
      }
      return Promise.resolve(null);
    });
    const { rerender } = render(DiffPanel, {
      props: { target: wtTarget(), refreshRevision: 0, onClose: noop },
    });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toHaveTextContent("a.ts:initial"));

    await fireEvent.click(screen.getAllByTestId("changed-file")[1]!);
    await waitFor(() => expect(screen.getByTestId("diff-view")).toHaveTextContent("b.ts:initial"));

    body = "refreshed";
    await rerender({ target: wtTarget(), refreshRevision: 1, onClose: noop });
    expect(screen.queryByTestId("detail-loading")).not.toBeInTheDocument();
    expect(screen.getByTestId("diff-view")).toHaveTextContent("b.ts:initial");

    changedFilesResolve?.([
      { path: "a.ts", change: "modified" },
      { path: "b.ts", change: "modified" },
    ]);
    diffResolve?.(
      diffFixture({
        path: "b.ts",
        hunks: [
          {
            header: "@@ -1 +1 @@",
            lines: [
              { origin: "added", old_lineno: null, new_lineno: 1, content: "b.ts:refreshed" },
            ],
          },
        ],
      }),
    );
    await waitFor(() =>
      expect(screen.getByTestId("diff-view")).toHaveTextContent("b.ts:refreshed"),
    );
    expect(screen.getAllByTestId("changed-file")[1]).toHaveAttribute("data-selected", "true");
  });
});

describe("DiffPanel (commit target)", () => {
  it("renders a commit's diff via the commit commands, not the worktree commands", async () => {
    wire({ files: [{ path: "code.ts", change: "modified" }] });
    render(DiffPanel, { props: { target: commitTarget(), onClose: noop } });

    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    expect(screen.getByTestId("detail-title")).toHaveTextContent("my commit");
    const used = invokeMock.mock.calls.map((c) => c[0]);
    expect(used).toContain("commit_changed_files");
    expect(used).toContain("commit_file_diff");
    expect(used).not.toContain("changed_files");
    expect(used).not.toContain("file_diff");
    // The commit commands carry the repo root + oid, not a worktree path.
    const filesCall = invokeMock.mock.calls.find((c) => c[0] === "commit_changed_files");
    expect(filesCall?.[1]).toMatchObject({ repoRoot: "/repo", oid: "abc123def456" });
  });

  it("shows a calm empty state for a commit that changed no files", async () => {
    wire({ files: [] });
    render(DiffPanel, { props: { target: commitTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("detail-no-changes")).toBeInTheDocument());
    expect(screen.getByTestId("detail-no-changes")).toHaveTextContent("changed no files");
  });
});
