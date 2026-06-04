<script lang="ts">
  /// The worktree detail panel: a file's working-tree changes, inspected without
  /// leaving Switchboard. Header (branch label + clickable path + layout toggle +
  /// close), a changed-files list, and the selected file's read-only diff. Loads
  /// are guarded against path/file races (a newer selection's result always wins).
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

  let { path, label, onClose }: { path: string; label: string; onClose: () => void } = $props();

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

  // (Re)load the file list whenever the worktree path changes. Auto-selects the
  // first file so the panel opens showing a diff, not an empty pane.
  $effect(() => {
    const token = ++filesToken;
    const wt = path;
    files = null;
    filesError = null;
    selectedFile = null;
    diff = null;
    void changedFiles(wt)
      .then((result) => {
        if (token !== filesToken) return;
        files = result;
        selectedFile = result[0]?.path ?? null;
      })
      .catch((e: unknown) => {
        if (token !== filesToken) return;
        files = [];
        filesError = e instanceof Error ? e.message : String(e);
      });
  });

  // Load the selected file's diff. Keyed on both path and file so switching either
  // refetches; the token guard drops a superseded result.
  $effect(() => {
    const file = selectedFile;
    const wt = path;
    if (file === null) {
      diff = null;
      return;
    }
    const token = ++diffToken;
    diffLoading = true;
    diffError = null;
    void fileDiff(wt, file)
      .then((result) => {
        if (token !== diffToken) return;
        diff = result;
        diffLoading = false;
      })
      .catch((e: unknown) => {
        if (token !== diffToken) return;
        diff = null;
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

  function revealWorktree(): void {
    void revealInFinder(path).catch((e: unknown) => {
      console.error("[switchboard] reveal worktree failed", e);
    });
  }
</script>

<div class="flex min-h-0 flex-1 flex-col overflow-hidden" data-testid="worktree-detail-panel">
  <!-- Header -->
  <div class="border-border/60 flex items-center gap-2 border-b px-3 py-1.5">
    <span class="text-fg shrink-0 text-sm font-semibold" data-testid="detail-branch">{label}</span>
    <button
      type="button"
      class="text-muted hover:text-fg min-w-0 truncate text-left font-mono text-[11px]"
      title={`${path} — reveal in Finder`}
      data-testid="detail-path"
      onclick={revealWorktree}
    >
      {path}
    </button>

    <div
      class={cn(SEGMENTED_MAIN_CONTAINER_CLASS, "ml-auto flex shrink-0")}
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
    <div class="flex min-h-0 flex-1 overflow-hidden">
      <!-- Changed-files list -->
      <ul
        class="border-border/60 w-56 shrink-0 overflow-y-auto border-r py-1"
        data-testid="changed-files"
      >
        {#each files as file (file.path)}
          {@const badge = changeBadge(file.change)}
          <li>
            <button
              type="button"
              class={cn(
                "flex w-full items-center gap-2 px-2 py-1 text-left text-xs",
                file.path === selectedFile ? "bg-raised text-fg" : "text-muted hover:bg-panel",
              )}
              data-testid="changed-file"
              data-selected={file.path === selectedFile}
              onclick={() => (selectedFile = file.path)}
            >
              <span class={cn("w-3 shrink-0 text-center font-mono", badge.class)}
                >{badge.letter}</span
              >
              <span class="truncate" title={file.path}>{basename(file.path)}</span>
            </button>
          </li>
        {/each}
      </ul>

      <!-- Diff -->
      <div class="min-w-0 flex-1 overflow-auto" data-testid="diff-scroll">
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
