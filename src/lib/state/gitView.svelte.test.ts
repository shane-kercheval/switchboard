import { afterEach, describe, expect, it, vi } from "vitest";
import type { RepoListing } from "$lib/types";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

const {
  gitView,
  view,
  fetchStates,
  enterGitView,
  refreshAll,
  refreshStale,
  refreshRepo,
  fetchRepo,
  fetchAll,
  addRepo,
  removeRepo,
  selectBranch,
  selectUncommitted,
  branchSelection,
  branchCommits,
  diffTarget,
  gitRefresh,
  loadProjectRepo,
  revealProjectBranch,
  _testing,
} = await import("./gitView.svelte");

afterEach(() => {
  _testing.reset();
  invokeMock.mockReset();
});

const listing = (root: string): RepoListing => ({
  repo: {
    root,
    name: root.split("/").pop() ?? root,
    default_branch: "main",
    available: true,
    is_bare: false,
    local_branches: [],
    remote_branches: [],
    detached_worktrees: [],
  },
  linked_projects: {},
});

/// A listing whose `main` branch is checked out at `wtPath`, for selection tests.
const listingWithWorktree = (root: string, wtPath: string): RepoListing => {
  const l = listing(root);
  l.repo.local_branches = [
    {
      name: "main",
      upstream: null,
      sync: { kind: "in_sync" },
      behind_base: null,
      merged: null,
      dangling: false,
      worktree: { path: wtPath, dirty: true, untracked: false, detached_hash: null, warning: null },
    },
  ];
  return l;
};

const unavailableListing = (root: string): RepoListing => ({
  repo: {
    root,
    name: root.split("/").pop() ?? root,
    default_branch: null,
    available: false,
    is_bare: false,
    local_branches: [],
    remote_branches: [],
    detached_worktrees: [],
  },
  linked_projects: {},
});

/// A listing whose `main` worktree at `wtPath` is linked to `projectId`, for the
/// project-panel lookup.
const listingWithProject = (root: string, wtPath: string, projectId: string): RepoListing => {
  const l = listingWithWorktree(root, wtPath);
  l.linked_projects = { [wtPath]: [{ id: projectId, name: "proj", directory: wtPath }] };
  return l;
};

/// Route invoke by command; default list/read/fetch all succeed, and the commit
/// read returns an empty range set unless a test overrides it.
function wire(opts: { list?: RepoListing[]; fetchRejects?: boolean } = {}) {
  invokeMock.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "list_tracked_repos") return Promise.resolve(opts.list ?? []);
    if (cmd === "read_tracked_repo") return Promise.resolve(listing(String(args?.path)));
    if (cmd === "branch_commits") return Promise.resolve([]);
    if (cmd === "fetch_repo") {
      return opts.fetchRejects ? Promise.reject(new Error("no remote")) : Promise.resolve(null);
    }
    return Promise.resolve(null);
  });
}

/// A `SelectedRef` for `main` in `/a`, the common selection-test target.
const mainRef = (root = "/a") => ({ repoRoot: root, kind: "local" as const, name: "main" });
const dirtyOpts = { worktreePath: "/a/wt", hasChanges: true, worktreeSubtitle: "~/wt" };

