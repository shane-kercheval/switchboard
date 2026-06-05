<script lang="ts">
  /// One tracked repo in the Git view: a collapsible header (repo name/path,
  /// availability, per-repo refresh + last-fetched indicator) over its branches.
  /// Branch filtering is applied by the parent and passed in, so this node is
  /// pure presentation over the data.
  import { onMount } from "svelte";
  import { homeDir } from "@tauri-apps/api/path";
  import { cn, basename } from "$lib/utils";
  import { formatHomePath } from "$lib/utils";
  import Badge from "$lib/components/ui/Badge.svelte";
  import GitBadge from "$lib/components/GitBadge.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";
  import { localBranchBadges, remoteBranchBadges, remoteOnlyBadge } from "$lib/gitBadges";
  import type { BranchView, RepoListing, WorktreeView } from "$lib/types";
  import type { FetchState } from "$lib/state/gitView.svelte";
  import {
    refreshRepo,
    fetchRepo,
    removeRepo,
    selectWorktree,
    worktreeSelection,
  } from "$lib/state/gitView.svelte";
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
  let homePath = $state<string | null>(null);

  const repo = $derived(listing.repo);
  const localBranchCount = $derived(repo.local_branches.length);

  // Branches with a folder are actionable and sort first. Branches without a
  // local folder are hidden until the user asks for them.
  const localBranches = $derived(
    [...repo.local_branches]
      .filter((b) =>
        branchFilter === "remote" ? b.upstream !== null : showInactive || b.worktree !== null,
      )
      .sort((a, b) => {
        const aActive = a.worktree !== null ? 0 : 1;
        const bActive = b.worktree !== null ? 0 : 1;
        return aActive - bActive || a.name.localeCompare(b.name);
      }),
  );
  const remoteOnlyBranches = $derived(
    repo.remote_branches.filter(
      (remote) => !repo.local_branches.some((local) => local.upstream === remote.name),
    ),
  );
  const visibleRemoteOnlyBranches = $derived(branchFilter === "local" ? [] : remoteOnlyBranches);

  onMount(() => {
    void homeDir()
      .then((path) => {
        homePath = path;
      })
      .catch(() => {
        homePath = null;
      });
  });

  function displayPath(path: string): string {
    return formatHomePath(path, homePath);
  }

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

<div
  class="border-border/70 bg-raised overflow-hidden rounded-md border"
  data-testid="git-repo"
  data-repo-root={repo.root}
