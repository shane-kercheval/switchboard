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
  import DiffPanel from "$lib/components/DiffPanel.svelte";
  import {
    SEGMENTED_MAIN_CONTAINER_CLASS,
    SEGMENTED_MAIN_ITEM_ACTIVE_CLASS,
    SEGMENTED_MAIN_ITEM_CLASS,
    SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
  } from "$lib/components/ui/segmentedControl";
  import {
    gitView,
    fetchStates,
    refreshAll,
    fetchAll,
    addRepo,
    diffTarget,
    clearBranchSelection,
    gitRefresh,
  } from "$lib/state/gitView.svelte";
  import { pickDirectory } from "$lib/native";

  let branchFilter = $state<"local" | "remote" | "both">("both");
  let showInactive = $state(false);
  let refreshing = $state(false);
  let adding = $state(false);
  let addError = $state<string | null>(null);

  // The open diff target (a commit or a worktree's uncommitted changes), or null.
  // Drives the right inspector.
  const panel = $derived(diffTarget.current);
  let splitEl = $state<HTMLDivElement | null>(null);
  let detailWidth = $state<number | null>(null);
  let resizingDetail = false;

  function startDetailResize(event: PointerEvent): void {
    resizingDetail = true;
    event.preventDefault();
  }

  function resizeDetail(event: PointerEvent): void {
    if (!resizingDetail || splitEl === null) return;
    const rect = splitEl.getBoundingClientRect();
    const max = rect.width * 0.85;
    const width = rect.right - event.clientX;
    detailWidth = Math.min(max, Math.max(360, width));
  }

  function endDetailResize(): void {
    resizingDetail = false;
  }

  const filterOptions: { value: "local" | "remote" | "both"; label: string }[] = [
    { value: "both", label: "Both" },
    { value: "local", label: "Local" },
    { value: "remote", label: "Remote" },
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
  <div class="border-border/60 bg-surface flex min-h-11 items-center gap-3 border-b px-4 py-2">
    <div class="min-w-0">
      <div class="text-fg text-sm leading-5 font-semibold">Repositories</div>
      <div class="text-muted text-[11px] leading-4">
        {gitView.repos.length} tracked
      </div>
    </div>

    <div class="flex min-w-0 flex-1 items-center gap-2">
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

      <label
        class="text-muted hover:bg-panel flex h-6 cursor-pointer items-center gap-1.5 rounded-full px-2 text-xs transition-colors"
      >
        <input
          class="accent-accent h-3.5 w-3.5"
          type="checkbox"
          bind:checked={showInactive}
          data-testid="show-inactive"
        />
        Show branches without folders
      </label>
    </div>

    <div class="flex shrink-0 items-center gap-2">
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
    <div
      class="border-border/60 bg-status-failed-soft text-status-failed border-b px-4 py-2 text-xs"
      data-testid="git-add-error"
    >
      {addError}
    </div>
  {/if}

  <div class="flex min-h-0 flex-1 overflow-hidden" bind:this={splitEl} data-testid="git-split">
    <div
      class="git-scrollbar bg-surface flex min-h-0 min-w-0 flex-1 [scrollbar-gutter:stable] flex-col gap-2 overflow-y-scroll p-3"
      data-testid="git-repo-list"
    >
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

    <div
      class="border-border/60 bg-panel hover:bg-raised w-1.5 shrink-0 cursor-col-resize border-x transition-colors"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize diff panel"
      data-testid="git-detail-resizer"
      onpointerdown={startDetailResize}
    ></div>
    <aside
      class={cn(
        "border-border/60 bg-raised flex min-h-0 shrink-0 flex-col border-l",
        detailWidth === null && "w-2/3",
      )}
      style={detailWidth !== null ? `width: ${detailWidth}px` : undefined}
      data-testid="git-detail-sidebar"
    >
      {#if panel !== null}
        <DiffPanel
          target={panel}
          refreshRevision={gitRefresh.revision}
          onClose={clearBranchSelection}
        />
      {:else}
        <EmptyState
          testid="git-detail-empty"
          title="Select a commit"
          description="Choose a branch, commit, or uncommitted changes to inspect the diff."
        />
      {/if}
    </aside>
  </div>
</div>

<svelte:window onpointermove={resizeDetail} onpointerup={endDetailResize} />
