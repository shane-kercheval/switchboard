<script lang="ts">
  /// One tracked repo in the Git view: a collapsible header (name, default
  /// branch, availability, per-repo refresh + last-fetched indicator) over its
  /// branches. Branch filtering (local/remote/inactive) is applied by the
  /// parent and passed in, so this node is pure presentation over the data.
  import { cn, basename } from "$lib/utils";
  import Badge from "$lib/components/ui/Badge.svelte";
  import GitBadge from "$lib/components/GitBadge.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";
  import { localBranchBadges, remoteBranchBadges } from "$lib/gitBadges";
  import type { RepoListing } from "$lib/types";
  import type { FetchState } from "$lib/state/gitView.svelte";
  import { refreshRepo, fetchRepo, removeRepo } from "$lib/state/gitView.svelte";
  import { openInEditor, openInTerminal, revealInFinder } from "$lib/api";
  import { copyText } from "$lib/native";

  let {
    listing,
    branchFilter,
    showInactive,
    fetchState,
  }: {
    listing: RepoListing;
    branchFilter: "local" | "remote" | "both";
    showInactive: boolean;
    fetchState: FetchState | undefined;
  } = $props();

  let expanded = $state(true);
  let busy = $state(false);

  const repo = $derived(listing.repo);

  // Active = has a worktree (checked out somewhere). Inactive branches are dimmed
  // and hidden when the toggle is off; active ones sort first.
  const localBranches = $derived(
    [...repo.local_branches]
      .filter((b) => showInactive || b.worktree !== null)
      .sort((a, b) => {
        const aActive = a.worktree !== null ? 0 : 1;
        const bActive = b.worktree !== null ? 0 : 1;
        return aActive - bActive || a.name.localeCompare(b.name);
      }),
  );

  const showLocal = $derived(branchFilter !== "remote");
  const showRemote = $derived(branchFilter !== "local");

  function linkedProjects(worktreePath: string) {
    return listing.linked_projects[worktreePath] ?? [];
  }

  function fetchLabel(state: FetchState | undefined): string {
    if (state === undefined || state.kind === "never") return "not fetched";
    if (state.kind === "failed") return "fetch failed";
    return "fetched";
  }

  async function onRefresh(): Promise<void> {
    busy = true;
    try {
      await refreshRepo(repo.root);
      await fetchRepo(repo.root);
    } finally {
      busy = false;
    }
  }

  // Open actions fire-and-forget: a silently-swallowed failure looks like
  // "nothing happened," so surface it to the console (the path is opened
  // backend-side). Copy actions go straight to the clipboard.
  function runAction(action: Promise<void>): void {
    void action.catch((e: unknown) => {
      console.error("[switchboard] git view open action failed", e);
    });
  }
</script>