>
  <div class="bg-raised flex min-h-10 items-center gap-2 px-3 py-2">
    <button
      type="button"
      class="text-muted hover:bg-panel hover:text-fg flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-full transition-colors"
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

    <div class="min-w-0 flex-1">
      <div class="flex min-w-0 items-baseline gap-1.5">
        <span class="text-fg truncate text-sm font-semibold" title={repo.root}>{repo.name}</span>
        <span class="text-border shrink-0 text-[11px]">/</span>
        <span class="text-muted truncate font-mono text-[11px] leading-4" title={repo.root}>
          {displayPath(repo.root)}
        </span>
        {#if repo.is_bare}
          <Badge testid="repo-bare" class="shrink-0">bare</Badge>
        {/if}
        {#if !repo.available}
          <Badge testid="repo-unavailable" class="bg-warning-soft text-warning shrink-0">
            unavailable
          </Badge>
        {/if}
      </div>
    </div>

    <div class="flex shrink-0 items-center gap-2">
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
    <div class="border-border/60 bg-surface border-t px-2 py-1.5" data-testid="repo-branches">
      {#if !repo.available}
        <p class="text-muted px-2 py-2 text-xs">
          This repository is unavailable (moved or unmounted).
        </p>
      {:else}
        {#each localBranches as branch (branch.name)}
          {@const selected =
            branch.worktree !== null && worktreeSelection.current?.path === branch.worktree.path}
          <div
            class={cn(
              "group flex min-h-8 items-center gap-2 rounded-md px-2 py-1.5 transition-colors",
              branch.worktree === null && "opacity-60",
              branch.worktree !== null && "hover:bg-panel",
              selected && "bg-raised hover:bg-raised",
            )}
            data-testid="git-branch"
            data-branch={branch.name}
          >
            {#if branch.worktree}
              {@const worktreePath = branch.worktree.path}
              <!-- Clicking the row (but not the actions menu) opens the diff
                   panel for this branch's local folder; the menu is a sibling
                   button so the two clicks don't nest. -->
              <button
                type="button"
                class="flex min-w-0 flex-1 items-center gap-2 text-left"
                data-testid="worktree-select"
                data-selected={selected}
                onclick={() => selectWorktree(worktreePath, branch.name)}
              >
                {@render branchInner(branch)}
              </button>
              <DropdownMenu
                triggerLabel={`Actions for ${branch.name}`}
                triggerTestid="worktree-actions-trigger"
                triggerClass={cn(
                  ICON_BUTTON_CLASS,
                  "shrink-0 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 data-[state=open]:opacity-100",
                )}
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
            {:else}
              <!-- No local folder → not openable; render the same content inert. -->
              <div class="flex min-w-0 flex-1 items-center gap-2">
                {@render branchInner(branch)}
              </div>
            {/if}
          </div>
        {/each}

        {#each visibleRemoteOnlyBranches as branch (branch.name)}
          <div
            class="flex min-h-8 items-center gap-2 rounded-md px-2 py-1.5 opacity-65"
            data-testid="git-remote-branch"
            data-branch={branch.name}
          >
            <div class="min-w-0 flex-1">
              <div class="flex min-w-0 items-center gap-1.5">
                <span class="text-muted truncate text-[13px] leading-5" title={branch.name}>
                  {branch.name}
                </span>
                <div class="flex shrink-0 items-center gap-1">
                  <GitBadge badge={remoteOnlyBadge(branch.name)} />
                </div>
              </div>
              <div class="text-muted truncate text-[11px] leading-4">No local folder</div>
            </div>
            <div class="flex shrink-0 items-center gap-1">
              {#each remoteBranchBadges(branch, repo.default_branch) as badge (badge.key)}
                <GitBadge {badge} />
              {/each}
            </div>
          </div>
        {/each}

        {#if localBranches.length === 0 && visibleRemoteOnlyBranches.length === 0}
          <p class="text-muted px-2 py-1.5 text-xs">
            {#if branchFilter === "remote"}
              No remote branches.
            {:else if showInactive && localBranchCount === 0}
              No local branches.
            {:else}
              No branches with local folders.
            {/if}
          </p>
        {/if}

        {#if branchFilter !== "remote"}
          {#each repo.detached_worktrees as wt (wt.path)}
            {@const dselected = worktreeSelection.current?.path === wt.path}
            <div
              class={cn(
                "flex min-h-8 items-center gap-2 rounded-md px-2 py-1.5 transition-colors",
                wt.warning !== "prunable" && "hover:bg-panel",
                dselected && "bg-raised hover:bg-raised",
              )}
              data-testid="git-detached-worktree"
            >
              {#if wt.warning === "prunable"}
                <!-- Directory is gone — nothing to inspect, so not openable. -->
                <div class="flex min-w-0 flex-1 items-center gap-2">
                  {@render detachedInner(wt)}
                </div>
              {:else}
                <button
                  type="button"
                  class="flex min-w-0 flex-1 items-center gap-2 text-left"
                  data-testid="worktree-select"
                  data-selected={dselected}
                  onclick={() => selectWorktree(wt.path, wt.detached_hash ?? "detached")}
                >
                  {@render detachedInner(wt)}
                </button>
              {/if}
            </div>
          {/each}
        {/if}
      {/if}
    </div>
  {/if}
</div>

{#snippet branchInner(branch: BranchView)}
  <div class="min-w-0 flex-1">
    <div class="flex min-w-0 items-center gap-1.5">
      <span class="text-fg truncate text-[13px] leading-5" title={branch.name}>{branch.name}</span>
      <div class="flex shrink-0 items-center gap-1">
        {#each localBranchBadges(branch, repo.default_branch) as badge (badge.key)}
          <GitBadge {badge} />
        {/each}
      </div>
      {#if branch.worktree}
        <div class="flex min-w-0 items-center gap-1">
          {#each linkedProjects(branch.worktree.path) as project (project.id)}
            <Badge testid="linked-project">{project.name}</Badge>
          {/each}
        </div>
      {/if}
    </div>
    {#if branch.worktree}
      <div class="text-muted truncate font-mono text-[11px] leading-4" title={branch.worktree.path}>
        {displayPath(branch.worktree.path)}
      </div>
    {:else}
      <div class="text-muted truncate text-[11px] leading-4">No local folder</div>
    {/if}
  </div>
{/snippet}

{#snippet detachedInner(wt: WorktreeView)}
  <div class="min-w-0 flex-1">
    <div class="flex min-w-0 items-center gap-1.5">
      <span class="text-muted truncate font-mono text-[12px] leading-5">
        {wt.detached_hash ?? "detached"}
      </span>
      <div class="flex shrink-0 items-center gap-1">
        {#if wt.warning === "orphaned"}
          <GitBadge
            badge={{
              key: "orphaned",
              label: "orphaned",
              tone: "warning",
              title: "Orphaned folder",
              description: "The branch this folder was on was deleted.",
            }}
          />
        {:else if wt.warning === "prunable"}
          <GitBadge
            badge={{
              key: "prunable",
              label: "prunable",
              tone: "warning",
              title: "Missing folder",
              description: "This folder path is gone; the git worktree record can be pruned.",
            }}
          />
        {/if}
      </div>
    </div>
    <div class="text-muted truncate font-mono text-[11px] leading-4" title={wt.path}>
      {basename(wt.path)}
    </div>
  </div>
{/snippet}
