import { afterEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import GitView from "./GitView.svelte";
import type { RepoListing } from "$lib/types";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

const { refreshAll, _testing } = await import("$lib/state/gitView.svelte");

afterEach(() => {
  _testing.reset();
  invokeMock.mockReset();
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
    remote_branches: [{ name: "origin/main", merged: null, behind_base: null }],
    detached_worktrees: [],
    ...over,
  },
  linked_projects: { "/repos/app": [{ id: "p1", name: "app-proj", directory: "/repos/app" }] },
});

function wire(list: RepoListing[]) {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_tracked_repos") return Promise.resolve(list);
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
    // Uncommitted badge surfaced for the dirty worktree.
    expect(screen.getByText("uncommitted")).toBeInTheDocument();
  });

  it("hides inactive branches by default and reveals them via the toggle", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    // `old-feature` has no worktree → inactive → hidden by default.
    expect(screen.queryByText("old-feature")).not.toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("show-inactive"));
    expect(screen.getByText("old-feature")).toBeInTheDocument();
  });

  it("the branch filter switches between local, remote, and both", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    // Default = local: the remote branch row is not shown.
    expect(screen.queryByTestId("git-remote-branch")).not.toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("branch-filter-remote"));
    expect(screen.getByTestId("git-remote-branch")).toBeInTheDocument();
    // Local branch row hidden under remote-only (the repo header's default-branch
    // label still says "main"; assert the local branch *row* is gone).
    expect(document.querySelector('[data-testid="git-branch"][data-branch="main"]')).toBeNull();
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
});
