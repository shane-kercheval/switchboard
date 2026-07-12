<script lang="ts">
  /// The Git view's center-pane body: the repos→branches tree with a controls row
  /// (global refresh, local/remote filter, show-inactive toggle). The view toggle
  /// itself lives in the title bar (App.svelte); this is everything below it.
  ///
  /// No polling — `enterGitView` (called by App on toggle) runs the staleness-
  /// gated entry refresh; the global refresh button forces a re-read + fetch.
  import { untrack } from "svelte";
  import { cn } from "$lib/utils";
  import Button from "$lib/components/ui/Button.svelte";
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import ResizeHandle from "$lib/components/ui/ResizeHandle.svelte";
  import { GIT_DETAIL_MIN_WIDTH, layout } from "$lib/layout.svelte";
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
    branchSelection,
    clearBranchSelection,
    selectedWorktreePathForEditor,
    removeRepo,
    gitRefresh,
    hoverSuppressed,
  } from "$lib/state/gitView.svelte";
  import {
    setCommandSource,
    clearCommandSource,
    palette,
    type Command,
  } from "$lib/state/commandPalette.svelte";
  import * as api from "$lib/api";
  import { pickDirectory, copyText } from "$lib/native";
  import { isEditableShortcutTarget } from "$lib/keyboard";

  let branchFilter = $state<"local" | "remote" | "both">("both");
  let showInactive = $state(false);
  let refreshing = $state(false);
  let adding = $state(false);
  let addError = $state<string | null>(null);
  // Visible failure surface for palette-triggered repo/worktree actions, so they
  // report errors the way the row action menu (`GitRepoNode`) and the Projects
  // palette already do — never a silent `console.warn`.
  let commandError = $state<string | null>(null);

  // The open diff target (a commit or a worktree's uncommitted changes), or null.
  // Drives the right inspector.
  const panel = $derived(diffTarget.current);
  let splitEl = $state<HTMLDivElement | null>(null);
  let detailEl = $state<HTMLElement | null>(null);
  /// Live width during a resize drag; the layout store commits on pointer-up.
  /// `layout.gitDetailWidth` stays null until the first drag, keeping the CSS
  /// default (2/3 of the split) — double-click reset returns to it.
  let draftDetailWidth = $state<number | null>(null);
  const detailWidth = $derived(draftDetailWidth ?? layout.gitDetailWidth);
  let detailExpanded = $state(false);

  function detailStartWidth(): number {
    return detailWidth ?? detailEl?.getBoundingClientRect().width ?? GIT_DETAIL_MIN_WIDTH;
  }

  function detailMaxWidth(): number {
    return splitEl === null
      ? Number.POSITIVE_INFINITY
      : splitEl.getBoundingClientRect().width * 0.85;
  }

  function onWindowPointerMove(): void {
    if (hoverSuppressed.value) hoverSuppressed.value = false;
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (palette.open) return;
    if (isEditableShortcutTarget(event.target)) return;
    const command = event.metaKey || event.ctrlKey;
    if (!command || event.altKey) return;
    const key = event.key.toLowerCase();
    if (key === "d" && event.shiftKey) {
      if (panel === null) return;
      event.preventDefault();
      toggleDetailExpanded();
    } else if (key === "n" && !event.shiftKey) {
      // Contextual ⌘N: in the Git view it adds a repo (App handles Add project
      // for the other views).
      event.preventDefault();
      void onAddRepo();
    } else if (key === "r" && !event.shiftKey) {
      // ⌘R refreshes every tracked repo. preventDefault suppresses the webview's
      // default reload accelerator.
      event.preventDefault();
      void onGlobalRefresh();
    }
  }

  function toggleDetailExpanded(): void {
    // Expanding mid-drag unmounts the resize handle (ending the drag); drop
    // any uncommitted draft so it can't pin the width after un-expanding.
    draftDetailWidth = null;
    detailExpanded = !detailExpanded;
  }

  $effect(() => {
    if (panel === null) {
      detailExpanded = false;
    }
  });

  $effect(() => {
    window.addEventListener("keydown", handleKeydown);
    return () => window.removeEventListener("keydown", handleKeydown);
  });

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

  // Run a palette-triggered action that can fail, surfacing any rejection in the
  // visible `commandError` banner (same posture as the row action menu) rather
  // than swallowing it. `null` path/selection is a no-op (the command is also
  // `disabled` in that state).
  function runPaletteAction(action: () => Promise<void>): void {
    commandError = null;
    void action().catch((e) => {
      commandError = e instanceof Error ? e.message : String(e);
    });
  }

  function openWorktree(action: (path: string) => Promise<void>, path: string | null): void {
    if (path === null) return;
    runPaletteAction(() => action(path));
  }

  // Contribute the Git view's commands to the palette while this view is
  // mounted. Re-runs whenever the state the commands close over (filters,
  // selection, open diff) changes, so `disabled`/title stay current; the
  // separate no-dep effect clears the source on unmount.
  $effect(() => {
    const worktreePath = selectedWorktreePathForEditor();
    const branchName = branchSelection.current?.name ?? null;
    const repoRoot = branchSelection.current?.repoRoot ?? null;
    const cmds: Command[] = [
      {
        id: "git.add-repo",
        title: "Add repository",
        group: "Git",
        shortcut: ["mod", "N"],
        keywords: "track repo",
        run: () => void onAddRepo(),
      },
      {
        id: "git.refresh-all",
        title: "Refresh all repositories",
        group: "Git",
        shortcut: ["mod", "R"],
        keywords: "fetch reload",
        run: () => void onGlobalRefresh(),
      },
      {
        id: "git.filter-both",
        title: "Branch filter: Both",
        group: "Git",
        disabled: branchFilter === "both",
        run: () => {
          branchFilter = "both";
        },
      },
      {
        id: "git.filter-local",
        title: "Branch filter: Local",
        group: "Git",
        disabled: branchFilter === "local",
        run: () => {
          branchFilter = "local";
        },
      },
      {
        id: "git.filter-remote",
        title: "Branch filter: Remote",
        group: "Git",
        disabled: branchFilter === "remote",
        run: () => {
          branchFilter = "remote";
        },
      },
      {
        id: "git.toggle-inactive",
        title: showInactive ? "Hide branches without folders" : "Show branches without folders",
        group: "Git",
        run: () => {
          showInactive = !showInactive;
        },
      },
      {
        id: "git.toggle-detail",
        title: detailExpanded ? "Collapse diff panel" : "Expand diff panel",
        group: "Git",
        shortcut: ["mod", "shift", "D"],
        disabled: panel === null,
        run: () => toggleDetailExpanded(),
      },
      {
        id: "git.open-editor",
        title: "Open selected worktree in editor",
        group: "Git",
        shortcut: ["mod", "shift", "E"],
        disabled: worktreePath === null,
        run: () => openWorktree(api.openInEditor, worktreePath),
      },
      {
        id: "git.open-terminal",
        title: "Open selected worktree in terminal",
        group: "Git",
        disabled: worktreePath === null,
        run: () => openWorktree(api.openInTerminal, worktreePath),
      },
      {
        id: "git.reveal-finder",
        title: "Reveal selected worktree in Finder",
        group: "Git",
        disabled: worktreePath === null,
        run: () => openWorktree(api.revealInFinder, worktreePath),
      },
      {
        id: "git.copy-branch",
        title: "Copy selected branch name",
        group: "Git",
        disabled: branchName === null,
        run: () => {
          if (branchName !== null) runPaletteAction(() => copyText(branchName));
        },
      },
      {
        id: "git.remove-repo",
        title: "Remove selected repository from view",
        group: "Git",
        keywords: "untrack",
        disabled: repoRoot === null,
        run: () => {
          if (repoRoot !== null) runPaletteAction(() => removeRepo(repoRoot));
        },
      },
    ];
    // `setCommandSource` reads the registry to find/replace this source's slot;
    // untrack it so this effect doesn't take a dependency on the state it writes
    // (which would be a read-and-write-same-state loop).
    untrack(() => setCommandSource("git", cmds));
  });

  $effect(() => () => clearCommandSource("git"));
