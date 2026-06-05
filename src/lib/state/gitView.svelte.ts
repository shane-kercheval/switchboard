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
import type {
  BranchKind,
  BranchView,
  GitCommitRange,
  GitCommitSummary,
  RepoListing,
} from "$lib/types";

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

/// A branch (or remote-tracking ref) selected in the tree. Identifies it for the
/// on-demand commit read; `kind` picks the local vs. remote ref namespace. When
/// set, the tree expands this branch to show its commits.
export type SelectedRef = {
  repoRoot: string;
  kind: BranchKind;
  /// Branch shorthand for a local branch (`feature`), or the remote-tracking name
  /// for a remote branch (`origin/feature`).
  name: string;
};

/// What the right-hand panel shows: either a worktree's *uncommitted* changes
/// (needs a checked-out folder) or one *commit's* diff (committed history; no
/// folder required, so it serves branches with no folder and remote-only refs).
/// `title`/`subtitle` are the panel header text, resolved at selection time so the
/// panel is pure presentation.
export type DiffTarget =
  | {
      kind: "uncommitted";
      repoRoot: string;
      worktreePath: string;
      title: string;
      subtitle: string;
    }
  | {
      kind: "commit";
      repoRoot: string;
      oid: string;
      shortOid: string;
      title: string;
      subtitle: string;
    };

/// The branch whose commits are expanded in the tree, or `null`. Session-only UI
/// state (like `view.mode`).
export const branchSelection = $state<{ current: SelectedRef | null }>({ current: null });

/// The commit ranges for the selected branch (loaded on demand). `ref` is the
/// branch the `ranges` belong to, so a late response for a since-changed selection
/// can be discarded.
export const branchCommits = $state<{
  ref: SelectedRef | null;
  status: "loading" | "loaded" | "failed";
  ranges: GitCommitRange[];
}>({ ref: null, status: "loaded", ranges: [] });

/// The diff shown in the right panel, or `null` when nothing is selected.
export const diffTarget = $state<{ current: DiffTarget | null }>({ current: null });

/// Monotonic signal for the diff panel: a repo refresh can change a worktree's
/// uncommitted diff without changing the selected target, so the panel depends on
/// this in addition to the target identity.
export const gitRefresh = $state<{ revision: number }>({ revision: 0 });

function refsEqual(a: SelectedRef | null, b: SelectedRef | null): boolean {
  return (
    a !== null && b !== null && a.repoRoot === b.repoRoot && a.kind === b.kind && a.name === b.name
  );
}

/// The newest commit across a branch's ranges (ranges are newest-first), or
/// `undefined` for an empty branch.
function firstCommit(ranges: GitCommitRange[]): GitCommitSummary | undefined {
  for (const range of ranges) {
    if (range.commits.length > 0) return range.commits[0];
  }
  return undefined;
}

function commitSubtitle(commit: GitCommitSummary): string {
  return commit.author_name === null
    ? commit.short_oid
    : `${commit.short_oid} · ${commit.author_name}`;
}

/// Select (and expand) a branch: load its commits and pick a default diff target
/// — its uncommitted changes when the worktree is dirty, otherwise its latest
/// commit once the list loads. Re-selecting the same branch collapses it.
export async function selectBranch(
  ref: SelectedRef,
  opts: { worktreePath: string | null; hasChanges: boolean; worktreeSubtitle: string },
): Promise<void> {
  if (refsEqual(branchSelection.current, ref)) {
    clearBranchSelection();
    return;
  }
  branchSelection.current = ref;
  branchCommits.ref = ref;
  branchCommits.status = "loading";
  branchCommits.ranges = [];

  // Default target: uncommitted-if-dirty now; otherwise the latest commit, once
  // the commit list resolves below.
  diffTarget.current =
    opts.worktreePath !== null && opts.hasChanges
      ? {
          kind: "uncommitted",
          repoRoot: ref.repoRoot,
          worktreePath: opts.worktreePath,
          title: "Uncommitted changes",
          subtitle: opts.worktreeSubtitle,
        }
      : null;

  try {
    const ranges = await api.branchCommits(ref.repoRoot, ref.kind, ref.name);
    if (!refsEqual(branchCommits.ref, ref)) return; // selection moved on while loading
    branchCommits.ranges = ranges;
    branchCommits.status = "loaded";
    if (diffTarget.current === null) {
      const first = firstCommit(ranges);
      if (first !== undefined) selectCommit(ref.repoRoot, first);
    }
  } catch (e) {
    if (!refsEqual(branchCommits.ref, ref)) return;
    console.warn("[switchboard] git view branch commits failed", { ref, error: e });
    branchCommits.status = "failed";
  }
}

