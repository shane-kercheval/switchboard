<script lang="ts">
  /// The Git view's center-pane body: the repos→branches tree with a controls row
  /// (global refresh, local/remote filter, show-inactive toggle). The view toggle
  /// itself lives in the title bar (App.svelte); this is everything below it.
  ///
  /// No polling — `enterGitView` (called by App on toggle) runs the staleness-
  /// gated entry refresh; the global refresh button forces a re-read + fetch.
  import { cn } from "$lib/utils";
  import Button from "$lib/components/ui/Button.svelte";
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import GitRepoNode from "$lib/components/GitRepoNode.svelte";
  import {
    SEGMENTED_MAIN_CONTAINER_CLASS,
    SEGMENTED_MAIN_ITEM_ACTIVE_CLASS,
    SEGMENTED_MAIN_ITEM_CLASS,
    SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
  } from "$lib/components/ui/segmentedControl";
  import { gitView, fetchStates, refreshAll, fetchAll, addRepo } from "$lib/state/gitView.svelte";
  import { pickDirectory } from "$lib/native";

  let branchFilter = $state<"local" | "remote" | "both">("local");
  let showInactive = $state(false);
  let refreshing = $state(false);
  let adding = $state(false);
  let addError = $state<string | null>(null);

  const filterOptions: { value: "local" | "remote" | "both"; label: string }[] = [
    { value: "local", label: "Local" },
    { value: "remote", label: "Remote" },
    { value: "both", label: "Both" },
  ];

  async function onGlobalRefresh(): Promise<void> {
    refreshing = true;
    try {
      await refreshAll();
      await fetchAll();
    } finally {
      refreshing = false;
    }
  }

  async function onAddRepo(): Promise<void> {
    addError = null;
    const path = await pickDirectory();
    if (path === null) return;
    adding = true;
    try {
      await addRepo(path);
    } catch (e) {
      addError = e instanceof Error ? e.message : String(e);
    } finally {
      adding = false;
    }
  }
</script>

<div class="flex min-h-0 flex-1 flex-col overflow-hidden" data-testid="git-view">
  <div class="border-border/60 flex items-center gap-3 border-b px-4 py-2">
    <div
      class={cn(SEGMENTED_MAIN_CONTAINER_CLASS, "inline-grid grid-cols-3")}
      role="radiogroup"
      aria-label="Branch filter"
    >
      {#each filterOptions as option (option.value)}
        <button
          type="button"
          role="radio"
          class={cn(
            SEGMENTED_MAIN_ITEM_CLASS,
            "px-3",
            branchFilter === option.value
              ? SEGMENTED_MAIN_ITEM_ACTIVE_CLASS
              : SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
          )}
          aria-checked={branchFilter === option.value}
          data-testid={`branch-filter-${option.value}`}
          onclick={() => (branchFilter = option.value)}
        >
          {option.label}
        </button>
      {/each}
    </div>

    <label class="text-muted flex cursor-pointer items-center gap-1.5 text-xs">
      <input type="checkbox" bind:checked={showInactive} data-testid="show-inactive" />
      Show inactive branches
    </label>

    <div class="ml-auto flex items-center gap-2">
      <Button
        variant="secondary"
        size="sm"
        data-testid="git-add-repo"
        disabled={adding}
        onclick={onAddRepo}
      >
        {#if adding}
          <Spinner class="mr-1.5 h-3.5 w-3.5" />
        {/if}
        Add Repo
      </Button>

      <Button
        variant="secondary"
        size="sm"
        data-testid="git-refresh-all"
        disabled={refreshing}
        onclick={onGlobalRefresh}
      >
        {#if refreshing}
          <Spinner class="mr-1.5 h-3.5 w-3.5" />
        {/if}
        Refresh
      </Button>
    </div>
  </div>

  {#if addError}
    <p
      class="text-status-failed border-border/60 border-b px-4 py-2 text-xs"
      data-testid="git-add-error"
    >
      {addError}
    </p>
  {/if}

  <div class="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto p-3" data-testid="git-repo-list">
    {#if gitView.status === "loading" && gitView.repos.length === 0}
      <EmptyState testid="git-loading" title="Loading repositories…" />
    {:else if gitView.status === "failed" && gitView.repos.length === 0}
      <EmptyState
        testid="git-failed"
        tone="error"
        title="Couldn't load repositories."
        description="Try refreshing."
      >
        {#snippet action()}
          <Button variant="secondary" size="sm" onclick={onGlobalRefresh}>Retry</Button>
        {/snippet}
      </EmptyState>
    {:else if gitView.repos.length === 0}
      <EmptyState
        testid="git-empty"
        title="No repositories tracked yet."
        description="Repositories are tracked automatically when you add a project that lives in a git repo, or add one with Add Repo."
      >
        {#snippet action()}
          <Button variant="secondary" size="sm" disabled={adding} onclick={onAddRepo}>
            Add Repo
          </Button>
        {/snippet}
      </EmptyState>
    {:else}
      {#each gitView.repos as listing (listing.repo.root)}
        <GitRepoNode
          {listing}
          {branchFilter}
          {showInactive}
          fetchState={fetchStates[listing.repo.root]}
        />
      {/each}
    {/if}
  </div>
</div>
