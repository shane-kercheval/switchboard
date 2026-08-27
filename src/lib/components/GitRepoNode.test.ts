import { afterEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { tick } from "svelte";
import { fireEvent, render, screen, within } from "@testing-library/svelte";
import GitRepoNode from "./GitRepoNode.svelte";
import type { GitCommitRange, GitCommitSummary, RepoListing } from "$lib/types";

const invokeMock = vi.fn();
const copyTextMock = vi.fn(async (_text: string): Promise<void> => undefined);
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));
vi.mock("@tauri-apps/api/path", () => ({ homeDir: async () => "/repos" }));
vi.mock("$lib/native", () => ({ copyText: (text: string) => copyTextMock(text) }));

const { branchSelection, branchCommits, diffTarget, selectCommit, _testing } =
  await import("$lib/state/gitView.svelte");
const { palette, _testing: paletteTesting } = await import("$lib/state/commandPalette.svelte");

afterEach(() => {
  vi.useRealTimers();
  _testing.reset();
  paletteTesting.reset();
  invokeMock.mockReset();
  copyTextMock.mockReset();
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
      last_commit_at: null,
      local_branches: [
        {
          name: "main",
          upstream: null,
          sync: { kind: "local_only" },
          behind_base: null,
          merged: null,
          dangling: false,
          github_url: null,
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
    expanded: true,
    onExpandedChange: vi.fn(),
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

  it("ignores arrows while this node's branch-actions menu is open", async () => {
    selectRepoA();
    render(GitRepoNode, { props: props("/a") });
    await tick();

    await fireEvent.click(screen.getByTestId("branch-actions-trigger"));
    await tick();
    expect(screen.getByTestId("git-branch")).toHaveAttribute("data-actions-open", "true");

    await fireEvent.keyDown(window, { key: "ArrowDown" });
    await tick();
    expect(diffTarget.current).toMatchObject({ oid: "aaaaaaa0" }); // unchanged
  });

  it("ignores arrows while a branch-actions menu is open in a different repo node", async () => {
    selectRepoA(); // selection lives in /a
    render(GitRepoNode, { props: props("/a") });
    render(GitRepoNode, { props: props("/b") });
    await tick();

    // Open /b's menu — /a owns the selection, so this is the cross-node case.
    const bRow = screen.getAllByTestId("git-branch")[1]!;
    await fireEvent.click(within(bRow).getByTestId("branch-actions-trigger"));
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

describe("GitRepoNode commit list preview cap", () => {
  function manyCommits(count: number, offset = 0): GitCommitSummary[] {
    return Array.from({ length: count }, (_, i) =>
      commit(`oid${String(i + offset).padStart(4, "0")}`, `subject ${i + offset}`),
    );
  }

  function selectMainWith(commitRanges: GitCommitRange[]): void {
    branchSelection.current = { repoRoot: "/a", kind: "local", name: "main" };
    branchCommits.ref = branchSelection.current;
    branchCommits.status = "loaded";
    branchCommits.ranges = commitRanges;
  }

  it("caps the rendered list at 15; Show more reveals the rest and disappears", async () => {
    selectMainWith([
      { kind: "recent", label: "Recent commits", truncated: false, commits: manyCommits(40) },
    ]);
    render(GitRepoNode, { props: props("/a") });
    await tick();

    expect(screen.getAllByTestId("commit-row")).toHaveLength(15);
    const more = screen.getByTestId("commit-show-more");
    expect(more).toHaveTextContent("Show 25 more");

    await fireEvent.click(more);
    expect(screen.getAllByTestId("commit-row")).toHaveLength(40);
    expect(screen.queryByTestId("commit-show-more")).not.toBeInTheDocument();
  });

  it("renders no Show-more row at or under the cap", async () => {
    selectMainWith([
      { kind: "recent", label: "Recent commits", truncated: false, commits: manyCommits(15) },
    ]);
    render(GitRepoNode, { props: props("/a") });
    await tick();

    expect(screen.getAllByTestId("commit-row")).toHaveLength(15);
    expect(screen.queryByTestId("commit-show-more")).not.toBeInTheDocument();
  });

  it("never hides incoming commits: the cap previews local history only, and re-collapses on a ref switch", async () => {
    const twoRanges: GitCommitRange[] = [
      { kind: "recent", label: "Recent commits", truncated: false, commits: manyCommits(20) },
      { kind: "incoming", label: "Incoming commits", truncated: true, commits: manyCommits(5, 20) },
    ];
    selectMainWith(twoRanges);
    render(GitRepoNode, { props: props("/a") });
    await tick();

    // 15 previewed history commits + ALL 5 incoming — what a pull brings must
    // never sit behind Show more. The Show-more row counts only the hidden
    // history and renders inside that section, before the incoming label.
    expect(screen.getAllByTestId("commit-row")).toHaveLength(20);
    const more = screen.getByTestId("commit-show-more");
    expect(more).toHaveTextContent("Show 5 more");
    const incomingLabel = screen.getByText("Incoming commits");
    expect(
      more.compareDocumentPosition(incomingLabel) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    // The untouched incoming range keeps its backend-real truncation note.
    expect(screen.getByText("…older commits not shown")).toBeInTheDocument();

    await fireEvent.click(more);
    expect(screen.getAllByTestId("commit-row")).toHaveLength(25);
    expect(screen.queryByTestId("commit-show-more")).not.toBeInTheDocument();

    // Loading a different ref re-collapses the preview.
    branchCommits.ref = { repoRoot: "/a", kind: "local", name: "other" };
    branchCommits.ranges = [
      { kind: "recent", label: "Recent commits", truncated: false, commits: manyCommits(20) },
    ];
    await tick();
    expect(screen.getAllByTestId("commit-row")).toHaveLength(15);
    expect(screen.getByTestId("commit-show-more")).toHaveTextContent("Show 5 more");
  });
});

describe("GitRepoNode actions-trigger hover", () => {
  it("uses the shared control hover for neutral repo header actions", async () => {
    render(GitRepoNode, { props: props("/a") });
    await tick();

    for (const testid of [
      "repo-refresh",
      "repo-action-reveal",
      "repo-action-editor",
      "repo-action-copy-path",
    ]) {
      expect(screen.getByTestId(testid).className).toContain("hover:bg-control-hover");
    }
    expect(screen.getByTestId("repo-action-remove").className).toContain(
      "hover:bg-status-failed-soft",
    );
  });

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

    // The trigger carries the stronger row-action hover plus the selected-row white override;
    // CSS picks between them off the row's `data-selected`.
    const trigger = within(rows[0]!).getByTestId("branch-actions-trigger");
    expect(trigger.className).toContain("hover:bg-active");
    expect(trigger.className).toContain("group-data-[selected=true]:hover:bg-raised");
  });

  it("suppresses the actions-trigger hover reveal during commit keyboard nav", async () => {
    selectRepoA();
    render(GitRepoNode, { props: props("/a") });
    await tick();
    const trigger = screen.getByTestId("branch-actions-trigger");
    expect(trigger.className).toContain("group-hover:opacity-100");

    // A keyboard move suppresses hover so the `…` doesn't linger under the cursor.
    await fireEvent.keyDown(window, { key: "ArrowDown" });
    await tick();
    expect(trigger.className).not.toContain("group-hover:opacity-100");
  });
});

describe("GitRepoNode GitHub actions", () => {
  it("opens a tracked folderless local branch in GitHub", async () => {
    const componentProps = props("/a");
    const branch = componentProps.listing.repo.local_branches[0]!;
    branch.upstream = "fork/main";
    branch.sync = { kind: "in_sync" };
    branch.github_url = "https://github.com/acme/widgets/tree/main";
    branch.worktree = null;
    render(GitRepoNode, { props: componentProps });
    await tick();

    await fireEvent.click(screen.getByTestId("branch-actions-trigger"));
    await tick();
    await fireEvent.click(screen.getByTestId("branch-action-github"));

    expect(invokeMock).toHaveBeenCalledWith("open_external_url", {
      url: "https://github.com/acme/widgets/tree/main",
    });
  });

  it("opens a remote-only branch in GitHub", async () => {
    const componentProps = props("/a");
    componentProps.listing.repo.remote_branches = [
      {
        name: "fork/feature",
        github_url: "https://github.com/acme/widgets/tree/feature",
        merged: false,
        behind_base: 0,
      },
    ];
    render(GitRepoNode, { props: { ...componentProps, branchFilter: "remote" } });
    await tick();

    const remote = screen.getByTestId("git-remote-branch");
    await fireEvent.click(within(remote).getByTestId("branch-actions-trigger"));
    await tick();
    await fireEvent.click(screen.getByTestId("branch-action-github"));

    expect(invokeMock).toHaveBeenCalledWith("open_external_url", {
      url: "https://github.com/acme/widgets/tree/feature",
    });
  });

  it("offers Copy for an unpushed folderless branch without offering GitHub", async () => {
    const componentProps = props("/a");
    componentProps.listing.repo.local_branches[0]!.worktree = null;
    render(GitRepoNode, { props: componentProps });
    await tick();

    await fireEvent.click(screen.getByTestId("branch-actions-trigger"));
    await tick();
    expect(screen.queryByTestId("branch-action-github")).not.toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("branch-action-copy-branch"));
    expect(copyTextMock).toHaveBeenCalledWith("main");
  });

  it("offers Copy for a non-GitHub remote branch without offering GitHub", async () => {
    const componentProps = props("/a");
    componentProps.listing.repo.remote_branches = [
      {
        name: "upstream/feature",
        github_url: null,
        merged: false,
        behind_base: 0,
      },
    ];
    render(GitRepoNode, { props: { ...componentProps, branchFilter: "remote" } });
    await tick();

    const remote = screen.getByTestId("git-remote-branch");
    await fireEvent.click(within(remote).getByTestId("branch-actions-trigger"));
    await tick();
    expect(screen.queryByTestId("branch-action-github")).not.toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("branch-action-copy-branch"));
    expect(copyTextMock).toHaveBeenCalledWith("upstream/feature");
  });
});

describe("GitRepoNode supplemental text tooltips", () => {
  it("uses one long-delay keyboard target for repository identity", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    render(GitRepoNode, { props: props("/a") });
    await tick();

    const identity = screen.getByTestId("repo-identity");
    const repoName = screen.getByText("a");
    const repoPath = screen.getByText("/a");
    expect(identity).toHaveAttribute("tabindex", "0");
    expect(repoName).not.toHaveAttribute("tabindex");
    expect(repoPath).not.toHaveAttribute("tabindex");
    expect(repoName).not.toHaveAttribute("title");
    expect(repoPath).not.toHaveAttribute("title");

    await fireEvent.pointerEnter(identity);
    await vi.advanceTimersByTimeAsync(600);
    expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();

    await vi.advanceTimersByTimeAsync(500);
    expect(screen.getByTestId("tooltip-content")).toHaveTextContent("/a");
  });

  it("opens branch details from the existing branch button without a nested tab stop", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    render(GitRepoNode, { props: props("/a") });
    await tick();

    const branch = screen.getByTestId("branch-select");
    expect(branch.querySelector('[tabindex="0"]')).toBeNull();

    await fireEvent.keyDown(window, { key: "Tab" });
    await fireEvent.focus(branch);
    await vi.advanceTimersByTimeAsync(1100);
    const tooltip = screen.getByTestId("tooltip-content");
    expect(tooltip).toHaveTextContent("main · /a/wt");
    expect(tooltip).toHaveTextContent(
      "Local only: No upstream is configured; this branch has not been pushed.",
    );
  });

  it("opens the canonical prunable-worktree explanation from its existing focus target", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const componentProps = props("/a");
    componentProps.listing.repo.detached_worktrees = [
      {
        path: "/a/missing-worktree",
        dirty: false,
        untracked: false,
        detached_hash: "deadbee",
        warning: "prunable",
      },
    ];
    render(GitRepoNode, { props: componentProps });
    await tick();

    const identity = screen.getByTestId("detached-identity");
    expect(identity).toHaveAttribute("tabindex", "0");
    expect(identity.querySelector('[tabindex="0"]')).toBeNull();

    await fireEvent.keyDown(window, { key: "Tab" });
    await fireEvent.focus(identity);
    await vi.advanceTimersByTimeAsync(1100);
    const tooltip = screen.getByTestId("tooltip-content");
    expect(tooltip).toHaveTextContent("/a/missing-worktree");
    expect(tooltip).toHaveTextContent(
      "Missing folder: This folder path is gone; the git worktree record can be pruned.",
    );
  });

  it("opens commit identity from the existing commit button", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    selectRepoA();
    render(GitRepoNode, { props: props("/a") });
    await tick();

    const commitRow = screen.getAllByTestId("commit-row")[0]!;
    expect(commitRow.querySelector('[tabindex="0"]')).toBeNull();

    await fireEvent.keyDown(window, { key: "Tab" });
    await fireEvent.focus(commitRow);
    await vi.advanceTimersByTimeAsync(1100);
    expect(screen.getByTestId("tooltip-content")).toHaveTextContent("aaaaaaa · a");
  });
});
