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
import { SvelteMap, SvelteSet } from "svelte/reactivity";
import { compareIsoTimestampsDescending } from "$lib/utils";
import type {
  BranchComparison,
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
export type RepoSortMode = "recent" | "alphabetical";

/// Per-repo fetch state, mirroring the backend model: never fetched, last fetch
/// failed, or succeeded at a time. Drives the quiet fetch-failure indicator —
/// fetch failure is shown, never thrown as a toast.
export type FetchState =
  | { kind: "never" }
  | { kind: "failed"; at: number }
  | { kind: "ok"; at: number };

export type RevealProjectBranchResult =
  | { kind: "revealed" }
  | { kind: "unresolved" }
  | { kind: "superseded" }
  | { kind: "failed"; message: string };

type RepoRuntime = {
  /// Monotonic ms (performance.now) of the last successful local read.
  lastRead: number;
  fetch: FetchState;
};

export const view = $state<{ mode: ViewMode }>({ mode: "projects" });

/// Session-only tree position. GitView is unmounted when the user returns to
/// Projects, so this state lives beside the view mode rather than in the
/// component.
export const collapsedRepoRoots = new SvelteSet<string>();
export const repoListScroll = $state<{ top: number }>({ top: 0 });
/// Session-only sort choice and stable order, kept outside GitView so navigation
/// preserves both across component remounts.
export const repoSort = $state<{ mode: RepoSortMode; roots: string[] }>({
  mode: "recent",
  roots: [],
});

// A consume-once request for GitView to expand and scroll to one repository.
const repoReveal = $state<{ root: string | null }>({ root: null });

export function requestRepoReveal(root: string): void {
  repoReveal.root = root;
}

/// Supersede pending project navigation when the user explicitly switches
/// views. Temporary overlays leave it intact for GitView to consume on return.
export function cancelProjectBranchReveal(): void {
  gitRevealSeq += 1;
  repoReveal.root = null;
}

export function takeRepoReveal(): string | null {
  const root = repoReveal.root;
  repoReveal.root = null;
  return root;
}

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

/// What the right-hand panel shows: a branch comparison, a worktree's
/// *uncommitted* changes, or one *commit's* diff. Comparisons and commits need no
/// folder, so branches without a worktree and remote-only refs remain inspectable.
/// `title`/`subtitle` are the panel header text, resolved at selection time so the
/// panel is pure presentation.
export type DiffTarget =
  | {
      kind: "comparison";
      repoRoot: string;
      branchName: string;
      worktreePath: string | null;
      comparison: BranchComparison;
      title: string;
      subtitle: string;
    }
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

export type ComparisonBase = { kind: BranchKind; name: string };

/// The aggregate branch comparison for the expanded branch. Loaded on demand
/// beside its commit ranges; `base === null` means automatic default-branch
/// resolution, while a value records the user's session-only override.
export const branchComparison = $state<{
  ref: SelectedRef | null;
  status: "loading" | "loaded" | "failed";
  result: BranchComparison | null;
  base: ComparisonBase | null;
  pendingBase: ComparisonBase | null | undefined;
  error: string | null;
  worktreePath: string | null;
}>({
  ref: null,
  status: "loaded",
  result: null,
  base: null,
  pendingBase: undefined,
  error: null,
  worktreePath: null,
});

let branchSelectionEpoch = 0;
let branchCommitsRequestId = 0;
let branchComparisonRequestId = 0;

function comparisonBasesEqual(a: ComparisonBase | null, b: ComparisonBase | null): boolean {
  return a === null ? b === null : b !== null && a.kind === b.kind && a.name === b.name;
}

/// The diff shown in the right panel, or `null` when nothing is selected.
export const diffTarget = $state<{ current: DiffTarget | null }>({ current: null });

/// Which pane the arrow keys navigate, set by the last selection the user made:
/// picking a commit / uncommitted row focuses the commit pane, picking a changed
/// file focuses the file pane. `null` when nothing is selected.
export const navFocus = $state<{ pane: "commits" | "files" | null }>({ pane: null });

/// Set true on keyboard navigation so the row under the (stationary) mouse drops
/// its hover highlight — otherwise two rows look active at once. Cleared on the
/// next pointer move, so hover returns the instant the mouse is used again.
export const hoverSuppressed = $state<{ value: boolean }>({ value: false });

/// A hover-only class (background or `group-hover:` reveal) gated on
/// [`hoverSuppressed`]: empty while keyboard navigation is active, so the rule
/// doesn't fire under a stationary mouse, and back to `cls` on the next pointer
/// move. Used wherever a Git-view row's mouse-hover affordance must yield to the
/// keyboard selection.
export function hoverableClass(cls: string): string {
  return hoverSuppressed.value ? "" : cls;
}

/// Repo roots whose branch-actions menu is currently open. The commit keyboard
/// navigator bails while any is open so the arrows drive the menu, not the commit
/// list — global (not node-local) so a menu open in one repo also yields the keys
/// when the selected commit lives in a different repo node. A keyed set (not a
/// counter) so it's idempotent: a stray double-close or a reset can't drive it
/// negative. Read at event time, so plain (non-reactive) state is enough.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const openBranchMenuRoots = new Set<string>();

/// Mark (or clear) a repo node's branch-actions menu as open. Idempotent.
export function setBranchMenuOpen(repoRoot: string, open: boolean): void {
  if (open) openBranchMenuRoots.add(repoRoot);
  else openBranchMenuRoots.delete(repoRoot);
}

/// Whether any repo node has a branch-actions menu open.
export function anyBranchMenuOpen(): boolean {
  return openBranchMenuRoots.size > 0;
}

/// A navigable entry in the commit pane: the aggregate branch comparison, the
/// worktree's uncommitted row, or one commit. Built by the component from live
/// data; consumed by [`nextCommitSelection`].
export type CommitNavItem =
  | {
      kind: "comparison";
      branchName: string;
      worktreePath: string | null;
      comparison: BranchComparison;
    }
  | { kind: "uncommitted"; worktreePath: string }
  | { kind: "commit"; commit: GitCommitSummary };

/// The index to move to when stepping `delta` (+1 down, -1 up) from `current`
/// within a list of `len` entries, clamped at both ends (no wrap). When nothing
/// is current (`-1`), a downward step starts at the top and an upward step at the
/// bottom. Returns `null` when the list is empty. Shared by the commit and file
/// pane navigators so the two keep identical step/clamp semantics.
export function nextIndex(len: number, current: number, delta: number): number | null {
  if (len === 0) return null;
  if (current === -1) return delta > 0 ? 0 : len - 1;
  return Math.min(len - 1, Math.max(0, current + delta));
}

/// The entry to select when moving `delta` (+1 down, -1 up) from the `current`
/// diff target within `items`. Returns `null` when there's nothing to move to.
export function nextCommitSelection(
  items: CommitNavItem[],
  current: DiffTarget | null,
  delta: number,
): CommitNavItem | null {
  const idx = items.findIndex((item) =>
    current === null
      ? false
      : item.kind === "comparison"
        ? current.kind === "comparison" &&
          current.comparison.merge_base_oid === item.comparison.merge_base_oid &&
          current.comparison.head_oid === item.comparison.head_oid
        : item.kind === "uncommitted"
          ? current.kind === "uncommitted" && current.worktreePath === item.worktreePath
          : current.kind === "commit" && current.oid === item.commit.oid,
  );
  const next = nextIndex(items.length, idx, delta);
  return next === null ? null : items[next]!;
}

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
  return [commit.short_oid, readableCommitTimestamp(commit.authored_at), commit.author_name]
    .filter((part): part is string => part !== null && part.length > 0)
    .join(" · ");
}