</script>

<div class="flex min-h-0 flex-1 flex-col overflow-hidden" data-testid="git-view">
  {#if !detailExpanded}
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
  {/if}

  {#if commandError}
    <div
      class="border-border/60 bg-status-failed-soft text-status-failed flex items-start justify-between gap-3 border-b px-4 py-2 text-xs"
      data-testid="git-command-error"
    >
      <span>{commandError}</span>
      <button
        type="button"
        class="text-status-failed shrink-0 underline"
        onclick={() => (commandError = null)}
      >
        Dismiss
      </button>
    </div>
  {/if}

  <div class="flex min-h-0 flex-1 overflow-hidden" bind:this={splitEl} data-testid="git-split">
    {#if !detailExpanded}
      <div
        class="git-scrollbar bg-raised flex min-h-0 min-w-0 flex-1 [scrollbar-gutter:stable] flex-col gap-1 overflow-y-scroll p-2"
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

      <ResizeHandle
        value={detailStartWidth}
        min={GIT_DETAIL_MIN_WIDTH}
        max={detailMaxWidth}
        edge="start"
        label="Resize diff panel"
        testid="git-detail-resizer"
        class="border-border/60 bg-panel hover:bg-focus w-1.5 border-x transition-colors"
        onDraft={(px) => (draftDetailWidth = px)}
        onCommit={(px) => {
          layout.gitDetailWidth = px;
          draftDetailWidth = null;
        }}
        onReset={() => {
          layout.gitDetailWidth = null;
          draftDetailWidth = null;
        }}
      />
    {/if}
    <aside
      bind:this={detailEl}
      class={cn(
        "border-border/60 bg-raised flex min-h-0 flex-col border-l",
        // The live max-width mirrors detailMaxWidth() in CSS (85% of the split
        // container — the *actual* container, tighter than the store's
        // viewport-based read clamp), so a persisted width is capped the
        // moment the container shrinks. Never applied while expanded: that
        // state is flex-1 and must fill the row.
        detailExpanded ? "min-w-0 flex-1 border-l-0" : "max-w-[85%] shrink-0",
        !detailExpanded && detailWidth === null && "w-2/3",
      )}
      style={!detailExpanded && detailWidth !== null ? `width: ${detailWidth}px` : undefined}
      data-testid="git-detail-sidebar"
      data-expanded={detailExpanded}
    >
      {#if panel !== null}
        <DiffPanel
          target={panel}
          refreshRevision={gitRefresh.revision}
          onClose={clearBranchSelection}
          {detailExpanded}
          onToggleDetailExpanded={toggleDetailExpanded}
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

<!-- Keyboard navigation suppresses hover highlights; any pointer motion restores them. -->
<svelte:window onpointermove={onWindowPointerMove} />
