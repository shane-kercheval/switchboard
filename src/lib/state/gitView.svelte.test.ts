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
  fetchRepo,
  fetchAll,
  addRepo,
  removeRepo,
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

/// Route invoke by command; default list/read/fetch all succeed.
function wire(opts: { list?: RepoListing[]; fetchRejects?: boolean } = {}) {
  invokeMock.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "list_tracked_repos") return Promise.resolve(opts.list ?? []);
    if (cmd === "read_tracked_repo") return Promise.resolve(listing(String(args?.path)));
    if (cmd === "fetch_repo") {
      return opts.fetchRejects ? Promise.reject(new Error("no remote")) : Promise.resolve(null);
    }
    return Promise.resolve(null);
  });
}

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
