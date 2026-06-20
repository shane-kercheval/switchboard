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
const copyTextMock = vi.fn(async (_text: string): Promise<void> => undefined);
vi.mock("$lib/native", () => ({
  pickDirectory: () => pickDirectoryMock(),
  copyText: (text: string) => copyTextMock(text),
}));

const { refreshAll, fetchStates, _testing } = await import("$lib/state/gitView.svelte");
const {
  contributedCommands,
  openPalette,
  closePalette,
  _testing: paletteTesting,
} = await import("$lib/state/commandPalette.svelte");
const { tick } = await import("svelte");
const currentYear = new Date().getFullYear();

function gitCommand(id: string) {
  const found = contributedCommands().find((c) => c.id === id);
  if (found === undefined) throw new Error(`missing git command ${id}`);
  return found;
}

afterEach(() => {
  _testing.reset();
  paletteTesting.reset();
  invokeMock.mockReset();
  pickDirectoryMock.mockReset();
  pickDirectoryMock.mockResolvedValue(null);
  copyTextMock.mockReset();
  copyTextMock.mockResolvedValue(undefined);
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
    if (cmd === "changed_files") return Promise.resolve([]);
    if (cmd === "commit_changed_files") return Promise.resolve({ found: true, files: [] });
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
              authored_at: `${currentYear}-06-05T17:14:00Z`,
              branch_work: true,
              unpushed: true,
            },
            {
              oid: "5ca1ab1e99",
              short_oid: "5ca1ab1",
              subject: "shared commit",
              author_name: "T",
              author_email: null,
              authored_at: `${currentYear}-06-04T17:14:00Z`,
              branch_work: true,
              unpushed: false,
            },
          ],
        },
      ]);
    if (cmd === "read_tracked_repo") return Promise.resolve(list[0] ?? repo());
    return Promise.resolve(null); // fetch_repo
  });
}

