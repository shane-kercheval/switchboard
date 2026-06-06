// Pure mapping from the M1 read-model (branch/worktree status) to the
// at-a-glance status indicators the Git view renders. Kept separate from the
// Svelte components so the mapping is unit-testable and reused wherever git
// state is shown (the Git view now; the project-scoped panel in M6).

import type { BranchView, RemoteBranchView, SyncState, WorktreeView } from "$lib/types";

/// How an indicator should read visually. `warning` → attention color;
/// `neutral` → ordinary sync/count info; `muted` → low-emphasis fact.
export type IndicatorTone = "warning" | "neutral" | "muted";

export type GitStatusIndicator = {
  /// Short accessible label for the icon (e.g. "changes", "↑3", "merged").
  label: string;
  tone: IndicatorTone;
  /// Stable key for `{#each}` and test lookup.
  key: string;
  /// Tooltip title; the label alone is terse by design.
  title: string;
  /// Tooltip detail shown under the title.
  description: string;
};

/// The sync-vs-own-upstream signal. Counts are neutral informational indicators;
/// `local_only` is a muted fact; `in_sync`/`unknown` show nothing unless an
/// upstream relation is available.
function syncIndicators(sync: SyncState): GitStatusIndicator[] {
  switch (sync.kind) {
    case "ahead":
      return [
        {
          key: "ahead",
          label: `↑${sync.commits}`,
          tone: "neutral",
          title: "Ahead of upstream",
          description: `${sync.commits} unpushed commit(s).`,
        },
      ];
    case "behind":
      return [
        {
          key: "behind",
          label: `↓${sync.commits}`,
          tone: "neutral",
          title: "Behind upstream",
          description: `${sync.commits} commit(s) behind upstream.`,
        },
      ];
    case "diverged":
      return [
        {
          key: "diverged",
          label: `↑${sync.ahead} ↓${sync.behind}`,
          tone: "neutral",
          title: "Diverged from upstream",
          description: `${sync.ahead} ahead, ${sync.behind} behind.`,
        },
      ];
    case "local_only":
      return [
        {
          key: "local_only",
          label: "local",
          tone: "muted",
          title: "Local only",
          description: "No upstream is configured; this branch has not been pushed.",
        },
      ];
    case "in_sync":
    case "unknown":
      return [];
  }
}

function remoteName(upstream: string): string {
  return upstream.split("/", 1)[0] ?? upstream;
}

function upstreamIndicator(upstream: string): GitStatusIndicator {
  return {
    key: "upstream",
    label: `on ${remoteName(upstream)}`,
    tone: "muted",
    title: "Tracks remote",
    description: `Tracks ${upstream}.`,
  };
}

function branchStateIndicators(branch: BranchView): GitStatusIndicator[] {
  if (branch.dangling) {
    return [
      {
        key: "dangling",
        label: "upstream gone",
        tone: "warning",
        title: "Upstream gone",
        description: "The upstream branch was deleted on the remote.",
      },
    ];
  }
  const sync = syncIndicators(branch.sync);
  if (sync.length > 0) return sync;
  if (branch.upstream != null && branch.sync.kind === "in_sync") {
    return [upstreamIndicator(branch.upstream)];
  }
  return [];
}

/// Worktree-level signal: any uncommitted change (dirty OR untracked) collapses
/// to one "changes" indicator; the staged/unstaged/untracked split is only
/// surfaced in the M5 diff panel, not the tree.
function worktreeIndicators(wt: WorktreeView): GitStatusIndicator[] {
  const indicators: GitStatusIndicator[] = [];
  if (wt.dirty || wt.untracked) {
    indicators.push({
      key: "uncommitted",
      label: "changes",
      tone: "warning",
      title: "Uncommitted changes",
      description: "This folder has modified or new files.",
    });
  }
  if (wt.warning === "orphaned") {
    indicators.push({
      key: "orphaned",
      label: "orphaned",
      tone: "warning",
      title: "Orphaned folder",
      description: "The branch this folder was on was deleted.",
    });
  } else if (wt.warning === "prunable") {
    indicators.push({
      key: "prunable",
      label: "prunable",
      tone: "warning",
      title: "Missing folder",
      description: "This folder path is gone; the git worktree record can be pruned.",
    });
  }
  return indicators;
}

/// All indicators for a local branch, in render order: branch relationship/state
/// first (upstream gone, local-only, tracks remote, ahead/behind/diverged), then
/// worktree state, behind-base, and cleanup state.
export function localBranchIndicators(
  branch: BranchView,
  defaultBranch: string | null,
): GitStatusIndicator[] {
  const indicators: GitStatusIndicator[] = [];
  indicators.push(...branchStateIndicators(branch));
  if (branch.worktree) {
    indicators.push(...worktreeIndicators(branch.worktree));
  }
  if (branch.behind_base != null && branch.behind_base > 0) {
    indicators.push({
      key: "behind_base",
      label: `behind ${defaultBranch ?? "default"}`,
      tone: "warning",
      title: `Behind ${defaultBranch ?? "default"}`,
      description: `${branch.behind_base} commit(s) behind the default branch.`,
    });
  }
  if (branch.merged === true && branch.name !== defaultBranch) {
    indicators.push(mergedIndicator());
  }
  return indicators;
}

/// Indicators for a remote-tracking branch: only the cleanup signals it carries.
export function remoteBranchIndicators(
  branch: RemoteBranchView,
  defaultBranch: string | null,
): GitStatusIndicator[] {
  const indicators: GitStatusIndicator[] = [];
  if (branch.behind_base != null && branch.behind_base > 0) {
    indicators.push({
      key: "behind_base",
      label: `behind ${defaultBranch ?? "default"}`,
      tone: "warning",
      title: `Behind ${defaultBranch ?? "default"}`,
      description: `${branch.behind_base} commit(s) behind the default branch.`,
    });
  }
  const isDefaultRemote = defaultBranch != null && branch.name === `origin/${defaultBranch}`;
  if (branch.merged === true && !isDefaultRemote) {
    indicators.push(mergedIndicator());
  }
  return indicators;
}

function mergedIndicator(): GitStatusIndicator {
  return {
    key: "merged",
    label: "merged",
    tone: "muted",
    title: "Merged",
    description: "Merged into the default branch; safe to delete.",
  };
}

export function remoteOnlyIndicator(name: string): GitStatusIndicator {
  return {
    key: "remote_only",
    label: "remote only",
    tone: "muted",
    title: "Remote only",
    description: `${name} exists on the remote, but there is no local branch or folder.`,
  };
}

/// Tailwind classes for an unframed icon. Warning changes the stroke color;
/// neutral and muted indicators stay quiet.
export function indicatorToneClass(tone: IndicatorTone): string {
  switch (tone) {
    case "warning":
      return "text-warning";
    case "neutral":
      return "text-muted";
    case "muted":
      return "text-muted/80";
  }
}
