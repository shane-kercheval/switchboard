<script lang="ts">
  /// The right-hand diff panel: the changed files + read-only diff for the
  /// selected [`DiffTarget`] — either a worktree's uncommitted changes or one
  /// commit's diff (vs. its first parent). One panel serves both because a commit
  /// diff needs no worktree, so a branch with no local folder (or a remote-only
  /// ref) still shows real content. Loads are guarded against target/file races
  /// (a newer selection's result always wins).
  import { untrack } from "svelte";
  import { ExternalLink, Maximize2, Minimize2 } from "@lucide/svelte";
  import { cn, basename } from "$lib/utils";
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import DiffView from "$lib/components/DiffView.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";
  import {
    SEGMENTED_MAIN_CONTAINER_CLASS,
    SEGMENTED_MAIN_ITEM_CLASS,
    SEGMENTED_MAIN_ITEM_ACTIVE_CLASS,
    SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
  } from "$lib/components/ui/segmentedControl";
  import {
    changedFiles,
    fileDiff,
    commitChangedFiles,
    commitFileDiff,
    openCommitFileDifftool,
    openWorktreeFileDifftool,
    revealInFinder,
  } from "$lib/api";
  import { languageForPath } from "$lib/diff";
  import { shortcut } from "$lib/platform";
  import { preferences, updatePreferences } from "$lib/preferences.svelte";
  import type { ChangedFile, ChangeKind, DiffStyle, FileDiff } from "$lib/types";
  import type { DiffTarget } from "$lib/state/gitView.svelte";

  let {
    target,
    refreshRevision = 0,
    onClose,
    detailExpanded = false,
    onToggleDetailExpanded,
  }: {
    target: DiffTarget;
    refreshRevision?: number;
    onClose: () => void;
    detailExpanded?: boolean;
    onToggleDetailExpanded?: () => void;
  } = $props();

  let files = $state<ChangedFile[] | null>(null);
  let filesError = $state<string | null>(null);
  let selectedFile = $state<string | null>(null);
  // For a commit target, whether the commit still resolved. `false` (gc'd /
  // force-updated) is shown distinctly from a commit that changed nothing.
  let commitFound = $state(true);

  let diff = $state<FileDiff | null>(null);
  let diffError = $state<string | null>(null);
  let externalActionError = $state<string | null>(null);
  let diffLoading = $state(false);

  // Monotonic tokens: a stale async result (the target/file changed before it
  // resolved) is discarded rather than clobbering the current selection.
  let filesToken = 0;
  let diffToken = 0;
  let filesKey: string | null = null;
  let diffKey: string | null = null;
  let bodyEl = $state<HTMLDivElement | null>(null);
  let fileListWidth = $state(256);
  let resizingFiles = false;

  // Stable identity for the selected target — changes when the user picks a
  // different commit or worktree, the signal the load effects key on.
  const targetKey = $derived(
    target.kind === "uncommitted"
      ? `wt:${target.worktreePath}`
      : `c:${target.repoRoot}:${target.oid}`,
  );

  // Normalized to `{ found, files }` for both target kinds: a worktree read
  // always "found" (its absence is handled upstream by reconciliation), a commit
  // read carries the backend's found flag.
  function loadFiles(t: DiffTarget): Promise<{ found: boolean; files: ChangedFile[] }> {
    return t.kind === "uncommitted"
      ? changedFiles(t.worktreePath).then((files) => ({ found: true, files }))
      : commitChangedFiles(t.repoRoot, t.oid);
  }

  function loadDiff(t: DiffTarget, file: string): Promise<FileDiff> {
    return t.kind === "uncommitted"
      ? fileDiff(t.worktreePath, file)
      : commitFileDiff(t.repoRoot, t.oid, file);
  }

  // (Re)load the file list whenever the target or refresh revision changes. A
  // target change clears immediately; a same-target refresh updates in place so
  // the panel doesn't flash empty while the re-read is in flight.
  $effect(() => {
    const token = ++filesToken;
    const t = target;
    const key = targetKey;
    const revision = refreshRevision;
    const previousFile = untrack(() => selectedFile);
    if (filesKey !== key) {
      files = null;
      selectedFile = null;
      diff = null;
      commitFound = true;
    }
    filesError = null;
    void loadFiles(t)
      .then((result) => {
        if (token !== filesToken || revision !== refreshRevision) return;
        filesKey = key;
        commitFound = result.found;
        files = result.files;
        selectedFile = result.files.some((file) => file.path === previousFile)
          ? previousFile
          : (result.files[0]?.path ?? null);
      })
      .catch((e: unknown) => {
        if (token !== filesToken || revision !== refreshRevision) return;
        filesKey = key;
        files = [];
        filesError = e instanceof Error ? e.message : String(e);
      });
  });

  // Load the selected file's diff. Keyed on target, file, and refresh revision.
  // Same-file refreshes keep the current diff visible until replacement content
  // arrives; target/file switches show the loading state.
  $effect(() => {
    const file = selectedFile;
    const t = target;
    const key = targetKey;
    const revision = refreshRevision;
    if (file === null) {
      diff = null;
      return;
    }
    const token = ++diffToken;
    const sameTarget = diffKey === `${key}::${file}` && diff !== null;
    if (!sameTarget) {
      diffLoading = true;
    }
    diffError = null;
    void loadDiff(t, file)
      .then((result) => {
        if (token !== diffToken || revision !== refreshRevision) return;
        diffKey = `${key}::${file}`;
        diff = result;
        diffLoading = false;
      })
      .catch((e: unknown) => {
        if (token !== diffToken || revision !== refreshRevision) return;
        diff = null;
        diffKey = null;
        diffError = e instanceof Error ? e.message : String(e);
        diffLoading = false;
      });
  });

  const language = $derived(selectedFile ? languageForPath(selectedFile) : "");

  const styleOptions: { value: DiffStyle; label: string }[] = [
    { value: "side_by_side", label: "Split" },
    { value: "unified", label: "Unified" },
  ];

  function changeBadge(kind: ChangeKind): { letter: string; class: string } {
    switch (kind) {
      case "added":
      case "untracked":
        return { letter: "A", class: "text-diff-added" };
      case "deleted":
        return { letter: "D", class: "text-diff-removed" };
      case "renamed":
        return { letter: "R", class: "text-muted" };
      case "modified":
        return { letter: "M", class: "text-warning" };
    }
  }

  function directoryLabel(filePath: string): string {
    const i = filePath.lastIndexOf("/");
    return i >= 0 ? filePath.slice(0, i) : "";
  }

  // Only a worktree path is revealable in Finder; a commit has no folder.
  function revealTarget(): void {
    if (target.kind !== "uncommitted") return;
    const path = target.worktreePath;
    void revealInFinder(path).catch((e: unknown) => {
      console.error("[switchboard] reveal worktree failed", e);
    });
  }

  function openFileDifftool(file: ChangedFile, event: MouseEvent): void {
    // Mouse clicks should not leave the hover-revealed action pinned visible;
    // keyboard activation keeps focus for accessibility.
    if (event.detail > 0) {
      (event.currentTarget as HTMLButtonElement).blur();
    }
    externalActionError = null;
    const action =
      target.kind === "uncommitted"
        ? openWorktreeFileDifftool(target.worktreePath, file.path, file.change)
        : openCommitFileDifftool(target.repoRoot, target.oid, file.path);
    void action.catch((e: unknown) => {
      externalActionError = e instanceof Error ? e.message : String(e);
      console.error("[switchboard] git difftool failed", e);
    });
  }

  function startFileResize(event: PointerEvent): void {
    resizingFiles = true;
    event.preventDefault();
  }

  function resizeFiles(event: PointerEvent): void {
    if (!resizingFiles || bodyEl === null) return;
    const rect = bodyEl.getBoundingClientRect();
    const max = Math.min(440, rect.width * 0.55);
    fileListWidth = Math.min(max, Math.max(176, event.clientX - rect.left));
  }

  function endFileResize(): void {
    resizingFiles = false;
  }
