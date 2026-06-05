// Pure mapping from the M1 read-model (branch/worktree status) to the at-a-glance
// badges the Git view renders. Kept separate from the Svelte components so the
// D2 mapping is unit-testable on its own and reused wherever git state is shown
// (the Git view now; the project-scoped panel in M6).
//
// The mapping is deliberately calm and one-tier (decision D2): every attention
// state (changes, behind-default, upstream gone,
// orphaned, prunable) is one amber `warning`; ahead/behind/diverged counts are
// neutral informational chips; local-only, upstream presence, and merged are
// muted labels. No success/green token is introduced.

import type { BranchView, RemoteBranchView, SyncState, WorktreeView } from "$lib/types";

/// How a badge should read visually. `warning` → amber attention token;
/// `neutral` → a plain count/info chip; `muted` → low-emphasis fact label.
export type BadgeTone = "warning" | "neutral" | "muted";

export type GitBadge = {
  /// Short text shown in the badge (e.g. "changes", "↑3", "merged").
  label: string;
  tone: BadgeTone;
  /// Stable key for `{#each}` and test lookup.
  key: string;
  /// Longer hover description; the label alone is terse by design.
  title: string;
};

/// The sync-vs-own-upstream signal. Counts are neutral chips (informational, not
/// attention); `local_only` is a muted fact; `in_sync`/`unknown` show nothing.
function syncBadges(sync: SyncState): GitBadge[] {
  switch (sync.kind) {
    case "ahead":
      return [
        {
          key: "ahead",
          label: `↑${sync.commits}`,
          tone: "neutral",
          title: `${sync.commits} unpushed commit(s)`,
        },
      ];
    case "behind":
      return [
        {
          key: "behind",
          label: `↓${sync.commits}`,
          tone: "neutral",
          title: `${sync.commits} commit(s) behind upstream`,
        },
      ];
    case "diverged":
      return [
        {
          key: "diverged",
          label: `↑${sync.ahead} ↓${sync.behind}`,
          tone: "neutral",
          title: `Diverged from upstream: ${sync.ahead} ahead, ${sync.behind} behind`,
        },
      ];
    case "local_only":
      return [
        { key: "local_only", label: "local", tone: "muted", title: "No upstream — not pushed" },
      ];
    case "in_sync":
    case "unknown":
      return [];
  }
}

function remoteName(upstream: string): string {
  return upstream.split("/", 1)[0] ?? upstream;
}

/// Worktree-level signal: any uncommitted change (dirty OR untracked) collapses
/// to one amber "changes" badge; the staged/unstaged/untracked split is only
/// surfaced in the M5 diff panel, not the tree. Warnings are amber too.
function worktreeBadges(wt: WorktreeView): GitBadge[] {
  const badges: GitBadge[] = [];
  if (wt.dirty || wt.untracked) {
    badges.push({
      key: "uncommitted",
      label: "changes",
      tone: "warning",
      title: "Uncommitted changes in this folder",
    });
  }
  if (wt.warning === "orphaned") {
    badges.push({
      key: "orphaned",
      label: "orphaned",
      tone: "warning",
      title: "The branch this worktree was on was deleted",
    });
  } else if (wt.warning === "prunable") {
    badges.push({
      key: "prunable",
      label: "prunable",
      tone: "warning",
      title: "This worktree's directory is gone; the record can be pruned",
    });
  }
  return badges;
}

/// All badges for a local branch, in render order: worktree state first (most
/// actionable), then behind-base, dangling, sync, merged.
///
/// `defaultBranch` is the repo's resolved default (e.g. `main`). The `merged`
/// cleanup badge ("safe to delete") is suppressed for the default branch itself
/// — it's trivially an ancestor of its own tip, so M1 reports it as merged, but
/// labeling `main` "safe to delete" is exactly wrong.
export function localBranchBadges(branch: BranchView, defaultBranch: string | null): GitBadge[] {
  const badges: GitBadge[] = [];
  if (branch.worktree) {
    badges.push(...worktreeBadges(branch.worktree));
  }
  if (branch.behind_base != null && branch.behind_base > 0) {
    badges.push({
      key: "behind_base",
      label: `behind ${defaultBranch ?? "default"}`,
      tone: "warning",
      title: `${branch.behind_base} commit(s) behind the default branch`,
    });
  }
  if (branch.dangling) {
    badges.push({
      key: "dangling",
      label: "upstream gone",
      tone: "warning",
      title: "Upstream branch was deleted on the remote",
    });
  }
  if (branch.upstream != null && branch.sync.kind === "in_sync") {
    badges.push({
      key: "upstream",
      label: `on ${remoteName(branch.upstream)}`,
      tone: "muted",
      title: `Tracks ${branch.upstream}`,
    });
  }
  badges.push(...syncBadges(branch.sync));
  if (branch.merged === true && branch.name !== defaultBranch) {
    badges.push(mergedBadge());
  }
  return badges;
}

/// Badges for a remote-tracking branch — only the two cleanup signals it carries
/// (decision 10a): behind-base (amber) and merged (muted). `merged` is suppressed
/// for the default branch's own remote ref (`origin/<default>`), same rationale
/// as the local default.
export function remoteBranchBadges(
  branch: RemoteBranchView,
  defaultBranch: string | null,
): GitBadge[] {
  const badges: GitBadge[] = [];
  if (branch.behind_base != null && branch.behind_base > 0) {
    badges.push({
      key: "behind_base",
      label: `behind ${defaultBranch ?? "default"}`,
      tone: "warning",
      title: `${branch.behind_base} commit(s) behind the default branch`,
    });
  }
  const isDefaultRemote = defaultBranch != null && branch.name === `origin/${defaultBranch}`;
  if (branch.merged === true && !isDefaultRemote) {
    badges.push(mergedBadge());
  }
  return badges;
}

function mergedBadge(): GitBadge {
  return {
    key: "merged",
    label: "merged",
    tone: "muted",
    title: "Merged into the default branch — safe to delete",
  };
}

/// Tailwind classes for a tone. `warning` reuses the amber attention token;
/// `neutral`/`muted` lean on the existing panel/muted tokens — no new hue.
export function badgeToneClass(tone: BadgeTone): string {
  switch (tone) {
    case "warning":
      return "bg-warning-soft text-warning";
    case "neutral":
      return "bg-panel text-fg";
    case "muted":
      return "bg-panel text-muted";
  }
}
