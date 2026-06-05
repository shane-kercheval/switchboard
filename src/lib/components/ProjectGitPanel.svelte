<script lang="ts">
  /// Compact git-status block at the top of the right sidebar, scoped to the
  /// active project's worktree: branch name + the same status icons the Git view
  /// shows (uncommitted, sync, behind-base/"out of date", merged, dangling). It's
  /// a filtered slice of the same `RepoView` data — no new data path. The
  /// behind-base "out of date" signal is emitted by `localBranchIndicators`
  /// already emits, so the two surfaces can't drift.
  ///
  /// Renders nothing until the project's branch resolves, and nothing at all when
  /// the project isn't in a tracked git repo (or its worktree is detached) — a
  /// calm degrade, not an error.
  import GitStatusIcon from "$lib/components/GitStatusIcon.svelte";
  import { localBranchIndicators } from "$lib/gitStatusIndicators";
  import { loadProjectRepo, projectBranch } from "$lib/state/gitView.svelte";
  import type { ProjectListing } from "$lib/types";

  let { project }: { project: ProjectListing } = $props();

  // Load (+ staleness-gated fetch) the project's repo when the active project
  // changes; the result lands in the shared Git-view cache that `projectBranch`
  // reads, so this derives reactively as the read/fetch resolve.
  $effect(() => {
    void loadProjectRepo(project.directory);
  });

  const status = $derived(projectBranch(project.id));
  const indicators = $derived(
    status ? localBranchIndicators(status.branch, status.defaultBranch) : [],
  );
</script>

{#if status}
  <div class="border-border/60 shrink-0 border-b px-3 py-2" data-testid="project-git-panel">
    <div class="bg-panel/70 rounded-md px-2 py-1.5">
      <div class="flex min-w-0 items-center gap-2">
        <div
          class="text-muted shrink-0 text-[11px] leading-none font-semibold tracking-wide uppercase"
        >
          Git
        </div>
        <div
          class="text-fg min-w-0 flex-1 truncate text-[13px] leading-5 font-medium"
          data-testid="project-git-branch"
          title={status.branch.name}
        >
          {status.branch.name}
        </div>
        {#if indicators.length > 0}
          <div class="flex shrink-0 items-center gap-1" data-testid="project-git-badges">
            {#each indicators as indicator (indicator.key)}
              <GitStatusIcon {indicator} />
            {/each}
          </div>
        {/if}
      </div>
      {#if indicators.length === 0}
        <div class="text-muted mt-0.5 text-xs" data-testid="project-git-clean">
          No changes · up to date
        </div>
      {/if}
    </div>
  </div>
{/if}