</script>

<div class="flex min-h-0 flex-1 flex-col overflow-hidden" data-testid="diff-panel">
  <!-- Header -->
  <div class="border-border/60 bg-raised flex min-h-11 items-center gap-3 border-b px-3 py-2">
    <div class="min-w-0 flex-1">
      <div class="text-fg truncate text-sm leading-5 font-semibold" data-testid="detail-title">
        {target.title}
      </div>
      {#if target.kind === "uncommitted"}
        <button
          type="button"
          class="text-muted hover:text-fg block max-w-full min-w-0 truncate text-left font-mono text-[11px] leading-4"
          title={`${target.subtitle} — reveal in Finder`}
          data-testid="detail-subtitle"
          onclick={revealTarget}
        >
          {target.subtitle}
        </button>
      {:else}
        <div
          class="text-muted truncate font-mono text-[11px] leading-4"
          data-testid="detail-subtitle"
        >
          {target.subtitle}
        </div>
      {/if}
    </div>

    <div
      class={cn(SEGMENTED_MAIN_CONTAINER_CLASS, "flex shrink-0")}
      role="radiogroup"
      aria-label="Diff layout"
    >
      {#each styleOptions as option (option.value)}
        <button
          type="button"
          role="radio"
          class={cn(
            SEGMENTED_MAIN_ITEM_CLASS,
            preferences.diff_style === option.value
              ? SEGMENTED_MAIN_ITEM_ACTIVE_CLASS
              : SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
          )}
          aria-checked={preferences.diff_style === option.value}
          data-testid={`diff-style-${option.value}`}
          onclick={() => void updatePreferences({ diff_style: option.value })}
        >
          {option.label}
        </button>
      {/each}
    </div>

    {#if onToggleDetailExpanded}
      <Tooltip
        label={detailExpanded ? "Restore Git details panel" : "Expand Git details panel"}
        shortcut={shortcut("mod", "shift", "D")}
        side="bottom"
      >
        {#snippet trigger(props)}
          <button
            {...props}
            type="button"
            class={cn(ICON_BUTTON_CLASS, "hover:bg-border/60 shrink-0")}
            aria-label={detailExpanded ? "Restore Git details panel" : "Expand Git details panel"}
            data-testid="detail-expand-toggle"
            onclick={onToggleDetailExpanded}
          >
            {#if detailExpanded}
              <Minimize2 size={14} strokeWidth={1.8} aria-hidden="true" />
            {:else}
              <Maximize2 size={14} strokeWidth={1.8} aria-hidden="true" />
            {/if}
          </button>
        {/snippet}
      </Tooltip>
    {/if}

    <button
      type="button"
      class={cn(ICON_BUTTON_CLASS, "hover:bg-border/60 shrink-0")}
      aria-label="Close diff panel"
      data-testid="detail-close"
      onclick={onClose}
    >
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
        <path d="M18 6 6 18M6 6l12 12" />
      </svg>
    </button>
  </div>

  {#if externalActionError}
    <p
      class="border-border/60 text-status-failed border-b px-3 py-1.5 text-xs"
      data-testid="diff-external-error"
    >
      {externalActionError}
    </p>
  {/if}

  <!-- Body: file list + diff -->
  {#if files === null}
    <EmptyState testid="detail-loading" title="Loading changes…" />
  {:else if filesError !== null}
    <EmptyState
      testid="detail-files-error"
      tone="error"
      title="Couldn't read changes."
      description={filesError}
    />
  {:else if !commitFound}
    <EmptyState
      testid="detail-commit-missing"
      title="This commit is no longer available."
      description="It may have been garbage-collected or the branch was updated. Refresh the branch."
    />
  {:else if files.length === 0}
    {#if target.kind === "uncommitted"}
      <EmptyState
        testid="detail-no-changes"
        title="No uncommitted changes."
        description="This folder matches its last commit."
      />
    {:else}
      <EmptyState testid="detail-no-changes" title="This commit changed no files." />
    {/if}
  {:else}
    <div class="flex min-h-0 flex-1 overflow-hidden" bind:this={bodyEl}>
      <!-- Changed-files list -->
      <div
        class="border-border/60 bg-panel shrink-0 overflow-hidden border-r"
        style={`width: ${fileListWidth}px`}
      >
        <div
          class="border-border/60 bg-surface flex h-8 items-center justify-between border-b px-2"
        >
          <span class="text-muted text-[11px] font-semibold tracking-wide uppercase">
            Changed files
          </span>
          <span class="text-muted font-mono text-[11px]">{files.length}</span>
        </div>
        <ul class="h-[calc(100%-2rem)] overflow-y-auto py-1" data-testid="changed-files">
          {#each files as file (file.path)}
            {@const badge = changeBadge(file.change)}
            {@const directory = directoryLabel(file.path)}
            <li>
              <div
                class={cn(
                  "group flex w-full items-stretch gap-1 rounded-none text-xs transition-colors",
                  file.path === selectedFile ? "bg-raised text-fg" : "text-muted hover:bg-raised",
                )}
              >
                <!-- The padding lives on the button (not the row) so the click
                     target fills the whole hover area — otherwise the top/bottom
                     (and left) padding band highlights but swallows the click. -->
                <button
                  type="button"
                  class="flex min-w-0 flex-1 items-start gap-2 px-2 py-1.5 text-left"
                  data-testid="changed-file"
                  data-selected={file.path === selectedFile}
                  onclick={() => (selectedFile = file.path)}
                >
                  <span
                    class={cn("mt-0.5 w-4 shrink-0 text-center font-mono text-[11px]", badge.class)}
                    >{badge.letter}</span
                  >
                  <span class="min-w-0 flex-1">
                    <span class="block truncate" title={file.path}>{basename(file.path)}</span>
                    {#if directory}
                      <span class="text-muted/80 block truncate font-mono text-[10px] leading-4">
                        {directory}
                      </span>
                    {/if}
                  </span>
                </button>
                <Tooltip label="Open in difftool" side="left">
                  {#snippet trigger(props)}
                    <button
                      {...props}
                      type="button"
                      class={cn(
                        ICON_BUTTON_CLASS,
                        "hover:bg-border/60 mr-2 h-6 w-6 shrink-0 self-center opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100",
                      )}
                      aria-label={`Open ${file.path} in difftool`}
                      data-testid="changed-file-difftool"
                      onclick={(event) => openFileDifftool(file, event)}
                    >
                      <ExternalLink size={14} strokeWidth={1.8} aria-hidden="true" />
                    </button>
                  {/snippet}
                </Tooltip>
              </div>
            </li>
          {/each}
        </ul>
      </div>

      <div
        class="border-border/60 bg-panel hover:bg-raised w-1.5 shrink-0 cursor-col-resize border-r transition-colors"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize changed files list"
        data-testid="changed-files-resizer"
        onpointerdown={startFileResize}
      ></div>

      <!-- Diff -->
      <div class="bg-raised min-w-0 flex-1 overflow-auto" data-testid="diff-scroll">
        {#if diffLoading}
          <div class="flex items-center justify-center py-6">
            <Spinner class="h-4 w-4" />
          </div>
        {:else if diffError !== null}
          <EmptyState
            testid="diff-error"
            tone="error"
            title="Couldn't read this file's diff."
            description={diffError}
          />
        {:else if diff !== null}
          <DiffView {diff} style={preferences.diff_style} {language} />
        {/if}
      </div>
    </div>
  {/if}
</div>

<svelte:window onpointermove={resizeFiles} onpointerup={endFileResize} />
