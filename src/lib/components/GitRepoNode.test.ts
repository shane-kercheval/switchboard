import { afterEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { tick } from "svelte";
import { fireEvent, render, screen, within } from "@testing-library/svelte";
import GitRepoNode from "./GitRepoNode.svelte";
import type { GitCommitRange, GitCommitSummary, RepoListing } from "$lib/types";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));
vi.mock("@tauri-apps/api/path", () => ({ homeDir: async () => "/repos" }));
vi.mock("$lib/native", () => ({ copyText: async (): Promise<void> => undefined }));

const { branchSelection, branchCommits, diffTarget, selectCommit, _testing } =
  await import("$lib/state/gitView.svelte");
const { palette, _testing: paletteTesting } = await import("$lib/state/commandPalette.svelte");

afterEach(() => {
  _testing.reset();
  paletteTesting.reset();
  invokeMock.mockReset();
});

const commit = (oid: string, subject: string): GitCommitSummary => ({
  oid,
  short_oid: oid.slice(0, 7),
  subject,
  author_name: "T",
  author_email: null,
  authored_at: null,
  branch_work: false,
  unpushed: false,
});

const ranges = (): GitCommitRange[] => [
  {
    kind: "recent",
    label: "Recent commits",
    truncated: false,
    commits: [commit("aaaaaaa0", "a"), commit("bbbbbbb0", "b")],
  },
];

// A repo whose `main` branch has a clean worktree (no uncommitted row, so the
// nav list is just the two commits).
function listing(root: string): RepoListing {
  return {
    repo: {
      root,
      name: root.split("/").pop() ?? root,
      default_branch: "main",
      available: true,
      is_bare: false,
      local_branches: [
        {
          name: "main",
          upstream: null,
          sync: { kind: "local_only" },
          behind_base: null,
          merged: null,
          dangling: false,
          worktree: {
            path: `${root}/wt`,
            dirty: false,
            untracked: false,
            detached_hash: null,
            warning: null,
          },
        },
      ],
      remote_branches: [],
      detached_worktrees: [],
    },
    linked_projects: {},
  };
}

function props(root: string) {
  return {
    listing: listing(root),
    branchFilter: "both" as const,
    showInactive: false,
    fetchState: undefined,
  };
}

// Select `main` in `/a` with its commits loaded and the first commit open, so
// the commit pane is the arrow-key focus.
function selectRepoA(): void {
  branchSelection.current = { repoRoot: "/a", kind: "local", name: "main" };
  branchCommits.ref = branchSelection.current;
  branchCommits.status = "loaded";
  branchCommits.ranges = ranges();
  selectCommit("/a", commit("aaaaaaa0", "a"));
}

describe("GitRepoNode commit keyboard navigation", () => {
  it("moves only the selection-owning repo's commit (N nodes register listeners, one acts)", async () => {
    selectRepoA();
    render(GitRepoNode, { props: props("/a") });
    render(GitRepoNode, { props: props("/b") }); // a second node + window listener
    await tick();

    await fireEvent.keyDown(window, { key: "ArrowDown" });
    await tick();

    // The non-owning node's guard (`repoRoot` mismatch) must stop it acting; a
    // regression would re-select with repoRoot "/b".
    expect(diffTarget.current).toMatchObject({ kind: "commit", oid: "bbbbbbb0", repoRoot: "/a" });
  });

  it("ignores arrows while this node's worktree-actions menu is open", async () => {
    selectRepoA();
    render(GitRepoNode, { props: props("/a") });
    await tick();

    await fireEvent.click(screen.getByTestId("worktree-actions-trigger"));
    await tick();
    expect(screen.getByTestId("git-branch")).toHaveAttribute("data-actions-open", "true");

    await fireEvent.keyDown(window, { key: "ArrowDown" });
    await tick();
    expect(diffTarget.current).toMatchObject({ oid: "aaaaaaa0" }); // unchanged
  });

  it("ignores arrows while a worktree-actions menu is open in a different repo node", async () => {
    selectRepoA(); // selection lives in /a
    render(GitRepoNode, { props: props("/a") });
    render(GitRepoNode, { props: props("/b") });
    await tick();

    // Open /b's menu — /a owns the selection, so this is the cross-node case.
    const bRow = screen.getAllByTestId("git-branch")[1]!;
    await fireEvent.click(within(bRow).getByTestId("worktree-actions-trigger"));
    await tick();
    expect(bRow).toHaveAttribute("data-actions-open", "true");

    await fireEvent.keyDown(window, { key: "ArrowDown" });
    await tick();
    // /a's commit must not move behind /b's open menu (shared open-menu guard).
    expect(diffTarget.current).toMatchObject({ oid: "aaaaaaa0", repoRoot: "/a" });
  });

  it("ignores arrows while the command palette is open", async () => {
    selectRepoA();
    render(GitRepoNode, { props: props("/a") });
    await tick();

    palette.open = true;
    await fireEvent.keyDown(window, { key: "ArrowDown" });
    await tick();
    expect(diffTarget.current).toMatchObject({ oid: "aaaaaaa0" }); // unchanged
  });

  it("ignores arrows originating from an editable element", async () => {
    selectRepoA();
    render(GitRepoNode, { props: props("/a") });
    await tick();

    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();
    await fireEvent.keyDown(input, { key: "ArrowDown" });
    await tick();
    expect(diffTarget.current).toMatchObject({ oid: "aaaaaaa0" }); // unchanged
    input.remove();
  });
});

describe("GitRepoNode actions-trigger hover", () => {
  it("marks a selected (blue) branch row so its actions trigger hovers white", async () => {
    selectRepoA(); // selects `main` in /a
    render(GitRepoNode, { props: props("/a") }); // selected
    render(GitRepoNode, { props: props("/b") }); // not selected
    await tick();

    const rows = screen.getAllByTestId("git-branch");
    // `data-selected` is the row state the CSS keys on (the trigger lives in a
    // snippet that doesn't re-render when selection changes).
    expect(rows[0]).toHaveAttribute("data-selected", "true");
    expect(rows[1]).toHaveAttribute("data-selected", "false");

    // The trigger carries the gray default plus the selected-row white override;
    // CSS picks between them off the row's `data-selected`.
    const trigger = within(rows[0]!).getByTestId("worktree-actions-trigger");
    expect(trigger.className).toContain("hover:bg-border/60");
    expect(trigger.className).toContain("group-data-[selected=true]:hover:bg-raised");
  });
});
