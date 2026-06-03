// Git-view state: the top-level view mode, the tracked-repo listings, and the
// staleness-gated refresh + bounded background fetch.
//
// **No polling** (decision D3): the view is an honest point-in-time snapshot.
// Entering it re-reads any repo whose local data is stale (> LOCAL_STALE_MS) and
// kicks a background fetch for any whose fetch is stale (> FETCH_STALE_MS); fresh
// repos serve from the last result. Manual refresh/fetch always force. Timestamps
// are per-repo and in-memory only (a fresh process reads fresh).
//
// **View mode is session-only** (decision D5): the app always opens to Projects;
// this never persists. It lives here (not component-local) so it's testable.

import * as api from "$lib/api";
import type { RepoListing } from "$lib/types";

/// Re-read a repo's local git state if its last read is older than this. The
/// local read is cheap + offline, so a short window keeps "I just committed,
/// flip to Git view" feeling live without polling.
const LOCAL_STALE_MS = 30_000;
/// Kick a background fetch if the last fetch is older than this. Network-bound,
/// so a longer window than the local read.
const FETCH_STALE_MS = 60_000;
/// Cap concurrent background fetches so entering a many-repo workspace doesn't
/// spawn a burst of `git fetch` subprocesses / overlapping auth prompts.
const FETCH_CONCURRENCY = 4;

export type ViewMode = "projects" | "git";

/// Per-repo fetch state, mirroring the backend model: never fetched, last fetch
/// failed, or succeeded at a time. Drives the quiet "last fetched"/"fetch
/// failed" label — fetch failure is shown, never thrown as a toast.
export type FetchState =
  | { kind: "never" }
  | { kind: "failed"; at: number }
  | { kind: "ok"; at: number };

type RepoRuntime = {
  /// Monotonic ms (performance.now) of the last successful local read.
  lastRead: number;
  fetch: FetchState;
};

export const view = $state<{ mode: ViewMode }>({ mode: "projects" });

/// The tracked repos, in registry order. `status` distinguishes the first load
/// (nothing rendered yet) from a populated/failed view.
export const gitView = $state<{
  repos: RepoListing[];
  status: "pending" | "loading" | "complete" | "failed";
}>({ repos: [], status: "pending" });

/// Per-repo refresh/fetch bookkeeping, keyed by canonical repo root (the
/// `RepoListing.repo.root` string). Not reactive UI state — plain maps.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const runtime = new Map<string, RepoRuntime>();
/// In-flight fetch guard: the running fetch promise per root. A second request
/// for a root already fetching *joins* that promise (awaits the same operation)
/// rather than starting a second subprocess — so `fetchAll`/manual refresh only
/// resolve once the real fetch (and its follow-up re-read) is done.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const inFlightFetch = new Map<string, Promise<void>>();

/// Per-repo "last fetched" / "fetch failed" state for the UI label. Reactive so
/// the indicator updates when a background fetch resolves.
export const fetchStates = $state<Record<string, FetchState>>({});

export function setViewMode(mode: ViewMode): void {
  view.mode = mode;
}

/// Switch into the Git view and run the staleness-gated entry refresh.
export async function enterGitView(): Promise<void> {
  view.mode = "git";
  await refreshStale();
}

/// Aggregate read of every tracked repo. **Pure read** — no fetch. Used as the
/// global manual refresh's read half; the component pairs it with `fetchAll`,
/// and the entry path (`refreshStale`) pairs it with the staleness-gated fetch.
/// Keeping fetch out of here avoids double-fetching on a global refresh and keeps
/// the read independently reasoned about.
async function loadTrackedRepos(): Promise<void> {
  const repos = await api.listTrackedRepos();
  applyRepos(repos);
  gitView.status = "complete";
}

export async function refreshAll(): Promise<void> {
  gitView.status = gitView.repos.length === 0 ? "loading" : gitView.status;
  try {
    await loadTrackedRepos();
  } catch (e) {
    console.warn("[switchboard] git view refreshAll failed", e);
    gitView.status = "failed";
  }
}

/// Track a repo by an explicit "Add Repo" action: the path is resolved to its
/// canonical root and added (a subdirectory / linked worktree of an
/// already-tracked repo dedups). On success the list is re-read so the repo
/// appears, then a staleness-gated fetch refreshes its sync state.
///
/// Unlike the passive global refresh, this is a *mutation* the user just
/// triggered, so it must report the truth: it re-reads via the **throwing**
/// `loadTrackedRepos` (not best-effort `refreshAll`) so that either the add and
/// its re-read both succeed, or the error propagates to the caller for an inline
/// surface — never a silent success that leaves the new repo invisible. A non-git
/// path also rejects from the backend through the same channel.
export async function addRepo(path: string): Promise<void> {
  await api.addTrackedRepo(path);
  await loadTrackedRepos();
  void fetchStaleRepos();
}

/// Untrack a repo ("Remove from view"): registry-only — never touches files or
/// the workspace. The list is re-read so the row disappears and its runtime /
/// fetch bookkeeping is dropped. Re-reads via the throwing primitive (same
/// honesty rationale as `addRepo`): a failed re-read surfaces rather than leaving
/// the removed row on screen as a false success.
export async function removeRepo(path: string): Promise<void> {
  await api.removeTrackedRepo(path);
  await loadTrackedRepos();
}

