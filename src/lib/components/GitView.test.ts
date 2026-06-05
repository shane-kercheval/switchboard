import { afterEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import GitView from "./GitView.svelte";
import type { RepoListing } from "$lib/types";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

vi.mock("@tauri-apps/api/path", () => ({
  homeDir: async () => "/repos",
}));

const pickDirectoryMock = vi.fn<() => Promise<string | null>>(async () => null);
vi.mock("$lib/native", () => ({
  pickDirectory: () => pickDirectoryMock(),
  copyText: vi.fn(async () => {}),
}));

const { refreshAll, _testing } = await import("$lib/state/gitView.svelte");

afterEach(() => {
  _testing.reset();
  invokeMock.mockReset();
  pickDirectoryMock.mockReset();
  pickDirectoryMock.mockResolvedValue(null);
});

const repo = (over: Partial<RepoListing["repo"]> = {}): RepoListing => ({
  repo: {
    root: "/repos/app",
    name: "app",
    default_branch: "main",
    available: true,
    is_bare: false,
    local_branches: [
      {
        name: "main",
        upstream: "origin/main",
        sync: { kind: "in_sync" },
        behind_base: null,
        merged: null,
        dangling: false,
        worktree: {
          path: "/repos/app",
          dirty: true,
          untracked: false,
          detached_hash: null,
          warning: null,
        },
      },
      {
        name: "old-feature",
        upstream: null,
        sync: { kind: "local_only" },
        behind_base: null,
        merged: true,
        dangling: false,
        worktree: null, // inactive (no worktree)
      },
    ],
    remote_branches: [
      { name: "origin/main", merged: null, behind_base: null },
      { name: "origin/remote-only", merged: null, behind_base: null },
    ],
    detached_worktrees: [],
    ...over,
  },
  linked_projects: { "/repos/app": [{ id: "p1", name: "app-proj", directory: "/repos/app" }] },
});

function wire(list: RepoListing[]) {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_tracked_repos") return Promise.resolve(list);
    if (cmd === "changed_files" || cmd === "commit_changed_files") return Promise.resolve([]);
    if (cmd === "file_diff" || cmd === "commit_file_diff")
      return Promise.resolve({ path: "", binary: false, truncated: false, hunks: [] });
    if (cmd === "branch_commits")
      return Promise.resolve([
        {
          kind: "recent",
          label: "Recent commits",
          truncated: false,
          commits: [
            {
              oid: "c0ffee1234",
              short_oid: "c0ffee1",
              subject: "first commit",
              author_name: "T",
              author_email: null,
              authored_at: null,
            },
          ],
        },
      ]);
    if (cmd === "read_tracked_repo") return Promise.resolve(list[0] ?? repo());
    return Promise.resolve(null); // fetch_repo
  });
}