describe("GitView", () => {
  it("renders tracked repos with their active branches and linked project actions", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);

    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());
    // Active branch (has a worktree) shows; linked projects stay out of the row.
    expect(document.querySelector('[data-testid="git-branch"][data-branch="main"]')).not.toBeNull();
    expect(screen.queryByTestId("linked-project")).not.toBeInTheDocument();
    await waitFor(() => expect(screen.getAllByText("~/app").length).toBeGreaterThan(0));
    await fireEvent.click(screen.getByTestId("worktree-actions-trigger"));
    expect(screen.getByTestId("worktree-action-open-project")).toHaveTextContent(
      "Open Project: app-proj",
    );
    // Dirty or untracked files surface the changes badge.
    expect(screen.getByLabelText("changes")).toBeInTheDocument();
    expect(screen.queryByTestId("repo-fetch-failed")).not.toBeInTheDocument();
    expect(screen.queryByTestId("repo-actions-trigger")).not.toBeInTheDocument();
    expect(screen.getByTestId("repo-action-reveal")).toBeInTheDocument();
    expect(screen.getByTestId("repo-action-editor")).toBeInTheDocument();
    expect(screen.getByTestId("repo-action-copy-path")).toBeInTheDocument();
    expect(screen.getByTestId("repo-action-remove")).toBeInTheDocument();
    expect(screen.getByTestId("repo-refresh")).not.toHaveAttribute("tabindex", "-1");
    expect(screen.getByTestId("repo-action-remove")).not.toHaveAttribute("tabindex", "-1");
    screen.getByTestId("repo-action-remove").focus();
    expect(screen.getByTestId("repo-action-remove")).toHaveFocus();
  });

  it("repo header actions show async success feedback", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("repo-action-editor"));
    expect(invokeMock).toHaveBeenCalledWith("open_in_editor", { path: "/repos/app" });
    await waitFor(() =>
      expect(screen.getByTestId("repo-action-editor")).toHaveAttribute("data-state", "done"),
    );

    await fireEvent.click(screen.getByTestId("repo-action-reveal"));
    expect(invokeMock).toHaveBeenCalledWith("reveal_in_finder", { path: "/repos/app" });
    await waitFor(() =>
      expect(screen.getByTestId("repo-action-reveal")).toHaveAttribute("data-state", "done"),
    );

    await fireEvent.click(screen.getByTestId("repo-action-copy-path"));
    expect(copyTextMock).toHaveBeenCalledWith("/repos/app");
    await waitFor(() =>
      expect(screen.getByTestId("repo-action-copy-path")).toHaveAttribute("data-state", "done"),
    );
  });

  it("contributes git commands to the palette while mounted and clears them on unmount", async () => {
    wire([repo()]);
    await refreshAll();
    const { unmount } = render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    const ids = contributedCommands().map((c) => c.id);
    expect(ids).toContain("git.add-repo");
    expect(ids).toContain("git.refresh-all");
    expect(ids).toContain("git.toggle-detail");

    unmount();
    expect(contributedCommands()).toEqual([]);
  });

  it("git.toggle-detail runs, and ⌘⇧D is suppressed while the palette is open", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    // Open a diff panel so the detail toggle is meaningful.
    const mainRow = document.querySelector(
      '[data-testid="git-branch"][data-branch="main"]',
    ) as HTMLElement;
    await fireEvent.click(within(mainRow).getByTestId("branch-select"));
    await waitFor(() => expect(screen.getByTestId("diff-panel")).toBeInTheDocument());
    expect(screen.getByTestId("git-detail-sidebar")).toHaveAttribute("data-expanded", "false");

    // While the palette owns the keyboard, ⌘⇧D must not toggle the panel.
    openPalette();
    await fireEvent.keyDown(window, { key: "D", code: "KeyD", metaKey: true, shiftKey: true });
    await tick();
    expect(screen.getByTestId("git-detail-sidebar")).toHaveAttribute("data-expanded", "false");

    // Running the command directly (the palette path) does toggle it.
    closePalette();
    await gitCommand("git.toggle-detail").run();
    await waitFor(() =>
      expect(screen.getByTestId("git-detail-sidebar")).toHaveAttribute("data-expanded", "true"),
    );
  });

  it("git.remove-repo is disabled with no selection and enabled once a branch is selected", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    expect(gitCommand("git.remove-repo").disabled).toBe(true);
    expect(gitCommand("git.open-editor").disabled).toBe(true);

    const mainRow = document.querySelector(
      '[data-testid="git-branch"][data-branch="main"]',
    ) as HTMLElement;
    await fireEvent.click(within(mainRow).getByTestId("branch-select"));
    await waitFor(() => expect(gitCommand("git.remove-repo").disabled).toBe(false));
    // `main` has a checked-out worktree, so editor/terminal/reveal become enabled too.
    expect(gitCommand("git.open-editor").disabled).toBe(false);
  });

  it("surfaces a visible banner when a palette action fails", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    const mainRow = document.querySelector(
      '[data-testid="git-branch"][data-branch="main"]',
    ) as HTMLElement;
    await fireEvent.click(within(mainRow).getByTestId("branch-select"));
    await waitFor(() => expect(gitCommand("git.open-editor").disabled).toBe(false));

    invokeMock.mockImplementationOnce((cmd: string) => {
      if (cmd === "open_in_editor") return Promise.reject(new Error("no editor on PATH"));
      return Promise.resolve(null);
    });
    await gitCommand("git.open-editor").run();
    await waitFor(() =>
      expect(screen.getByTestId("git-command-error")).toHaveTextContent("no editor on PATH"),
    );
  });

  it("keeps a branch row highlighted and its ellipsis visible while the action menu is open", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);

    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());
    const mainRow = document.querySelector(
      '[data-testid="git-branch"][data-branch="main"]',
    ) as HTMLElement;
    const trigger = within(mainRow).getByTestId("worktree-actions-trigger");

    expect(mainRow).toHaveAttribute("data-actions-open", "false");
    await fireEvent.click(trigger);

    expect(mainRow).toHaveAttribute("data-actions-open", "true");
    expect(mainRow.className).toContain("bg-raised");
    expect(trigger.className).toContain("opacity-100");

    await fireEvent.keyDown(screen.getByTestId("worktree-action-editor"), { key: "Escape" });

    await waitFor(() => expect(mainRow).toHaveAttribute("data-actions-open", "false"));
  });

  it("does not restore a stale open worktree menu after worktree actions unmount", async () => {
    const withWorktree = repo();
    wire([withWorktree]);
    await refreshAll();
    render(GitView);

    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());
    let mainRow = document.querySelector(
      '[data-testid="git-branch"][data-branch="main"]',
    ) as HTMLElement;
    await fireEvent.click(within(mainRow).getByTestId("worktree-actions-trigger"));
    expect(mainRow).toHaveAttribute("data-actions-open", "true");

    wire([
      repo({
        local_branches: withWorktree.repo.local_branches.map((branch) =>
          branch.name === "main" ? { ...branch, worktree: null } : branch,
        ),
      }),
    ]);
    await refreshAll();
    await waitFor(() =>
      expect(within(mainRow).queryByTestId("worktree-actions-trigger")).not.toBeInTheDocument(),
    );

    wire([withWorktree]);
    await refreshAll();
    mainRow = (await waitFor(() =>
      document.querySelector('[data-testid="git-branch"][data-branch="main"]'),
    )) as HTMLElement;

    expect(within(mainRow).getByTestId("worktree-actions-trigger")).toBeInTheDocument();
    expect(mainRow).toHaveAttribute("data-actions-open", "false");
  });

  it("surfaces repo action failures inline", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "reveal_in_finder") return Promise.reject(new Error("open failed"));
      return Promise.resolve(null);
    });

    await fireEvent.click(screen.getByTestId("repo-action-reveal"));

    await waitFor(() =>
      expect(screen.getByTestId("repo-action-error")).toHaveTextContent("open failed"),
    );
  });

  it("shows a fetch failure icon only when the repo fetch failed", async () => {
    wire([repo()]);
    await refreshAll();
    fetchStates["/repos/app"] = { kind: "failed", at: 0 };
    render(GitView);

    await waitFor(() => expect(screen.getByTestId("repo-fetch-failed")).toBeInTheDocument());
    expect(screen.getByLabelText("Fetch failed")).toBeInTheDocument();
    expect(screen.queryByTestId("repo-fetch-state")).not.toBeInTheDocument();
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

  it("always shows the default branch first even without a local folder", async () => {
    wire([
      repo({
        local_branches: [
          {
            name: "feature",
            upstream: null,
            sync: { kind: "local_only" },
            behind_base: null,
            merged: null,
            dangling: false,
            worktree: {
              path: "/repos/app-feature",
              dirty: false,
              untracked: false,
              detached_hash: null,
              warning: null,
            },
          },
          {
            name: "main",
            upstream: "origin/main",
            sync: { kind: "in_sync" },
            behind_base: null,
            merged: null,
            dangling: false,
            worktree: null,
          },
        ],
      }),
    ]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    const branches = Array.from(document.querySelectorAll('[data-testid="git-branch"]'));
    expect(branches.map((branch) => branch.getAttribute("data-branch"))).toEqual([
      "main",
      "feature",
    ]);
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

  it("⌘N adds a repo and ⌘R refreshes all repos", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    // ⌘N runs Add Repo (picks a folder, then tracks it).
    pickDirectoryMock.mockResolvedValueOnce("/repos/new");
    invokeMock.mockClear();
    await fireEvent.keyDown(window, { key: "n", metaKey: true });
    await waitFor(() =>
      expect(
        invokeMock.mock.calls.some(
          (c) => c[0] === "add_tracked_repo" && (c[1] as { path: string }).path === "/repos/new",
        ),
      ).toBe(true),
    );

    // ⌘R re-reads every tracked repo.
    invokeMock.mockClear();
    await fireEvent.keyDown(window, { key: "r", metaKey: true });
    await waitFor(() =>
      expect(invokeMock.mock.calls.some((c) => c[0] === "list_tracked_repos")).toBe(true),
    );
  });

  it("⌘N and ⌘R are suppressed while the palette is open", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    openPalette();
    pickDirectoryMock.mockResolvedValueOnce("/repos/new");
    invokeMock.mockClear();
    await fireEvent.keyDown(window, { key: "n", metaKey: true });
    await fireEvent.keyDown(window, { key: "r", metaKey: true });
    await tick();
    expect(invokeMock.mock.calls.some((c) => c[0] === "add_tracked_repo")).toBe(false);
    expect(invokeMock.mock.calls.some((c) => c[0] === "list_tracked_repos")).toBe(false);
    closePalette();
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

  it("clicking a branch opens the diff panel, lists its commits, and closing it returns to the empty inspector", async () => {
    wire([repo()]);
    await refreshAll();
    render(GitView);
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    // The inspector stays mounted so the split layout does not jump, but no
    // diff panel renders until a branch/commit/worktree is selected.
    expect(screen.queryByTestId("diff-panel")).not.toBeInTheDocument();
    expect(screen.getByTestId("git-detail-sidebar")).toBeInTheDocument();
    expect(screen.getByTestId("git-detail-resizer")).toBeInTheDocument();
    expect(screen.getByTestId("git-detail-empty")).toHaveTextContent("Select a commit");

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
    await waitFor(() => expect(screen.getAllByTestId("commit-row")).toHaveLength(2));
    const rowFor = (subject: string): HTMLElement =>
      screen.getAllByTestId("commit-row").find((row) => row.textContent?.includes(subject))!;
    const firstRow = rowFor("first commit");
    expect(firstRow).toHaveTextContent(/06-05 \d{2}:14/);
    // Unpushed commit → only the unpushed (amber) dot; the branch-work dot is
    // suppressed since the two never co-occur on one commit.
    expect(within(firstRow).getByTestId("unpushed-indicator")).toBeInTheDocument();
    expect(within(firstRow).queryByTestId("branch-work-indicator")).not.toBeInTheDocument();
    // Pushed branch-work commit → the branch-work (black) dot, no unpushed dot.
    const sharedRow = rowFor("shared commit");
    expect(within(sharedRow).getByTestId("branch-work-indicator")).toBeInTheDocument();
    expect(within(sharedRow).queryByTestId("unpushed-indicator")).not.toBeInTheDocument();

    await fireEvent.click(screen.getByTestId("detail-expand-toggle"));
    expect(screen.getByTestId("git-detail-sidebar")).toHaveAttribute("data-expanded", "true");
    expect(screen.queryByTestId("git-repo-list")).not.toBeInTheDocument();
    expect(screen.queryByTestId("git-detail-resizer")).not.toBeInTheDocument();

    await fireEvent.keyDown(window, { key: "D", metaKey: true, shiftKey: true });
    expect(screen.getByTestId("git-detail-sidebar")).toHaveAttribute("data-expanded", "false");
    expect(screen.getByTestId("git-repo-list")).toBeInTheDocument();
    expect(screen.getByTestId("git-detail-resizer")).toBeInTheDocument();

    await fireEvent.keyDown(window, { key: "d", ctrlKey: true, shiftKey: true });
    expect(screen.getByTestId("git-detail-sidebar")).toHaveAttribute("data-expanded", "true");
    await fireEvent.click(screen.getByTestId("detail-expand-toggle"));
    expect(screen.getByTestId("git-detail-sidebar")).toHaveAttribute("data-expanded", "false");

    await fireEvent.click(screen.getByTestId("detail-close"));
    await waitFor(() => expect(screen.queryByTestId("diff-panel")).not.toBeInTheDocument());
    expect(screen.getByTestId("git-detail-sidebar")).toBeInTheDocument();
    expect(screen.getByTestId("git-detail-empty")).toHaveTextContent("Select a commit");
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
    expect(screen.getByTestId("detail-subtitle").textContent).toMatch(/c0ffee1 · .+ · T/);
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