<div class="border-border/60 rounded-md border" data-testid="git-repo" data-repo-root={repo.root}>
  <div class="flex items-center gap-2 px-3 py-2">
    <button
      type="button"
      class="text-muted hover:text-fg flex h-5 w-5 shrink-0 items-center justify-center"
      aria-label={expanded ? "Collapse repo" : "Expand repo"}
      aria-expanded={expanded}
      onclick={() => (expanded = !expanded)}
    >
      <svg
        viewBox="0 0 24 24"
        class={cn("h-3.5 w-3.5 transition-transform", expanded && "rotate-90")}
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path d="m9 6 6 6-6 6" />
      </svg>
    </button>

    <span class="text-fg truncate text-sm font-semibold" title={repo.root}>{repo.name}</span>

    {#if repo.default_branch}
      <span class="text-muted shrink-0 text-xs">{repo.default_branch}</span>
    {/if}
    {#if repo.is_bare}
      <Badge testid="repo-bare">bare</Badge>
    {/if}
    {#if !repo.available}
      <Badge testid="repo-unavailable">unavailable</Badge>
    {/if}

    <div class="ml-auto flex shrink-0 items-center gap-2">
      <span class="text-muted text-[10px]" data-testid="repo-fetch-state"
        >{fetchLabel(fetchState)}</span
      >
      <button
        type="button"
        class={cn(ICON_BUTTON_CLASS, "disabled:opacity-50")}
        aria-label="Refresh repo"
        data-testid="repo-refresh"
        disabled={busy}
        onclick={onRefresh}
      >
        {#if busy}
          <Spinner class="h-4 w-4" />
        {:else}
          <svg
            viewBox="0 0 24 24"
            class="h-4 w-4"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M21 12a9 9 0 1 1-2.64-6.36M21 3v6h-6" />
          </svg>
        {/if}
      </button>

      <DropdownMenu
        triggerLabel={`Actions for ${repo.name}`}
        triggerTestid="repo-actions-trigger"
        triggerClass={cn(ICON_BUTTON_CLASS)}
        contentTestid="repo-actions-menu"
      >
        {#snippet trigger()}
          <svg viewBox="0 0 24 24" fill="currentColor" class="h-4 w-4" aria-hidden="true">
            <circle cx="12" cy="5" r="1.6" />
            <circle cx="12" cy="12" r="1.6" />
            <circle cx="12" cy="19" r="1.6" />
          </svg>
        {/snippet}
        <DropdownMenuItem
          onSelect={() => runAction(revealInFinder(repo.root))}
          data-testid="repo-action-reveal"
        >
          Reveal in Finder
        </DropdownMenuItem>
        <DropdownMenuItem
          onSelect={() => runAction(copyText(repo.root))}
          data-testid="repo-action-copy-path"
        >
          Copy path
        </DropdownMenuItem>
        <DropdownMenuItem
          onSelect={() => runAction(removeRepo(repo.root))}
          class="text-status-failed"
          data-testid="repo-action-remove"
        >
          Remove from view
        </DropdownMenuItem>
      </DropdownMenu>
    </div>
  </div>

  {#if expanded}
    <div class="border-border/60 border-t px-2 py-1.5" data-testid="repo-branches">
      {#if !repo.available}
        <p class="text-muted px-2 py-2 text-xs">
          This repository is unavailable (moved or unmounted).
        </p>
      {:else}
        {#if showLocal}
          {#each localBranches as branch (branch.name)}
            <div
              class={cn(
                "flex items-start gap-2 rounded px-2 py-1.5",
                branch.worktree === null && "opacity-60",
              )}
              data-testid="git-branch"
              data-branch={branch.name}
            >
              <span class="text-fg mt-0.5 shrink-0 text-[13px]">{branch.name}</span>
              <div class="flex flex-1 flex-wrap items-center gap-1">
                {#each localBranchBadges(branch, repo.default_branch) as badge (badge.key)}
                  <GitBadge {badge} />
                {/each}
                {#if branch.worktree}
                  <span
                    class="text-muted ml-1 truncate font-mono text-[11px]"
                    title={branch.worktree.path}
                  >
                    {basename(branch.worktree.path)}
                  </span>
                  {#each linkedProjects(branch.worktree.path) as project (project.id)}
                    <Badge testid="linked-project">{project.name}</Badge>
                  {/each}
                {/if}
              </div>
              {#if branch.worktree}
                {@const worktreePath = branch.worktree.path}
                <DropdownMenu
                  triggerLabel={`Actions for ${branch.name}`}
                  triggerTestid="worktree-actions-trigger"
                  triggerClass={cn(ICON_BUTTON_CLASS, "shrink-0")}
                  contentTestid="worktree-actions-menu"
                >
                  {#snippet trigger()}
                    <svg viewBox="0 0 24 24" fill="currentColor" class="h-4 w-4" aria-hidden="true">
                      <circle cx="12" cy="5" r="1.6" />
                      <circle cx="12" cy="12" r="1.6" />
                      <circle cx="12" cy="19" r="1.6" />
                    </svg>
                  {/snippet}
                  <DropdownMenuItem
                    onSelect={() => runAction(openInEditor(worktreePath))}
                    data-testid="worktree-action-editor"
                  >
                    Open in editor
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    onSelect={() => runAction(openInTerminal(worktreePath))}
                    data-testid="worktree-action-terminal"
                  >
                    Open in terminal
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    onSelect={() => runAction(revealInFinder(worktreePath))}
                    data-testid="worktree-action-reveal"
                  >
                    Reveal in Finder
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    onSelect={() => runAction(copyText(worktreePath))}
                    data-testid="worktree-action-copy-path"
                  >
                    Copy path
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    onSelect={() => runAction(copyText(branch.name))}
                    data-testid="worktree-action-copy-branch"
                  >
                    Copy branch name
                  </DropdownMenuItem>
                </DropdownMenu>
              {/if}
            </div>
          {/each}
          {#if localBranches.length === 0}
            <p class="text-muted px-2 py-1.5 text-xs">No active branches.</p>
          {/if}
        {/if}

        {#if showRemote}
          {#each repo.remote_branches as branch (branch.name)}
            <div
              class="flex items-start gap-2 rounded px-2 py-1.5 opacity-60"
              data-testid="git-remote-branch"
              data-branch={branch.name}
            >
              <span class="text-muted mt-0.5 shrink-0 text-[13px]">{branch.name}</span>
              <div class="flex flex-1 flex-wrap items-center gap-1">
                {#each remoteBranchBadges(branch, repo.default_branch) as badge (badge.key)}
                  <GitBadge {badge} />
                {/each}
              </div>
            </div>
          {/each}
        {/if}

        {#each repo.detached_worktrees as wt (wt.path)}
          <div
            class="flex items-start gap-2 rounded px-2 py-1.5"
            data-testid="git-detached-worktree"
          >
            <span class="text-muted mt-0.5 shrink-0 font-mono text-[11px]">
              {wt.detached_hash ?? "detached"}
            </span>
            <div class="flex flex-1 flex-wrap items-center gap-1">
              {#if wt.warning === "orphaned"}
                <GitBadge
                  badge={{
                    key: "orphaned",
                    label: "orphaned",
                    tone: "warning",
                    title: "Branch deleted",
                  }}
                />
              {:else if wt.warning === "prunable"}
                <GitBadge
                  badge={{
                    key: "prunable",
                    label: "prunable",
                    tone: "warning",
                    title: "Directory gone",
                  }}
                />
              {/if}
              <span class="text-muted truncate font-mono text-[11px]" title={wt.path}>
                {basename(wt.path)}
              </span>
            </div>
          </div>
        {/each}
      {/if}
    </div>
  {/if}
</div>