describe("gitView store", () => {
  it("enterGitView loads repos and sets git mode", async () => {
    wire({ list: [listing("/a"), listing("/b")] });
    await enterGitView();
    expect(view.mode).toBe("git");
    expect(gitView.repos.map((r) => r.repo.root)).toEqual(["/a", "/b"]);
    expect(gitView.status).toBe("complete");
  });

  it("refreshAll failure leaves a failed status without throwing", async () => {
    invokeMock.mockImplementation((cmd: string) =>
      cmd === "list_tracked_repos" ? Promise.reject(new Error("boom")) : Promise.resolve(null),
    );
    await refreshAll();
    expect(gitView.status).toBe("failed");
  });

  it("refreshStale only re-reads repos past the local-stale window", async () => {
    wire({ list: [listing("/a"), listing("/b")] });
    await refreshAll(); // both just-read → fresh
    const listCalls = () =>
      invokeMock.mock.calls.filter((c) => c[0] === "read_tracked_repo").length;
    invokeMock.mockClear();
    wire({ list: [listing("/a"), listing("/b")] });

    // Immediately re-entering: nothing is stale, so no per-repo re-reads fire.
    await refreshStale();
    expect(listCalls()).toBe(0);
  });

  it("entering the Git view after a project-panel read still does the full aggregate load", async () => {
    // Regression guard: a project-panel read upserts one repo into the shared
    // cache while in Projects mode; entry must still load the *whole* tracked set
    // (gate on aggregate-loaded status, not cache size) — else the Git view would
    // show only that one repo.
    // The project's worktree path resolves to repo root "/a" (the backend
    // collapses a worktree path to its root), so the panel read and the aggregate
    // refer to the same repo — not a separate "/a/wt" entry.
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "read_tracked_repo")
        return Promise.resolve(listingWithProject("/a", "/a/wt", "p1"));
      if (cmd === "list_tracked_repos")
        return Promise.resolve([listing("/a"), listing("/b"), listing("/c")]);
      return Promise.resolve(null);
    });

    await loadProjectRepo("/a/wt"); // one repo now cached, status still "pending"
    expect(gitView.status).not.toBe("complete");

    await refreshStale();
    expect(invokeMock.mock.calls.some((c) => c[0] === "list_tracked_repos")).toBe(true);
    expect(gitView.repos.map((r) => r.repo.root).sort()).toEqual(["/a", "/b", "/c"]);
  });

  it("selectBranch defaults to uncommitted changes when the worktree is dirty", async () => {
    wire({ list: [listingWithWorktree("/a", "/a/wt")] }); // dirty: true
    await refreshAll();
    await selectBranch(mainRef(), dirtyOpts);
    expect(diffTarget.current).toMatchObject({ kind: "uncommitted", worktreePath: "/a/wt" });
  });

  it("selectBranch loads commits and auto-selects the latest when the branch is clean", async () => {
    const clean = listing("/a");
    clean.repo.local_branches = [
      {
        name: "main",
        upstream: null,
        sync: { kind: "local_only" },
        behind_base: null,
        merged: null,
        dangling: false,
        worktree: {
          path: "/a/wt",
          dirty: false,
          untracked: false,
          detached_hash: null,
          warning: null,
        },
      },
    ];
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_tracked_repos") return Promise.resolve([clean]);
      if (cmd === "branch_commits")
        return Promise.resolve([
          {
            kind: "recent",
            label: "Recent commits",
            truncated: false,
            commits: [
              {
                oid: "abc123def456",
                short_oid: "abc123d",
                subject: "latest",
                author_name: "T",
                author_email: null,
                authored_at: null,
                branch_work: true,
              },
            ],
          },
        ]);
      return Promise.resolve(null);
    });
    await refreshAll();
    await selectBranch(mainRef(), {
      worktreePath: "/a/wt",
      hasChanges: false,
      worktreeSubtitle: "~/wt",
    });
    expect(branchCommits.ranges[0]?.commits[0]?.subject).toBe("latest");
    expect(diffTarget.current).toMatchObject({ kind: "commit", oid: "abc123def456" });
  });

  it("re-selecting the same branch collapses it", async () => {
    wire({ list: [listingWithWorktree("/a", "/a/wt")] });
    await refreshAll();
    await selectBranch(mainRef(), dirtyOpts);
    expect(branchSelection.current).not.toBeNull();
    await selectBranch(mainRef(), dirtyOpts);
    expect(branchSelection.current).toBeNull();
    expect(diffTarget.current).toBeNull();
  });

  it("a single-repo refresh clears a selection whose branch disappears", async () => {
    wire({ list: [listingWithWorktree("/a", "/a/wt")] });
    await refreshAll();
    await selectBranch(mainRef(), dirtyOpts);
    expect(branchSelection.current?.name).toBe("main");

    // A per-repo refresh of /a returns it without the `main` branch.
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "read_tracked_repo") return Promise.resolve(listing("/a"));
      if (cmd === "branch_commits") return Promise.resolve([]);
      return Promise.resolve(null);
    });
    await refreshRepo("/a");
    expect(branchSelection.current).toBeNull();
    expect(diffTarget.current).toBeNull();
  });

  it("bumps the diff refresh only when the selected target's repo is re-read", async () => {
    wire({ list: [listingWithWorktree("/a", "/a/wt"), listingWithWorktree("/b", "/b/wt")] });
    await refreshAll();
    await selectBranch(mainRef(), dirtyOpts); // uncommitted target on /a/wt
    const before = gitRefresh.revision;

    invokeMock.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "read_tracked_repo") {
        const root = String(args?.path);
        return Promise.resolve(listingWithWorktree(root, `${root}/wt`));
      }
      if (cmd === "branch_commits") return Promise.resolve([]);
      return Promise.resolve(null);
    });
    await refreshRepo("/b"); // not the selected repo → no bump
    expect(gitRefresh.revision).toBe(before);

    await refreshRepo("/a"); // selected target's repo → bump
    expect(gitRefresh.revision).toBe(before + 1);
  });

  it("falls back to the latest commit when a selected branch loses its worktree", async () => {
    // A recent range with one commit, so the fallback has something to select.
    const ranges = [
      {
        kind: "recent",
        label: "Recent commits",
        truncated: false,
        commits: [
          {
            oid: "fa11bac0",
            short_oid: "fa11bac",
            subject: "tip",
            author_name: "T",
            author_email: null,
            authored_at: null,
            branch_work: true,
          },
        ],
      },
    ];
    // `main` keeps existing after refresh, but its worktree is removed.
    const mainNoWorktree = (): RepoListing => {
      const l = listing("/a");
      l.repo.local_branches = [
        {
          name: "main",
          upstream: null,
          sync: { kind: "local_only" },
          behind_base: null,
          merged: null,
          dangling: false,
          worktree: null,
        },
      ];
      return l;
    };
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_tracked_repos")
        return Promise.resolve([listingWithWorktree("/a", "/a/wt")]);
      if (cmd === "branch_commits") return Promise.resolve(ranges);
      if (cmd === "read_tracked_repo") return Promise.resolve(mainNoWorktree());
      return Promise.resolve(null);
    });
    await refreshAll();
    await selectBranch(mainRef(), dirtyOpts);
    expect(diffTarget.current?.kind).toBe("uncommitted");

    await refreshRepo("/a"); // branch stays, worktree gone
    expect(diffTarget.current).toMatchObject({ kind: "commit", oid: "fa11bac0" });
    expect(branchSelection.current?.name).toBe("main"); // branch stays selected
  });

  it("closes the panel when a selected detached worktree is pruned", async () => {
    const withDetached = (): RepoListing => {
      const l = listing("/a");
      l.repo.detached_worktrees = [
        { path: "/a/dt", dirty: true, untracked: false, detached_hash: "abc1234", warning: null },
      ];
      return l;
    };
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_tracked_repos") return Promise.resolve([withDetached()]);
      if (cmd === "read_tracked_repo") return Promise.resolve(listing("/a")); // detached gone
      return Promise.resolve(null);
    });
    await refreshAll();
    // A detached worktree is selected directly (no branch), as GitRepoNode does.
    selectUncommitted("/a", "/a/dt", "~/dt");
    expect(diffTarget.current?.kind).toBe("uncommitted");

    await refreshRepo("/a"); // the detached worktree is no longer present
    expect(diffTarget.current).toBeNull();
  });

  it("loadProjectRepo skips a redundant read when the repo was just read", async () => {
    // Fetch rejects so there's no success-path follow-up re-read muddying the
    // count — every `read_tracked_repo` here comes from loadProjectRepo's own read.
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "read_tracked_repo")
        return Promise.resolve(listingWithProject("/a", "/a/wt", "p1"));
      if (cmd === "fetch_repo") return Promise.reject(new Error("no remote"));
      return Promise.resolve(null);
    });

    await loadProjectRepo("/a/wt");
    expect(invokeMock.mock.calls.filter((c) => c[0] === "read_tracked_repo").length).toBe(1);

    // A remount (sidebar toggle) within the read-staleness window serves from cache.
    await loadProjectRepo("/a/wt");
    expect(invokeMock.mock.calls.filter((c) => c[0] === "read_tracked_repo").length).toBe(1);
  });

  it("revealProjectBranch is false for a project linked to a detached worktree", async () => {
    const detached = listing("/a");
    detached.repo.detached_worktrees = [
      { path: "/a/dt", dirty: true, untracked: false, detached_hash: "abc1234", warning: null },
    ];
    detached.linked_projects = { "/a/dt": [{ id: "p1", name: "proj", directory: "/a/dt" }] };
    wire({ list: [detached] });
    await refreshAll();

    // Linked, but the worktree is detached → no `local_branches` row.
    await expect(revealProjectBranch("p1", "/a/dt")).resolves.toBe(false);
  });

  it("a failed fetch records a 'failed' state, never throws", async () => {
    wire({ list: [listing("/a")], fetchRejects: true });
    await refreshAll();
    await fetchRepo("/a");
    expect(fetchStates["/a"]).toEqual({ kind: "failed", at: expect.any(Number) });
  });

  it("a successful fetch records ok and re-reads the repo", async () => {
    wire({ list: [listing("/a")] });
    await refreshAll();
    invokeMock.mockClear();
    wire({ list: [listing("/a")] });
    await fetchRepo("/a");
    expect(fetchStates["/a"]).toMatchObject({ kind: "ok" });
    // Fetch success triggers a single-repo re-read.
    expect(invokeMock.mock.calls.some((c) => c[0] === "read_tracked_repo")).toBe(true);
  });

  it("concurrent fetches for the same root dedupe to one subprocess", async () => {
    wire({ list: [listing("/a")] });
    await refreshAll();
    invokeMock.mockClear();
    let resolveFetch!: () => void;
    invokeMock.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "fetch_repo") return new Promise<null>((r) => (resolveFetch = () => r(null)));
      if (cmd === "read_tracked_repo") return Promise.resolve(listing(String(args?.path)));
      return Promise.resolve(null);
    });

    const first = fetchRepo("/a");
    const second = fetchRepo("/a"); // joins the in-flight one, no second subprocess

    // The join is real, not a no-op early return: `second` stays pending until
    // the underlying fetch resolves, so callers (e.g. fetchAll) only continue
    // once the actual operation — and its follow-up re-read — is done.
    let secondSettled = false;
    void second.then(() => (secondSettled = true));
    await Promise.resolve();
    expect(secondSettled).toBe(false);

    resolveFetch();
    await Promise.all([first, second]);

    expect(secondSettled).toBe(true);
    expect(invokeMock.mock.calls.filter((c) => c[0] === "fetch_repo")).toHaveLength(1);
  });

  it("addRepo adds the path then re-reads so the repo appears", async () => {
    wire({ list: [] });
    await refreshAll();
    expect(gitView.repos).toHaveLength(0);

    wire({ list: [listing("/a")] });
    await addRepo("/a");

    expect(invokeMock.mock.calls.some((c) => c[0] === "add_tracked_repo")).toBe(true);
    expect(gitView.repos.map((r) => r.repo.root)).toEqual(["/a"]);
  });

  it("addRepo propagates a backend rejection (non-git path) for inline display", async () => {
    wire({ list: [] });
    await refreshAll();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "add_tracked_repo") return Promise.reject(new Error("not a git repo"));
      return Promise.resolve([]);
    });
    await expect(addRepo("/nope")).rejects.toThrow("not a git repo");
  });

  it("addRepo propagates a failed re-read (mutation never silently succeeds)", async () => {
    // The add persists, but the follow-up list read fails. A mutation must not
    // resolve as success while the new repo stays invisible — the error
    // propagates so the caller (GitView) can surface it inline.
    wire({ list: [] });
    await refreshAll();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "add_tracked_repo") return Promise.resolve(null);
      if (cmd === "list_tracked_repos") return Promise.reject(new Error("read boom"));
      return Promise.resolve(null);
    });
    await expect(addRepo("/a")).rejects.toThrow("read boom");
  });

  it("clears the selection when its repo disappears from the tree", async () => {
    wire({ list: [listingWithWorktree("/a", "/a/wt")] });
    await refreshAll();
    await selectBranch(mainRef(), dirtyOpts);
    expect(diffTarget.current).not.toBeNull();

    // A refresh where /a is gone entirely (e.g. repo removed).
    wire({ list: [] });
    await refreshAll();
    expect(branchSelection.current).toBeNull();
    expect(diffTarget.current).toBeNull();
  });

  it("keeps the selection when its branch is still present after a refresh", async () => {
    wire({ list: [listingWithWorktree("/a", "/a/wt")] });
    await refreshAll();
    await selectBranch(mainRef(), dirtyOpts);

    wire({ list: [listingWithWorktree("/a", "/a/wt")] });
    await refreshAll();
    expect(branchSelection.current?.name).toBe("main");
  });

  it("removeRepo untracks the repo and re-reads without it", async () => {
    wire({ list: [listing("/a"), listing("/b")] });
    await refreshAll();
    expect(gitView.repos).toHaveLength(2);

    wire({ list: [listing("/b")] });
    await removeRepo("/a");

    expect(invokeMock.mock.calls.some((c) => c[0] === "remove_tracked_repo")).toBe(true);
    expect(gitView.repos.map((r) => r.repo.root)).toEqual(["/b"]);
  });

  it("a fetch settling after its repo is removed leaves no dangling fetch state", async () => {
    // Race: a background fetch is in flight when the user removes the repo. The
    // backend's tracked-membership gate then rejects the now-untracked fetch, and
    // when that rejection settles it must not resurrect bookkeeping for the
    // removed root.
    let rejectFetch!: () => void;
    invokeMock.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "fetch_repo")
        return new Promise<null>(
          (_, reject) => (rejectFetch = () => reject(new Error("untracked"))),
        );
      if (cmd === "list_tracked_repos") return Promise.resolve([listing("/a")]);
      if (cmd === "read_tracked_repo") return Promise.resolve(listing(String(args?.path)));
      return Promise.resolve(null);
    });
    await refreshAll();

    const fetching = fetchRepo("/a"); // in flight, promise held open

    // Remove /a before the fetch settles; the re-read returns an empty list.
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_tracked_repos") return Promise.resolve([]);
      return Promise.resolve(null);
    });
    await removeRepo("/a");
    expect(_testing.runtimeSize()).toBe(0);

    rejectFetch();
    await fetching; // fetchRepo swallows the rejection internally

    expect(fetchStates["/a"]).toBeUndefined();
    expect(_testing.runtimeSize()).toBe(0);
  });

  it("revealProjectBranch opens Git view and selects the linked branch", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "read_tracked_repo")
        return Promise.resolve(listingWithProject("/a", "/a/wt", "p1"));
      if (cmd === "branch_commits") return Promise.resolve([]);
      return Promise.resolve(null);
    });

    await expect(revealProjectBranch("p1", "/a/wt")).resolves.toBe(true);

    expect(view.mode).toBe("git");
    expect(branchSelection.current).toEqual({ repoRoot: "/a", kind: "local", name: "main" });
    expect(diffTarget.current).toMatchObject({
      kind: "uncommitted",
      repoRoot: "/a",
      worktreePath: "/a/wt",
    });
  });

  it("revealProjectBranch does not collapse an already selected branch", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "read_tracked_repo")
        return Promise.resolve(listingWithProject("/a", "/a/wt", "p1"));
      if (cmd === "branch_commits") return Promise.resolve([]);
      return Promise.resolve(null);
    });

    await revealProjectBranch("p1", "/a/wt");
    await revealProjectBranch("p1", "/a/wt");

    expect(view.mode).toBe("git");
    expect(branchSelection.current).toEqual({ repoRoot: "/a", kind: "local", name: "main" });
    expect(diffTarget.current?.kind).toBe("uncommitted");
  });

  it("revealProjectBranch adds an untracked project repo before selecting it", async () => {
    let tracked = false;
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "read_tracked_repo")
        return Promise.resolve(
          tracked ? listingWithProject("/a", "/a/wt", "p1") : unavailableListing("/a"),
        );
      if (cmd === "add_tracked_repo") {
        tracked = true;
        return Promise.resolve(null);
      }
      if (cmd === "list_tracked_repos")
        return Promise.resolve([listingWithProject("/a", "/a/wt", "p1")]);
      if (cmd === "branch_commits") return Promise.resolve([]);
      return Promise.resolve(null);
    });

    await expect(revealProjectBranch("p1", "/a/wt")).resolves.toBe(true);

    expect(invokeMock).toHaveBeenCalledWith("add_tracked_repo", { path: "/a/wt" });
    expect(view.mode).toBe("git");
    expect(branchSelection.current).toEqual({ repoRoot: "/a", kind: "local", name: "main" });
  });

  it("revealProjectBranch returns false when no linked branch resolves", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "read_tracked_repo") return Promise.resolve(listingWithWorktree("/a", "/a/wt"));
      return Promise.resolve(null);
    });

    await expect(revealProjectBranch("missing", "/a/wt")).resolves.toBe(false);

    expect(view.mode).toBe("projects");
    expect(branchSelection.current).toBeNull();
  });

  it("loadProjectRepo reads the project's repo and fetches it when stale", async () => {
    invokeMock.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "read_tracked_repo")
        return Promise.resolve(listingWithProject(String(args?.path), "/a/wt", "p1"));
      return Promise.resolve(null); // fetch_repo
    });

    await loadProjectRepo("/a/wt");

    // The repo is read and upserted for later linked-project navigation…
    expect(invokeMock.mock.calls.some((c) => c[0] === "read_tracked_repo")).toBe(true);
    // …and a never-fetched repo is stale, so a background fetch is kicked.
    await vi.waitFor(() =>
      expect(invokeMock.mock.calls.some((c) => c[0] === "fetch_repo")).toBe(true),
    );
  });

  it("fetchAll fetches every tracked repo", async () => {
    wire({ list: [listing("/a"), listing("/b"), listing("/c")] });
    await refreshAll();
    invokeMock.mockClear();
    wire({ list: [listing("/a"), listing("/b"), listing("/c")] });
    await fetchAll();
    const fetched = invokeMock.mock.calls
      .filter((c) => c[0] === "fetch_repo")
      .map((c) => (c[1] as { path: string }).path)
      .sort();
    expect(fetched).toEqual(["/a", "/b", "/c"]);
  });
});
