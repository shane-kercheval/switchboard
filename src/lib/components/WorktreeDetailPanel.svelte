<script lang="ts">
  /// The worktree detail panel: a file's working-tree changes, inspected without
  /// leaving Switchboard. Header (branch label + clickable path + layout toggle +
  /// close), a changed-files list, and the selected file's read-only diff. Loads
  /// are guarded against path/file races (a newer selection's result always wins).
  import { untrack } from "svelte";
  import { cn, basename } from "$lib/utils";
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import DiffView from "$lib/components/DiffView.svelte";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";
  import {
    SEGMENTED_MAIN_CONTAINER_CLASS,
    SEGMENTED_MAIN_ITEM_CLASS,
    SEGMENTED_MAIN_ITEM_ACTIVE_CLASS,
    SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
  } from "$lib/components/ui/segmentedControl";
  import { changedFiles, fileDiff, revealInFinder } from "$lib/api";
  import { languageForPath } from "$lib/diff";
  import { preferences, updatePreferences } from "$lib/preferences.svelte";
  import type { ChangedFile, ChangeKind, DiffStyle, FileDiff } from "$lib/types";

  let {
    path,
    label,
    refreshRevision = 0,
    onClose,
  }: { path: string; label: string; refreshRevision?: number; onClose: () => void } = $props();

  let files = $state<ChangedFile[] | null>(null);
  let filesError = $state<string | null>(null);
  let selectedFile = $state<string | null>(null);

  let diff = $state<FileDiff | null>(null);
  let diffError = $state<string | null>(null);
  let diffLoading = $state(false);

  // Monotonic tokens: a stale async result (the worktree/file changed before it
  // resolved) is discarded rather than clobbering the current selection.
  let filesToken = 0;
  let diffToken = 0;
  let filesPath: string | null = null;
  let diffPath: string | null = null;
  let diffFile: string | null = null;
  let bodyEl = $state<HTMLDivElement | null>(null);
  let fileListWidth = $state(256);
  let resizingFiles = false;

  // (Re)load the file list whenever the worktree path or refresh revision
  // changes. A path change clears immediately; a same-path refresh updates in
  // place so the panel doesn't flash empty while the re-read is in flight.
  $effect(() => {
    const token = ++filesToken;
    const wt = path;
    const revision = refreshRevision;
    const previousFile = untrack(() => selectedFile);
    const pathChanged = filesPath !== wt;
    if (pathChanged) {
      files = null;
      selectedFile = null;
      diff = null;
    }
    filesError = null;
    void changedFiles(wt)
      .then((result) => {
        if (token !== filesToken || revision !== refreshRevision) return;
        filesPath = wt;
        files = result;
        selectedFile = result.some((file) => file.path === previousFile)
          ? previousFile
          : (result[0]?.path ?? null);
      })
      .catch((e: unknown) => {
        if (token !== filesToken || revision !== refreshRevision) return;
        filesPath = wt;
        files = [];
        filesError = e instanceof Error ? e.message : String(e);
      });
  });

  // Load the selected file's diff. Keyed on path, file, and refresh revision.
  // Same-file refreshes keep the current diff visible until replacement content
  // arrives; path/file switches show the loading state.
  $effect(() => {
    const file = selectedFile;
    const wt = path;
    const revision = refreshRevision;
    if (file === null) {
      diff = null;
      return;
    }
    const token = ++diffToken;
    const sameTarget = diffPath === wt && diffFile === file && diff !== null;
    if (!sameTarget) {
      diffLoading = true;
    }
    diffError = null;
    void fileDiff(wt, file)
      .then((result) => {
        if (token !== diffToken || revision !== refreshRevision) return;
        diffPath = wt;
        diffFile = file;
        diff = result;
        diffLoading = false;
      })
      .catch((e: unknown) => {
        if (token !== diffToken || revision !== refreshRevision) return;
        diff = null;
        diffPath = null;
        diffFile = null;
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

  function revealWorktree(): void {
    void revealInFinder(path).catch((e: unknown) => {
      console.error("[switchboard] reveal worktree failed", e);
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

<div class="flex min-h-0 flex-1 flex-col overflow-hidden" data-testid="worktree-detail-panel">
  <!-- Header -->
  <div class="border-border/60 bg-raised flex min-h-11 items-center gap-3 border-b px-3 py-2">
    <div class="min-w-0 flex-1">
      <div class="text-fg truncate text-sm leading-5 font-semibold" data-testid="detail-branch">
        {label}
      </div>
      <button
        type="button"
        class="text-muted hover:text-fg block max-w-full min-w-0 truncate text-left font-mono text-[11px] leading-4"
        title={`${path} — reveal in Finder`}
        data-testid="detail-path"
        onclick={revealWorktree}
      >
        {path}
      </button>
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

    <button
      type="button"
      class={cn(ICON_BUTTON_CLASS, "shrink-0")}
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
  {:else if files.length === 0}
    <EmptyState testid="detail-no-changes" title="No changes in this worktree." />
  {:else}
    <div class="flex min-h-0 flex-1 overflow-hidden" bind:this={bodyEl}>
      <!-- Changed-files list -->
      <div
        class="border-border/60 bg-surface shrink-0 overflow-hidden border-r"
        style={`width: ${fileListWidth}px`}
      >
        <div
          class="border-border/60 bg-panel/70 flex h-8 items-center justify-between border-b px-2"
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
              <button
                type="button"
                class={cn(
                  "flex w-full items-start gap-2 rounded-none px-2 py-1.5 text-left text-xs transition-colors",
                  file.path === selectedFile ? "bg-raised text-fg" : "text-muted hover:bg-panel",
                )}
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