/// Entry refresh (called on view entry): full read if nothing's loaded, else
/// re-read only the locally-stale repos, then kick a background fetch for the
/// fetch-stale ones. The fetch is fire-and-forget so the tree paints immediately.
export async function refreshStale(): Promise<void> {
  if (gitView.repos.length === 0) {
    await refreshAll();
  } else {
    const now = performance.now();
    const stale = gitView.repos.filter((r) => {
      const rt = runtime.get(r.repo.root);
      return rt === undefined || now - rt.lastRead > LOCAL_STALE_MS;
    });
    await Promise.all(stale.map((r) => refreshRepo(r.repo.root)));
  }
  void fetchStaleRepos();
}

/// Force a single repo's local re-read (per-repo refresh button / staleness).
export async function refreshRepo(root: string): Promise<void> {
  try {
    const listing = await api.readTrackedRepo(root);
    upsertRepo(listing);
  } catch (e) {
    console.warn("[switchboard] git view refreshRepo failed", { root, error: e });
  }
}

/// Background-fetch every repo whose fetch is stale, bounded by FETCH_CONCURRENCY.
async function fetchStaleRepos(): Promise<void> {
  const now = performance.now();
  const due = gitView.repos
    .map((r) => r.repo.root)
    .filter((root) => {
      const rt = runtime.get(root);
      if (rt === undefined) return true;
      return rt.fetch.kind === "never" || now - rt.fetch.at > FETCH_STALE_MS;
    });
  await runBounded(due, FETCH_CONCURRENCY, fetchRepo);
}

/// Fetch one repo (manual per-repo fetch, or the staleness pass). Deduped: a
/// fetch already running for this root joins the first. On success, re-reads the
/// repo's local state so updated sync/behind-base land. Failure degrades to a
/// `failed` fetch state — never thrown.
export async function fetchRepo(root: string): Promise<void> {
  const existing = inFlightFetch.get(root);
  if (existing !== undefined) return existing;

  const run = (async () => {
    try {
      await api.fetchRepo(root);
      recordFetch(root, { kind: "ok", at: performance.now() });
      await refreshRepo(root);
    } catch (e) {
      console.warn("[switchboard] git view fetchRepo failed", { root, error: e });
      recordFetch(root, { kind: "failed", at: performance.now() });
    } finally {
      inFlightFetch.delete(root);
    }
  })();
  inFlightFetch.set(root, run);
  return run;
}

/// Force a fetch of every tracked repo (global fetch button), ignoring staleness.
export async function fetchAll(): Promise<void> {
  const roots = gitView.repos.map((r) => r.repo.root);
  await runBounded(roots, FETCH_CONCURRENCY, fetchRepo);
}

// --- internals --------------------------------------------------------------

function applyRepos(repos: RepoListing[]): void {
  gitView.repos = repos;
  const now = performance.now();
  for (const r of repos) {
    const existing = runtime.get(r.repo.root);
    runtime.set(r.repo.root, {
      lastRead: now,
      fetch: existing?.fetch ?? { kind: "never" },
    });
  }
  // Drop runtime for repos no longer tracked.
  const live = new Set(repos.map((r) => r.repo.root));
  for (const root of [...runtime.keys()]) {
    if (!live.has(root)) {
      runtime.delete(root);
      delete fetchStates[root];
    }
  }
}

function upsertRepo(listing: RepoListing): void {
  const root = listing.repo.root;
  const idx = gitView.repos.findIndex((r) => r.repo.root === root);
  if (idx === -1) {
    gitView.repos = [...gitView.repos, listing];
  } else {
    gitView.repos[idx] = listing;
  }
  const existing = runtime.get(root);
  runtime.set(root, {
    lastRead: performance.now(),
    fetch: existing?.fetch ?? { kind: "never" },
  });
}

function recordFetch(root: string, state: FetchState): void {
  const rt = runtime.get(root);
  // The repo was untracked while this fetch was in flight (`removeRepo`'s re-read
  // dropped its runtime + fetch-state). Don't resurrect a dangling key for an
  // untracked root.
  if (rt === undefined) return;
  rt.fetch = state;
  fetchStates[root] = state;
}

/// Run `task` over `items` with at most `limit` concurrent. Failures are
/// swallowed by each task (fetchRepo never throws), so this always resolves.
async function runBounded<T>(
  items: T[],
  limit: number,
  task: (item: T) => Promise<void>,
): Promise<void> {
  let cursor = 0;
  const worker = async (): Promise<void> => {
    while (cursor < items.length) {
      const item = items[cursor++]!;
      await task(item);
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
}

/// Test-only reset.
export const _testing = {
  reset(): void {
    view.mode = "projects";
    gitView.repos = [];
    gitView.status = "pending";
    runtime.clear();
    inFlightFetch.clear();
    for (const k of Object.keys(fetchStates)) delete fetchStates[k];
  },
  runtimeSize(): number {
    return runtime.size;
  },
};