// Locale-aware detail text reads better in the wider inspector header.
function readableCommitTimestamp(iso: string | null): string | null {
  if (iso === null) return null;
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return null;
  return date.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

/// Select (and expand) a branch: immediately show uncommitted changes for a
/// dirty worktree while its aggregate comparison and commits load. Clean
/// branches default to the comparison, then the latest commit when no
/// base/common ancestor resolves. Re-selecting collapses the branch.
export async function selectBranch(
  ref: SelectedRef,
  opts: { worktreePath: string | null; hasChanges: boolean; worktreeSubtitle: string },
): Promise<void> {
  if (refsEqual(branchSelection.current, ref)) {
    clearBranchSelection();
    return;
  }
  const selectionEpoch = ++branchSelectionEpoch;
  const commitsRequestId = ++branchCommitsRequestId;
  const comparisonRequestId = ++branchComparisonRequestId;
  branchSelection.current = ref;
  branchCommits.ref = ref;
  branchCommits.status = "loading";
  branchCommits.ranges = [];
  branchComparison.ref = ref;
  branchComparison.status = "loading";
  branchComparison.result = null;
  branchComparison.base = null;
  branchComparison.pendingBase = undefined;
  branchComparison.error = null;
  branchComparison.worktreePath = opts.worktreePath;
  diffTarget.current = null;
  // Selecting a branch makes its commit list the arrow-key target regardless of
  // which aggregate/uncommitted/commit fallback becomes the default.
  navFocus.pane = "commits";
  if (opts.worktreePath !== null && opts.hasChanges) {
    selectUncommitted(ref.repoRoot, opts.worktreePath, opts.worktreeSubtitle);
  }

  const comparisonRead = api
    .branchComparison(ref.repoRoot, ref.kind, ref.name, null, opts.worktreePath)
    .then(
      (result) => {
        if (
          branchSelectionEpoch === selectionEpoch &&
          branchComparisonRequestId === comparisonRequestId &&
          refsEqual(branchSelection.current, ref)
        ) {
          branchComparison.result = result;
          branchComparison.status = "loaded";
        }
        return result;
      },
      (error: unknown) => {
        console.warn("[switchboard] git view branch comparison failed", { ref, error });
        if (
          branchSelectionEpoch === selectionEpoch &&
          branchComparisonRequestId === comparisonRequestId &&
          refsEqual(branchSelection.current, ref)
        ) {
          branchComparison.status = "failed";
          branchComparison.error = "Couldn't load branch changes.";
        }
        return null;
      },
    );
  const commitsRead = api.branchCommits(ref.repoRoot, ref.kind, ref.name).then(
    (ranges) => {
      if (
        branchSelectionEpoch === selectionEpoch &&
        branchCommitsRequestId === commitsRequestId &&
        refsEqual(branchSelection.current, ref)
      ) {
        branchCommits.ranges = ranges;
        branchCommits.status = "loaded";
      }
      return ranges;
    },
    (error: unknown) => {
      console.warn("[switchboard] git view branch commits failed", { ref, error });
      if (
        branchSelectionEpoch === selectionEpoch &&
        branchCommitsRequestId === commitsRequestId &&
        refsEqual(branchSelection.current, ref)
      ) {
        branchCommits.status = "failed";
      }
      return [];
    },
  );

  await Promise.all([comparisonRead, commitsRead]);
  if (branchSelectionEpoch !== selectionEpoch || !refsEqual(branchSelection.current, ref)) return;

  if (diffTarget.current !== null) return;
  if (branchComparison.result !== null) {
    selectBranchComparison(ref.repoRoot, ref.name, branchComparison.result, opts.worktreePath);
  } else {
    const first = firstCommit(branchCommits.ranges);
    if (first !== undefined) selectCommit(ref.repoRoot, first);
  }
}

export function selectBranchComparison(
  repoRoot: string,
  branchName: string,
  comparison: BranchComparison,
  worktreePath: string | null,
): void {
  diffTarget.current = {
    kind: "comparison",
    repoRoot,
    branchName,
    worktreePath,
    comparison,
    title: "Branch changes",
    subtitle: `${branchName} · compared with ${comparison.base_label}${comparison.includes_worktree ? " · includes uncommitted changes" : ""}`,
  };
  navFocus.pane = "commits";
}

export async function compareSelectedBranchAgainst(base: ComparisonBase | null): Promise<void> {
  const ref = branchSelection.current;
  if (ref === null) return;
  const selectionEpoch = branchSelectionEpoch;
  const requestId = ++branchComparisonRequestId;
  const worktreePath = branchComparison.worktreePath;
  branchComparison.status = "loading";
  branchComparison.pendingBase = base;
  branchComparison.error = null;
  try {
    const result = await api.branchComparison(ref.repoRoot, ref.kind, ref.name, base, worktreePath);
    if (
      branchSelectionEpoch !== selectionEpoch ||
      branchComparisonRequestId !== requestId ||
      !refsEqual(branchSelection.current, ref) ||
      branchComparison.pendingBase === undefined ||
      !comparisonBasesEqual(branchComparison.pendingBase, base)
    )
      return;
    branchComparison.pendingBase = undefined;
    branchComparison.base = base;
    branchComparison.result = result;
    branchComparison.status = "loaded";
    if (result !== null) {
      selectBranchComparison(ref.repoRoot, ref.name, result, worktreePath);
    } else if (diffTarget.current?.kind === "comparison") {
      diffTarget.current = null;
    }
  } catch (e) {
    if (
      branchSelectionEpoch !== selectionEpoch ||
      branchComparisonRequestId !== requestId ||
      !refsEqual(branchSelection.current, ref) ||
      branchComparison.pendingBase === undefined ||
      !comparisonBasesEqual(branchComparison.pendingBase, base)
    )
      return;
    console.warn("[switchboard] git view comparison-base change failed", { ref, base, error: e });
    branchComparison.pendingBase = undefined;
    branchComparison.status = "failed";
    const baseLabel = base?.name ?? "the default branch";
    branchComparison.error =
      branchComparison.result === null
        ? `Couldn't compare with ${baseLabel}.`
        : `Couldn't compare with ${baseLabel}. Kept the previous comparison.`;
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
  navFocus.pane = "commits";
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
  navFocus.pane = "commits";
}

export function selectedWorktreePathForEditor(): string | null {
  const target = diffTarget.current;
  if (target?.kind === "uncommitted") return target.worktreePath;
  if (target?.kind === "comparison" && target.worktreePath !== null) return target.worktreePath;

  const selected = branchSelection.current;
  if (selected === null || selected.kind !== "local") return null;
  const listing = gitView.repos.find((repo) => repo.repo.root === selected.repoRoot);
  const branch = listing?.repo.local_branches.find((candidate) => candidate.name === selected.name);
  return branch?.worktree?.path ?? null;
}

/// Collapse the selected branch and close the right panel.
export function clearBranchSelection(): void {
  branchSelectionEpoch += 1;
  branchCommitsRequestId += 1;
  branchComparisonRequestId += 1;
  branchSelection.current = null;
  branchCommits.ref = null;
  branchCommits.ranges = [];
  branchCommits.status = "loaded";
  branchComparison.ref = null;
  branchComparison.status = "loaded";
  branchComparison.result = null;
  branchComparison.base = null;
  branchComparison.pendingBase = undefined;
  branchComparison.error = null;
  branchComparison.worktreePath = null;
  diffTarget.current = null;
  navFocus.pane = null;
}

/// Re-read the selected branch's commit ranges in place (after a refresh that may
/// have added commits, e.g. a new local commit or fetched incoming commits).
/// Keeps the current `diffTarget`; a transient failure leaves the prior list.
async function reloadSelectedCommits(): Promise<void> {
  const ref = branchSelection.current;
  if (ref === null) return;
  const selectionEpoch = branchSelectionEpoch;
  const requestId = ++branchCommitsRequestId;
  try {
    const ranges = await api.branchCommits(ref.repoRoot, ref.kind, ref.name);
    if (
      branchSelectionEpoch !== selectionEpoch ||
      branchCommitsRequestId !== requestId ||
      !refsEqual(branchSelection.current, ref)
    )
      return;
    branchCommits.ranges = ranges;
    branchCommits.status = "loaded";
  } catch (e) {
    console.warn("[switchboard] git view commit reload failed", { ref, error: e });
  }
}

async function reloadSelectedComparison(): Promise<void> {
  const ref = branchSelection.current;
  if (ref === null || !refsEqual(branchComparison.ref, ref)) return;
  const base = branchComparison.base;
  const worktreePath = branchComparison.worktreePath;
  const selectionEpoch = branchSelectionEpoch;
  const requestId = ++branchComparisonRequestId;
  try {
    const result = await api.branchComparison(ref.repoRoot, ref.kind, ref.name, base, worktreePath);
    if (
      branchSelectionEpoch !== selectionEpoch ||
      branchComparisonRequestId !== requestId ||
      !refsEqual(branchSelection.current, ref) ||
      !comparisonBasesEqual(branchComparison.base, base)
    )
      return;
    branchComparison.result = result;
    branchComparison.status = "loaded";
    branchComparison.error = null;
    if (diffTarget.current?.kind === "comparison") {
      if (result === null) diffTarget.current = null;
      else selectBranchComparison(ref.repoRoot, ref.name, result, worktreePath);
      gitRefresh.revision += 1;
    }
  } catch (e) {
    console.warn("[switchboard] git view comparison reload failed", { ref, base, error: e });
  }
}

/// The tracked repos, in registry order. `status` distinguishes the first load
/// (nothing rendered yet) from a populated/failed view.
export const gitView = $state<{
  repos: RepoListing[];
  status: "pending" | "loading" | "complete" | "failed";
}>({ repos: [], status: "pending" });

const repoNameCollator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

function compareRepoListings(a: RepoListing, b: RepoListing): number {
  if (repoSort.mode === "recent") {
    const aTime = a.repo.last_commit_at;
    const bTime = b.repo.last_commit_at;
    if (aTime !== null && bTime !== null) {
      const byTime = compareIsoTimestampsDescending(aTime, bTime);
      if (byTime !== 0) return byTime;
    } else if (aTime !== null) {
      return -1;
    } else if (bTime !== null) {
      return 1;
    }
  }
  const byName = repoNameCollator.compare(a.repo.name, b.repo.name);
  if (byName !== 0) return byName;
  if (a.repo.root === b.repo.root) return 0;
  return a.repo.root < b.repo.root ? -1 : 1;
}

/// Capture one stable repository order from the current data. Passive refreshes
/// may update rows afterward, but do not move them while the user is reading.
export function snapshotRepoSort(): void {
  repoSort.roots = [...gitView.repos].sort(compareRepoListings).map((listing) => listing.repo.root);
}

export function setRepoSortMode(mode: RepoSortMode): void {
  repoSort.mode = mode;
  snapshotRepoSort();
}

export function repoListingsInDisplayOrder(): RepoListing[] {
  const byRoot = new SvelteMap(gitView.repos.map((listing) => [listing.repo.root, listing]));
  const ordered = repoSort.roots.flatMap((root) => {
    const listing = byRoot.get(root);
    if (listing === undefined) return [];
    byRoot.delete(root);
    return [listing];
  });
  return [...ordered, ...byRoot.values()];
}

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
let gitRevealSeq = 0;

/// Per-repo fetch state for the UI failure indicator. Reactive so the indicator
/// updates when a background fetch resolves.
export const fetchStates = $state<Record<string, FetchState>>({});

export function setViewMode(mode: ViewMode): void {
  view.mode = mode;
}

/// Switch into the Git view and run the staleness-gated entry refresh.
export async function enterGitView(): Promise<void> {
  if (gitView.status === "complete") snapshotRepoSort();
  else repoSort.roots = [];
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

export async function refreshAll(opts: { snapshotOrder?: boolean } = {}): Promise<void> {
  gitView.status = gitView.repos.length === 0 ? "loading" : gitView.status;
  try {
    await loadTrackedRepos();
    if (opts.snapshotOrder !== false) snapshotRepoSort();
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
  snapshotRepoSort();
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
  snapshotRepoSort();
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
/// Returns the listing it read, or `null` on failure. Normal refreshes upsert
/// the result into `gitView.repos`; project-only probes can skip unavailable
/// rows so non-git directories don't appear in the shared Git cache.
export async function refreshRepo(
  root: string,
  opts: { upsertUnavailable?: boolean } = {},
): Promise<RepoListing | null> {
  try {
    const listing = await api.readTrackedRepo(root);
    if (listing.repo.available || opts.upsertUnavailable !== false) {
      upsertRepo(listing);
    }
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

/// Load the repo a project lives in for project→Git navigation. Reuses the Git
/// view's read + fetch + dedup (shared `gitView.repos` cache, no new fetch
/// machinery): a repo already read within the local window is served from cache
/// rather than re-hitting the backend. A genuinely stale (or never-loaded) repo
/// is re-read. Either way a fetch-stale repo gets a background fetch. `path` is
/// the project's worktree directory; it resolves to the repo root when tracked.
export async function loadProjectRepo(path: string): Promise<RepoListing | null> {
  const cached = loadedRepoForWorktree(path);
  const listing =
    cached && !isReadStale(cached.repo.root)
      ? cached
      : await refreshRepo(cached?.repo.root ?? path, {
          upsertUnavailable: cached !== undefined,
        });
  if (listing?.repo.available && isFetchStale(listing.repo.root)) {
    void fetchRepo(listing.repo.root);
  }
  return listing;
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

/// The `BranchView` for the worktree a project lives in, plus that repo's
/// identity, or `null` when not resolvable yet — the repo isn't loaded, the
/// project's worktree is detached (no branch), or the directory isn't a tracked
/// git repo. Matches on the backend-computed project↔worktree linking, which is
/// robust against path-spelling differences.
type ProjectBranchTarget = {
  repoRoot: string;
  branch: BranchView;
  defaultBranch: string | null;
  worktreePath: string;
};

type ProjectBranchResolveResult =
  | { kind: "resolved"; target: ProjectBranchTarget }
  | { kind: "unresolved" }
  | { kind: "failed"; message: string };

function projectBranchTarget(projectId: string): ProjectBranchTarget | null {
  for (const listing of gitView.repos) {
    const worktreePath = Object.keys(listing.linked_projects).find((p) =>
      listing.linked_projects[p]?.some((lp) => lp.id === projectId),
    );
    if (worktreePath === undefined) continue;
    const branch = listing.repo.local_branches.find((b) => b.worktree?.path === worktreePath);
    if (branch) {
      return {
        repoRoot: listing.repo.root,
        branch,
        defaultBranch: listing.repo.default_branch,
        worktreePath,
      };
    }
  }
  return null;
}

async function resolveProjectBranchTarget(
  projectId: string,
  directory: string,
): Promise<ProjectBranchResolveResult> {
  const listing = await loadProjectRepo(directory);
  let target = projectBranchTarget(projectId);
  if (target !== null) return { kind: "resolved", target };

  if (listing?.repo.available) return { kind: "unresolved" };

  try {
    await addRepo(directory);
  } catch (e) {
    console.warn("[switchboard] git view reveal addRepo failed", { directory, error: e });
    return { kind: "failed", message: e instanceof Error ? e.message : String(e) };
  }
  target = projectBranchTarget(projectId);
  return target === null ? { kind: "unresolved" } : { kind: "resolved", target };
}

async function selectProjectBranchTarget(target: ProjectBranchTarget): Promise<void> {
  snapshotRepoSort();
  view.mode = "git";
  requestRepoReveal(target.repoRoot);
  const ref: SelectedRef = { repoRoot: target.repoRoot, kind: "local", name: target.branch.name };
  if (!refsEqual(branchSelection.current, ref)) {
    await selectBranch(ref, {
      worktreePath: target.worktreePath,
      hasChanges:
        target.branch.worktree?.dirty === true || target.branch.worktree?.untracked === true,
      worktreeSubtitle: target.worktreePath,
    });
  }
}

/// Switch to Git view and select the local branch/worktree linked to a project.
/// If the project directory is inside a git repo that is not tracked yet, the
/// repo is added to Git view and then resolved. Returns `unresolved` when no
/// resolvable linked branch exists (for example a non-git directory, a detached
/// worktree, or a project in a subfolder rather than the worktree root).
export async function revealProjectBranch(
  projectId: string,
  directory: string,
): Promise<RevealProjectBranchResult> {
  const seq = ++gitRevealSeq;
  const resolved = await resolveProjectBranchTarget(projectId, directory);
  if (seq !== gitRevealSeq) return { kind: "superseded" };
  if (resolved.kind !== "resolved") return resolved;

  if (gitView.status !== "complete") {
    try {
      await loadTrackedRepos();
    } catch (e) {
      console.warn("[switchboard] git view reveal loadTrackedRepos failed", { error: e });
      return { kind: "failed", message: e instanceof Error ? e.message : String(e) };
    }
    if (seq !== gitRevealSeq) return { kind: "superseded" };
  }

  const target = projectBranchTarget(projectId);
  if (target === null) return { kind: "unresolved" };
  await selectProjectBranchTarget(target);
  return seq === gitRevealSeq ? { kind: "revealed" } : { kind: "superseded" };
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
  for (const root of [...collapsedRepoRoots]) {
    if (!live.has(root)) collapsedRepoRoots.delete(root);
  }
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
      branchComparison.worktreePath =
        sel.kind === "local"
          ? (listing.repo.local_branches.find((branch) => branch.name === sel.name)?.worktree
              ?.path ?? null)
          : null;
      void reloadSelectedCommits();
      void reloadSelectedComparison();
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
    collapsedRepoRoots.clear();
    repoListScroll.top = 0;
    repoSort.mode = "recent";
    repoSort.roots = [];
    repoReveal.root = null;
    gitView.repos = [];
    gitView.status = "pending";
    runtime.clear();
    inFlightFetch.clear();
    gitRevealSeq = 0;
    for (const k of Object.keys(fetchStates)) delete fetchStates[k];
    branchSelection.current = null;
    branchSelectionEpoch += 1;
    branchCommitsRequestId += 1;
    branchComparisonRequestId += 1;
    branchCommits.ref = null;
    branchCommits.ranges = [];
    branchCommits.status = "loaded";
    branchComparison.ref = null;
    branchComparison.status = "loaded";
    branchComparison.result = null;
    branchComparison.base = null;
    branchComparison.pendingBase = undefined;
    branchComparison.error = null;
    branchComparison.worktreePath = null;
    diffTarget.current = null;
    navFocus.pane = null;
    hoverSuppressed.value = false;
    openBranchMenuRoots.clear();
    gitRefresh.revision = 0;
  },
  runtimeSize(): number {
    return runtime.size;
  },
};