describe("GitView", () => {
  it("renders tracked repos with their active branches and linked projects", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);

    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());
    // Active branch (has a worktree) shows; its linked project renders.
    expect(document.querySelector('[data-testid="git-branch"][data-branch="main"]')).not.toBeNull();
    expect(screen.getByTestId("linked-project")).toHaveTextContent("app-proj");
    await waitFor(() => expect(screen.getAllByText("~/app").length).toBeGreaterThan(0));
    // Dirty or untracked files surface the changes badge.
    expect(screen.getByLabelText("changes")).toBeInTheDocument();
  });

  it("hides inactive branches by default and reveals them via the toggle", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    // `old-feature` has no local folder → hidden by default.
    expect(screen.queryByText("old-feature")).not.toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("show-inactive"));
    expect(screen.getByText("old-feature")).toBeInTheDocument();
  });

  it("the branch filter switches between local, remote, and both", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    // Default = both: local branches and remote-only branch rows are shown.
    expect(screen.getByTestId("git-remote-branch")).toBeInTheDocument();
    expect(screen.getByText("origin/remote-only")).toBeInTheDocument();
    expect(screen.queryByText("origin/main")).not.toBeInTheDocument();
    expect(document.querySelector('[data-testid="git-branch"][data-branch="main"]')).not.toBeNull();
    await fireEvent.click(screen.getByTestId("branch-filter-local"));
    expect(screen.queryByTestId("git-remote-branch")).not.toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("branch-filter-remote"));
    expect(screen.getByTestId("git-remote-branch")).toBeInTheDocument();
    // Branches that track a remote stay visible as the canonical row; the
    // duplicate origin/main remote ref stays hidden.
    expect(document.querySelector('[data-testid="git-branch"][data-branch="main"]')).not.toBeNull();
    expect(screen.queryByText("origin/main")).not.toBeInTheDocument();
  });

  it("shows the empty state when no repos are tracked", async () => {
    wire([]);
    await refreshAll();
    render(GitView);
    expect(screen.getByTestId("git-empty")).toBeInTheDocument();
  });

  it("an unavailable repo renders marked and lists no branches", async () => {
    wire([
      repo({ available: false, local_branches: [], remote_branches: [], detached_worktrees: [] }),
    ]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("repo-unavailable")).toBeInTheDocument());
    const node = screen.getByTestId("git-repo");
    expect(within(node).queryByTestId("git-branch")).not.toBeInTheDocument();
  });

  it("global refresh re-reads the repo list", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    invokeMock.mockClear();
    wire([repo()]);
    await fireEvent.click(screen.getByTestId("git-refresh-all"));
    await waitFor(() =>
      expect(invokeMock.mock.calls.some((c) => c[0] === "list_tracked_repos")).toBe(true),
    );
  });

  it("Add Repo: a chosen folder is added and the list re-read", async () => {
    wire([]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-empty")).toBeInTheDocument());

    pickDirectoryMock.mockResolvedValueOnce("/repos/app");
    invokeMock.mockClear();
    wire([repo()]);
    await fireEvent.click(screen.getByTestId("git-add-repo"));

    await waitFor(() =>
      expect(
        invokeMock.mock.calls.some(
          (c) => c[0] === "add_tracked_repo" && (c[1] as { path: string }).path === "/repos/app",
        ),
      ).toBe(true),
    );
    // The added repo appears (list was re-read).
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());
  });

  it("clicking a branch opens the diff panel, lists its commits, and closing it returns to the full tree", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    // No panel until a branch is selected.
    expect(screen.queryByTestId("diff-panel")).not.toBeInTheDocument();
    expect(screen.queryByTestId("git-detail-sidebar")).not.toBeInTheDocument();

    // Click the `main` branch row (dirty worktree → defaults to its uncommitted
    // changes, and expands its commit list).
    const mainRow = document.querySelector(
      '[data-testid="git-branch"][data-branch="main"]',
    ) as HTMLElement;
    await fireEvent.click(within(mainRow).getByTestId("branch-select"));

    await waitFor(() => expect(screen.getByTestId("diff-panel")).toBeInTheDocument());
    expect(screen.getByTestId("git-detail-sidebar")).toBeInTheDocument();
    expect(screen.getByTestId("git-detail-resizer")).toBeInTheDocument();
    // Dirty branch → the panel opens on uncommitted changes…
    expect(screen.getByTestId("detail-title")).toHaveTextContent("Uncommitted changes");
    // …and the branch's commits load into the tree.
    await waitFor(() => expect(screen.getByTestId("commit-row")).toBeInTheDocument());
    expect(screen.getByTestId("commit-row")).toHaveTextContent("first commit");

    await fireEvent.click(screen.getByTestId("detail-close"));
    await waitFor(() => expect(screen.queryByTestId("diff-panel")).not.toBeInTheDocument());
  });

  it("clicking a remote-only branch opens the panel on its latest commit (no worktree)", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-remote-branch")).toBeInTheDocument());

    // A remote-only branch has no folder, so it has no uncommitted changes — the
    // panel opens on its latest commit instead.
    const remoteRow = screen.getByTestId("git-remote-branch");
    await fireEvent.click(within(remoteRow).getByTestId("branch-select"));

    await waitFor(() => expect(screen.getByTestId("diff-panel")).toBeInTheDocument());
    await waitFor(() =>
      expect(screen.getByTestId("detail-title")).toHaveTextContent("first commit"),
    );
    // The branch's commits are requested by repo root + remote ref kind.
    const commitsCall = invokeMock.mock.calls.find((c) => c[0] === "branch_commits");
    expect(commitsCall?.[1]).toMatchObject({ kind: "remote", name: "origin/remote-only" });
  });

  it("clicking a detached worktree opens its uncommitted changes, with no commit history", async () => {
    wire([
      repo({
        detached_worktrees: [
          {
            path: "/repos/app/wt",
            dirty: true,
            untracked: false,
            detached_hash: "abc1234",
            warning: null,
          },
        ],
      }),
    ]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-detached-worktree")).toBeInTheDocument());

    const detachedRow = screen.getByTestId("git-detached-worktree");
    await fireEvent.click(within(detachedRow).getByTestId("worktree-select"));

    await waitFor(() => expect(screen.getByTestId("diff-panel")).toBeInTheDocument());
    expect(screen.getByTestId("detail-title")).toHaveTextContent("Uncommitted changes");
    // A detached worktree has no branch → no commit history is requested, and no
    // commit list renders.
    expect(invokeMock.mock.calls.some((c) => c[0] === "branch_commits")).toBe(false);
    expect(screen.queryByTestId("commit-list")).not.toBeInTheDocument();
    // The changes are read from the worktree path.
    const filesCall = invokeMock.mock.calls.find((c) => c[0] === "changed_files");
    expect(filesCall?.[1]).toMatchObject({ path: "/repos/app/wt" });
  });

  it("Add Repo: cancelling the picker adds nothing", async () => {
    wire([]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-empty")).toBeInTheDocument());

    pickDirectoryMock.mockResolvedValueOnce(null); // user cancelled
    invokeMock.mockClear();
    await fireEvent.click(screen.getByTestId("git-add-repo"));

    expect(invokeMock.mock.calls.some((c) => c[0] === "add_tracked_repo")).toBe(false);
  });

  it("Add Repo: a non-git path surfaces the backend error inline", async () => {
    wire([]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-empty")).toBeInTheDocument());

    pickDirectoryMock.mockResolvedValueOnce("/tmp/not-a-repo");
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "add_tracked_repo")
        return Promise.reject(new Error("/tmp/not-a-repo is not inside a git repository"));
      if (cmd === "list_tracked_repos") return Promise.resolve([]);
      return Promise.resolve(null);
    });
    await fireEvent.click(screen.getByTestId("git-add-repo"));

    await waitFor(() =>
      expect(screen.getByTestId("git-add-error")).toHaveTextContent("not inside a git repository"),
    );
  });
});
