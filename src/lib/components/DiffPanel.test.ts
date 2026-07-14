import { afterEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { tick } from "svelte";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import DiffPanel from "./DiffPanel.svelte";
import type { ChangedFile, FileDiff } from "$lib/types";
import { navFocus, hoverSuppressed } from "$lib/state/gitView.svelte";
import type { DiffTarget } from "$lib/state/gitView.svelte";
import { palette, _testing as paletteTesting } from "$lib/state/commandPalette.svelte";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

const copyTextMock = vi.fn(async (_text: string): Promise<void> => undefined);
vi.mock("$lib/native", () => ({
  copyText: (text: string) => copyTextMock(text),
}));

// Real preferences store (its `diff_style` drives the layout toggle); reset between tests.
const { _testing } = await import("$lib/preferences.svelte");

afterEach(() => {
  vi.useRealTimers();
  _testing.reset();
  invokeMock.mockReset();
  copyTextMock.mockReset();
  copyTextMock.mockResolvedValue(undefined);
  navFocus.pane = null;
  hoverSuppressed.value = false;
  paletteTesting.reset();
});

const diffFixture = (over: Partial<FileDiff> = {}): FileDiff => ({
  path: "code.ts",
  binary: false,
  truncated: false,
  too_large: false,
  too_large_bytes: null,
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

// ChangedFile rows in these tests default to countless entries; tests that
// assert the counts column pass explicit values.
function changedFile(path: string, change: ChangedFile["change"] = "modified"): ChangedFile {
  return { path, change, additions: null, deletions: null };
}

// Both the worktree reads (changed_files/file_diff) and the commit reads
// (commit_changed_files/commit_file_diff) are wired, so a test can assert which
// pair the panel used for a given target kind.
function wire(
  opts: {
    files?: ChangedFile[];
    diff?: FileDiff;
    commitFound?: boolean;
    commitBody?: string | null;
  } = {},
) {
  const files = opts.files ?? [changedFile("code.ts")];
  invokeMock.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "changed_files") return Promise.resolve(files);
    // The commit read returns the `{ found, files }` shape.
    if (cmd === "commit_changed_files")
      return Promise.resolve({
        found: opts.commitFound ?? true,
        body: opts.commitBody ?? null,
        files,
      });
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
    wire({ files: [changedFile("code.ts")] });
    const { container } = render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });

    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    expect(screen.getByTestId("changed-files-resizer")).toBeInTheDocument();
    const text = screen.getByTestId("diff-view").textContent ?? "";
    expect(text).toContain("const OLD = 2;");
    expect(text).toContain("const NEW = 3;");
    expect(container.querySelector('[data-origin="removed"]')).not.toBeNull();
    expect(container.querySelector('[data-origin="added"]')).not.toBeNull();
  });

  it("shows +/− counts per file, omitting binary and pure-rename rows", async () => {
    wire({
      files: [
        { path: "code.ts", change: "modified", additions: 12, deletions: 3 },
        { path: "logo.png", change: "modified", additions: null, deletions: null },
        { path: "moved.ts", change: "renamed", additions: 0, deletions: 0 },
      ],
    });
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getAllByTestId("changed-file")).toHaveLength(3));

    const counts = screen.getAllByTestId("changed-file-counts");
    expect(counts).toHaveLength(1);
    expect(counts[0]!.textContent).toContain("+12");
    expect(counts[0]!.textContent).toContain("−3");
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

  it("opens the selected worktree file in git difftool", async () => {
    wire();
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());

    await fireEvent.click(screen.getByLabelText("Open code.ts in difftool"));

    expect(invokeMock).toHaveBeenCalledWith("open_worktree_file_difftool", {
      worktreePath: "/wt",
      file: "code.ts",
      change: "modified",
    });
    await waitFor(() =>
      expect(screen.getByTestId("changed-file-difftool")).toHaveAttribute("data-state", "done"),
    );
    expect(screen.getByTestId("changed-file-difftool")).toHaveClass("text-accent");
  });

  it("shows difftool launch success even when the external tool stays open", async () => {
    invokeMock.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "changed_files") return Promise.resolve([changedFile("code.ts")]);
      if (cmd === "file_diff") return Promise.resolve(diffFixture({ path: String(args?.file) }));
      if (cmd === "open_worktree_file_difftool") return new Promise<never>(() => {});
      return Promise.resolve(null);
    });
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());

    await fireEvent.click(screen.getByLabelText("Open code.ts in difftool"));

    expect(screen.getByTestId("changed-file-difftool")).toHaveAttribute("data-state", "pending");
    expect(screen.getByTestId("changed-file-difftool")).not.toBeDisabled();
    expect(screen.getByTestId("changed-file-difftool")).toHaveAttribute("aria-disabled", "true");
    await waitFor(
      () =>
        expect(screen.getByTestId("changed-file-difftool")).toHaveAttribute("data-state", "done"),
      { timeout: 1200 },
    );
  });

  it("copies the selected worktree file's absolute path", async () => {
    wire({ files: [changedFile("src/code.ts")] });
    render(DiffPanel, {
      props: { target: wtTarget({ worktreePath: "/wt/project/" }), onClose: noop },
    });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());

    await fireEvent.click(screen.getByLabelText("Copy path for src/code.ts"));

    expect(copyTextMock).toHaveBeenCalledWith("/wt/project/src/code.ts");
    await waitFor(() =>
      expect(screen.getByTestId("changed-file-copy-path")).toHaveAttribute("data-state", "done"),
    );
  });

  it("opens the selected worktree file in the configured editor", async () => {
    wire({ files: [changedFile("src/code.ts")] });
    render(DiffPanel, {
      props: { target: wtTarget({ worktreePath: "/wt/project/" }), onClose: noop },
    });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());

    await fireEvent.click(screen.getByLabelText("Open src/code.ts in editor"));

    expect(invokeMock).toHaveBeenCalledWith("open_in_editor", {
      path: "/wt/project/src/code.ts",
    });
    await waitFor(() =>
      expect(screen.getByTestId("changed-file-editor")).toHaveAttribute("data-state", "done"),
    );
  });

  it("orders copy before editor and difftool for uncommitted files", async () => {
    wire({ files: [changedFile("src/code.ts")] });
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());

    const row = screen.getByTestId("changed-file").parentElement;
    const actionTestIds = Array.from(
      row?.querySelectorAll('[data-testid^="changed-file-"]') ?? [],
    ).map((element) => element.getAttribute("data-testid"));

    expect(actionTestIds).toEqual([
      "changed-file-copy-path",
      "changed-file-editor",
      "changed-file-difftool",
    ]);
  });

  it("does not show the editor action for a deleted worktree file", async () => {
    wire({ files: [changedFile("src/removed.ts", "deleted")] });
    render(DiffPanel, {
      props: { target: wtTarget({ worktreePath: "/wt/project/" }), onClose: noop },
    });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());

    expect(screen.queryByTestId("changed-file-editor")).not.toBeInTheDocument();
    expect(screen.getByTestId("changed-file-copy-path")).toBeInTheDocument();
    expect(screen.getByTestId("changed-file-difftool")).toBeInTheDocument();
  });

  it("surfaces git difftool failures inline", async () => {
    wire();
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    invokeMock.mockRejectedValueOnce(new Error("git difftool is not configured"));

    await fireEvent.click(screen.getByLabelText("Open code.ts in difftool"));

    await waitFor(() =>
      expect(screen.getByTestId("diff-external-error")).toHaveTextContent(
        "git difftool is not configured",
      ),
    );
  });

  it("defaults to unified layout and persists layout changes", async () => {
    wire();
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    expect(screen.getByTestId("diff-view")).toHaveAttribute("data-style", "unified");

    await fireEvent.click(screen.getByTestId("diff-style-side_by_side"));

    await waitFor(() =>
      expect(screen.getByTestId("diff-view")).toHaveAttribute("data-style", "side_by_side"),
    );
    const saved = invokeMock.mock.calls.find((c) => c[0] === "set_preferences");
    expect((saved?.[1] as { preferences: { diff_style: string } }).preferences.diff_style).toBe(
      "side_by_side",
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
      files: [changedFile("logo.png")],
      diff: diffFixture({ path: "logo.png", binary: true, hunks: [] }),
    });
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-binary")).toBeInTheDocument());
    expect(screen.queryByTestId("diff-line")).not.toBeInTheDocument();
  });

  it("shows a too-large placeholder with the file size, not a rendered body", async () => {
    wire({
      files: [changedFile("recording.json", "added")],
      diff: diffFixture({
        path: "recording.json",
        too_large: true,
        too_large_bytes: 122_180_589,
        hunks: [],
      }),
    });
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    const placeholder = await waitFor(() => screen.getByTestId("diff-too-large"));
    expect(placeholder).toHaveTextContent("122 MB");
    expect(screen.queryByTestId("diff-line")).not.toBeInTheDocument();
  });

  it("renders malicious file content as inert text (no executable HTML)", async () => {
    wire({
      files: [changedFile("evil.txt", "added")],
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
    wire({ files: [changedFile("code.ts")] });
    const { rerender } = render(DiffPanel, {
      props: { target: wtTarget({ worktreePath: "/wt-a" }), onClose: noop },
    });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());

    invokeMock.mockClear();
    wire({ files: [changedFile("other.ts", "added")] });
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
        return Promise.resolve([changedFile("a.ts"), changedFile("b.ts")]);
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

    changedFilesResolve?.([changedFile("a.ts"), changedFile("b.ts")]);
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

  it("navigates the file list with arrow keys once a file has focus", async () => {
    wire({
      files: [changedFile("a.ts"), changedFile("b.ts"), changedFile("c.ts")],
    });
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    const fileEls = (): HTMLElement[] => screen.getAllByTestId("changed-file");

    // The first file is auto-selected, but arrows stay inert until the file pane
    // is the focused selection (a file was clicked) — auto-select doesn't focus.
    expect(fileEls()[0]).toHaveAttribute("data-selected", "true");
    await fireEvent.keyDown(window, { key: "ArrowDown" });
    await tick();
    expect(fileEls()[0]).toHaveAttribute("data-selected", "true");

    // Clicking a file focuses the pane; now arrows move and clamp at the ends.
    await fireEvent.click(fileEls()[0]!);
    await fireEvent.keyDown(window, { key: "ArrowDown" });
    await tick();
    expect(fileEls()[1]).toHaveAttribute("data-selected", "true");

    await fireEvent.keyDown(window, { key: "ArrowDown" });
    await tick();
    expect(fileEls()[2]).toHaveAttribute("data-selected", "true");

    await fireEvent.keyDown(window, { key: "ArrowDown" }); // clamp at the bottom
    await tick();
    expect(fileEls()[2]).toHaveAttribute("data-selected", "true");

    await fireEvent.keyDown(window, { key: "ArrowUp" });
    await tick();
    expect(fileEls()[1]).toHaveAttribute("data-selected", "true");
  });

  it("drops the file-row hover highlight and action icons on keyboard nav, restoring on pointer move", async () => {
    wire({
      files: [changedFile("a.ts"), changedFile("b.ts")],
    });
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    // The hover class lives on the row wrapper around the `changed-file` button.
    const rowOf = (i: number): HTMLElement =>
      screen.getAllByTestId("changed-file")[i]!.parentElement!;

    // Row 0 is auto-selected (stays blue, no row hover); row 1 is non-selected
    // and carries the white hover class.
    expect(rowOf(0).className).toContain("bg-selected");
    expect(rowOf(0).className).not.toContain("hover:bg-hover");
    expect(rowOf(1).className).toContain("hover:bg-hover");

    // The action-icons reveal is also keyed on hover, so it must suppress too.
    const actionsOf = (i: number): HTMLElement =>
      within(rowOf(i)).getByTestId("changed-file-difftool").parentElement!;
    expect(actionsOf(0).className).toContain("group-hover:opacity-100");

    await fireEvent.click(screen.getAllByTestId("changed-file")[0]!);
    await fireEvent.keyDown(window, { key: "ArrowDown" });
    await tick();
    // Row 1 is now the keyboard selection; the non-selected row 0 drops its hover
    // background AND its action-icons reveal so nothing lingers under the cursor.
    expect(rowOf(0).className).not.toContain("hover:bg-hover");
    expect(actionsOf(0).className).not.toContain("group-hover:opacity-100");

    await fireEvent.pointerMove(window);
    await tick();
    expect(rowOf(0).className).toContain("hover:bg-hover");
    expect(actionsOf(0).className).toContain("group-hover:opacity-100");
  });

  it("marks the selected row so its action icons hover white via a group-data variant", async () => {
    wire({
      files: [changedFile("a.ts"), changedFile("b.ts")],
    });
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    const rowOf = (i: number): HTMLElement =>
      screen.getAllByTestId("changed-file")[i]!.parentElement!;

    // Row 0 is auto-selected. The row's `data-selected` is what the CSS keys on
    // (the buttons live in Tooltip snippets that don't re-render on selection).
    expect(rowOf(0)).toHaveAttribute("data-selected", "true");
    expect(rowOf(1)).toHaveAttribute("data-selected", "false");

    // The action icons carry the stronger gray default plus the selected-row white
    // override; CSS picks between them off the row's `data-selected`.
    const difftool = within(rowOf(0)).getByTestId("changed-file-difftool");
    expect(difftool.className).toContain("hover:bg-active");
    expect(difftool.className).toContain("group-data-[selected=true]:hover:bg-raised");
  });

  it("does not navigate files while the command palette is open", async () => {
    wire({
      files: [changedFile("a.ts"), changedFile("b.ts")],
    });
    render(DiffPanel, { props: { target: wtTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    const fileEls = (): HTMLElement[] => screen.getAllByTestId("changed-file");

    await fireEvent.click(fileEls()[0]!); // focus the file pane
    palette.open = true;
    await fireEvent.keyDown(window, { key: "ArrowDown" });
    await tick();
    expect(fileEls()[0]).toHaveAttribute("data-selected", "true"); // unchanged
  });
});

describe("DiffPanel (commit target)", () => {
  it("renders a commit's diff via the commit commands, not the worktree commands", async () => {
    wire({ files: [changedFile("code.ts")] });
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

  it("shows the commit body in a scrollable dialog when present", async () => {
    wire({
      files: [changedFile("code.ts")],
      commitBody: "- Why this changed.\n- How reviewers should read it.",
    });
    render(DiffPanel, { props: { target: commitTarget(), onClose: noop } });

    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());
    expect(screen.getByTestId("commit-message-open")).toBeInTheDocument();
    expect(screen.queryByTestId("dialog-content")).not.toBeInTheDocument();

    await fireEvent.click(screen.getByTestId("commit-message-open"));

    expect(screen.getByTestId("dialog-title")).toHaveTextContent("Commit message");
    const body = screen.getByTestId("commit-message-body");
    expect(body).toHaveTextContent("Why this changed.");
    expect(body).toHaveTextContent("How reviewers should read it.");
    expect(body.querySelector("ul")).not.toBeNull();
    expect(body).toHaveClass("overflow-y-auto");
  });

  it("closes the commit message tooltip when opening the dialog", async () => {
    wire({
      files: [changedFile("code.ts")],
      commitBody: "Commit message.",
    });
    render(DiffPanel, { props: { target: commitTarget(), onClose: noop } });

    await waitFor(() => expect(screen.getByTestId("commit-message-open")).toBeInTheDocument());
    vi.useFakeTimers({ shouldAdvanceTime: true });
    await fireEvent.pointerEnter(screen.getByTestId("commit-message-open"));
    await vi.advanceTimersByTimeAsync(500);
    expect(await screen.findByTestId("tooltip-content")).toHaveTextContent("Show commit message");

    await fireEvent.click(screen.getByTestId("commit-message-open"));

    expect(screen.getByTestId("dialog-content")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument());
  });

  it("opens the selected commit file in git difftool", async () => {
    wire({ files: [changedFile("code.ts")] });
    render(DiffPanel, { props: { target: commitTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());

    await fireEvent.click(screen.getByLabelText("Open code.ts in difftool"));

    expect(invokeMock).toHaveBeenCalledWith("open_commit_file_difftool", {
      repoRoot: "/repo",
      oid: "abc123def456",
      file: "code.ts",
    });
  });

  it("copies a commit file's repo-root-based path", async () => {
    wire({ files: [changedFile("code.ts")] });
    render(DiffPanel, { props: { target: commitTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("diff-view")).toBeInTheDocument());

    await fireEvent.click(screen.getByLabelText("Copy path for code.ts"));

    expect(copyTextMock).toHaveBeenCalledWith("/repo/code.ts");
    await waitFor(() =>
      expect(screen.getByTestId("changed-file-copy-path")).toHaveAttribute("data-state", "done"),
    );
  });

  it("shows a calm empty state for a commit that changed no files", async () => {
    wire({ files: [], commitFound: true });
    render(DiffPanel, { props: { target: commitTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("detail-no-changes")).toBeInTheDocument());
    expect(screen.getByTestId("detail-no-changes")).toHaveTextContent("changed no files");
  });

  it("distinguishes a vanished commit from an empty one", async () => {
    // A gc'd / force-updated commit reports found: false → a distinct "no longer
    // available" state, not the "changed no files" message.
    wire({ files: [], commitFound: false });
    render(DiffPanel, { props: { target: commitTarget(), onClose: noop } });
    await waitFor(() => expect(screen.getByTestId("detail-commit-missing")).toBeInTheDocument());
    expect(screen.getByTestId("detail-commit-missing")).toHaveTextContent("no longer available");
    expect(screen.queryByTestId("commit-message-open")).not.toBeInTheDocument();
    expect(screen.queryByTestId("detail-no-changes")).not.toBeInTheDocument();
  });
});