/// Show a single commit's diff in the right panel.
export function selectCommit(repoRoot: string, commit: GitCommitSummary): void {
  diffTarget.current = {
    kind: "commit",
    repoRoot,
    oid: commit.oid,
    shortOid: commit.short_oid,
    title: commit.subject.length > 0 ? commit.subject : commit.short_oid,
    subtitle: commitSubtitle(commit),
  };
}

/// Show a worktree's uncommitted changes in the right panel.
export function selectUncommitted(repoRoot: string, worktreePath: string, subtitle: string): void {
  diffTarget.current = {
    kind: "uncommitted",
    repoRoot,
    worktreePath,
    title: "Uncommitted changes",
    subtitle,
  };
}

/// Collapse the selected branch and close the right panel.
export function clearBranchSelection(): void {
  branchSelection.current = null;
  branchCommits.ref = null;
  branchCommits.ranges = [];
  branchCommits.status = "loaded";
  diffTarget.current = null;
}

/// Re-read the selected branch's commit ranges in place (after a refresh that may
/// have added commits, e.g. a new local commit or fetched incoming commits).
/// Keeps the current `diffTarget`; a transient failure leaves the prior list.
async function reloadSelectedCommits(): Promise<void> {
  const ref = branchSelection.current;
  if (ref === null) return;
  try {
    const ranges = await api.branchCommits(ref.repoRoot, ref.kind, ref.name);
    if (!refsEqual(branchSelection.current, ref)) return;
    branchCommits.ranges = ranges;
    branchCommits.status = "loaded";
  } catch (e) {
    console.warn("[switchboard] git view commit reload failed", { ref, error: e });
  }
}

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
  // Gate on whether the *aggregate* has loaded, not on cache size: the project
  // panel (`loadProjectRepo`) upserts a single repo into the shared cache while
  // in Projects mode, so a non-empty cache no longer implies the full list was
  // read. `status === "complete"` is set only by a full `loadTrackedRepos`
  // (refreshAll / add / remove), so it's the precise "aggregate loaded" signal —
  // without it, entering the Git view after a project-panel read would show only
  // that one repo and hide every other tracked repo.
  if (gitView.status !== "complete") {
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
/// Returns the listing it read (also upserted into `gitView.repos`), or `null`
/// on failure — callers that need the resolved root (e.g. the project panel) use
/// it; the fire-and-forget callers ignore it.
export async function refreshRepo(root: string): Promise<RepoListing | null> {
  try {
    const listing = await api.readTrackedRepo(root);
    upsertRepo(listing);
    return listing;
  } catch (e) {
    console.warn("[switchboard] git view refreshRepo failed", { root, error: e });
    return null;
  }
}

/// Whether a repo's last fetch is stale (never fetched, or older than the fetch
/// window) — the gate shared by the entry refresh and the project panel so the
/// two surfaces don't double-fetch the same repo.
function isFetchStale(root: string): boolean {
  const rt = runtime.get(root);
  if (rt === undefined) return true;
  return rt.fetch.kind === "never" || performance.now() - rt.fetch.at > FETCH_STALE_MS;
}

/// Whether a repo's last local read is stale (never read, or older than the local
/// window) — the read counterpart to `isFetchStale`, shared by the entry refresh
/// and the project panel so a remount within the window doesn't re-hit the backend.
function isReadStale(root: string): boolean {
  const rt = runtime.get(root);
  return rt === undefined || performance.now() - rt.lastRead > LOCAL_STALE_MS;
}

/// Background-fetch every repo whose fetch is stale, bounded by FETCH_CONCURRENCY.
async function fetchStaleRepos(): Promise<void> {
  const due = gitView.repos.map((r) => r.repo.root).filter(isFetchStale);
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

/// Load the repo a project lives in, for the Projects-side git status panel
/// (M6). Reuses the Git view's read + fetch + dedup (shared `gitView.repos`
/// cache, no new fetch machinery): the panel `$effect` remounts on every sidebar
/// toggle, so the read is **read-staleness-gated** like the Git view — a repo
/// already read within the local window is served from cache rather than
/// re-hitting the backend. A genuinely stale (or never-loaded) repo is re-read.
/// Either way a fetch-stale repo gets a background fetch. `path` is the project's
/// worktree directory; it resolves to the repo root.
export async function loadProjectRepo(path: string): Promise<void> {
  const cached = loadedRepoForWorktree(path);
  const listing = cached && !isReadStale(cached.repo.root) ? cached : await refreshRepo(path);
  if (listing?.repo.available && isFetchStale(listing.repo.root)) {
    void fetchRepo(listing.repo.root);
  }
}

/// The already-loaded repo whose worktree set includes `path`, or `undefined`.
/// A path-spelling mismatch simply returns `undefined` (→ a fresh read), so this
/// only ever skips a redundant read, never serves the wrong repo.
function loadedRepoForWorktree(path: string): RepoListing | undefined {
  return gitView.repos.find(
    (r) =>
      r.repo.local_branches.some((b) => b.worktree?.path === path) ||
      r.repo.detached_worktrees.some((w) => w.path === path),
  );
}

/// The `BranchView` for the worktree a project lives in, plus that repo's default
/// branch (for badge computation), or `null` when not resolvable yet — the repo
/// isn't loaded, the project's worktree is detached (no branch), or the directory
/// isn't a tracked git repo. Matches on the backend-computed project↔worktree
/// linking (robust against path-spelling differences), reading from the reactive
/// `gitView.repos`, so a caller in a `$derived` re-runs as repos load/refresh.
export function projectBranch(
  projectId: string,
): { branch: BranchView; defaultBranch: string | null } | null {
  for (const listing of gitView.repos) {
    const worktreePath = Object.keys(listing.linked_projects).find((p) =>
      listing.linked_projects[p]?.some((lp) => lp.id === projectId),
    );
    if (worktreePath === undefined) continue;
    const branch = listing.repo.local_branches.find((b) => b.worktree?.path === worktreePath);
    if (branch) return { branch, defaultBranch: listing.repo.default_branch };
  }
  return null;
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
  afterRefresh(repos, true);
}

/// Reconcile the selection against refreshed listings and keep the open panel
/// live. `fullList` distinguishes a whole-list replace (a missing selected repo
/// means it was removed → clear) from a single-repo upsert (only that repo is
/// authoritative).
function afterRefresh(repos: RepoListing[], fullList: boolean): void {
  const sel = branchSelection.current;
  if (sel !== null) {
    const listing = repos.find((r) => r.repo.root === sel.repoRoot);
    if (fullList && listing === undefined) {
      clearBranchSelection();
    } else if (listing !== undefined && !branchExists(listing, sel)) {
      // The selected branch was deleted out from under the open commit list.
      clearBranchSelection();
    } else if (listing !== undefined) {
      // The selected branch's repo was re-read — its history may have changed.
      void reloadSelectedCommits();
    }
  }
  // Reconcile an open uncommitted diff target against the refreshed repo. (A
  // commit target is immutable and its branch is handled above, so it needs no
  // worktree check.)
  const target = diffTarget.current;
  if (target === null || target.kind !== "uncommitted") return;
  const listing = repos.find((r) => r.repo.root === target.repoRoot);
  if (listing === undefined) return; // the target's repo wasn't in this refresh
  if (worktreeExists(listing, target.worktreePath)) {
    // Still checked out → its working-tree content may have changed; re-read it.
    gitRefresh.revision += 1;
  } else if (branchSelection.current !== null) {
    // The selected branch lost its folder but still exists — fall back to its
    // latest commit so the panel doesn't dangle over a gone path. (Uses the
    // current ranges; a worktree removal doesn't rewrite history.)
    const latest = firstCommit(branchCommits.ranges);
    if (latest !== undefined) selectCommit(target.repoRoot, latest);
    else diffTarget.current = null;
  } else {
    // A detached worktree (no branch selected) was pruned → close the panel.
    diffTarget.current = null;
  }
}

/// Whether a worktree path is still checked out in a listing — a branch's folder
/// or a detached worktree.
function worktreeExists(listing: RepoListing, path: string): boolean {
  return (
    listing.repo.local_branches.some((b) => b.worktree?.path === path) ||
    listing.repo.detached_worktrees.some((w) => w.path === path)
  );
}

function branchExists(listing: RepoListing, ref: SelectedRef): boolean {
  return ref.kind === "local"
    ? listing.repo.local_branches.some((b) => b.name === ref.name)
    : listing.repo.remote_branches.some((b) => b.name === ref.name);
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
  // A single-repo update (per-repo refresh, post-fetch re-read, project-panel
  // load) can drop the selected branch or change its history — reconcile here
  // too, not just in the full-list `applyRepos`, so the panel never dangles.
  afterRefresh([listing], false);
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
    branchSelection.current = null;
    branchCommits.ref = null;
    branchCommits.ranges = [];
    branchCommits.status = "loaded";
    diffTarget.current = null;
    gitRefresh.revision = 0;
  },
  runtimeSize(): number {
    return runtime.size;
  },
};
