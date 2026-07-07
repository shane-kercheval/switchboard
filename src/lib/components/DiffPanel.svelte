<script lang="ts">
  /// The right-hand diff panel: the changed files + read-only diff for the
  /// selected [`DiffTarget`] — either a worktree's uncommitted changes or one
  /// commit's diff (vs. its first parent). One panel serves both because a commit
  /// diff needs no worktree, so a branch with no local folder (or a remote-only
  /// ref) still shows real content. Loads are guarded against target/file races
  /// (a newer selection's result always wins).
  import { tick, untrack } from "svelte";
  import {
    Code2,
    Copy,
    ExternalLink,
    Maximize2,
    MessageSquareText,
    Minimize2,
  } from "@lucide/svelte";
  import { cn, basename } from "$lib/utils";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import Markdown from "$lib/components/ui/Markdown.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import DiffView from "$lib/components/DiffView.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import AsyncIconButton from "$lib/components/ui/AsyncIconButton.svelte";
  import { ICON_BUTTON_CLASS, SELECTED_ROW_ICON_HOVER } from "$lib/components/ui/iconButton";
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
    openInEditor,
    openCommitFileDifftool,
    openWorktreeFileDifftool,
    revealInFinder,
  } from "$lib/api";
  import { languageForPath } from "$lib/diff";
  import { copyText } from "$lib/native";
  import { isEditableShortcutTarget } from "$lib/keyboard";
  import { shortcut } from "$lib/platform";
  import { preferences, updatePreferences } from "$lib/preferences.svelte";
  import type { ChangedFile, ChangeKind, DiffStyle, FileDiff } from "$lib/types";
  import { navFocus, hoverSuppressed, hoverableClass, nextIndex } from "$lib/state/gitView.svelte";
  import { palette } from "$lib/state/commandPalette.svelte";
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
  let commitBody = $state<string | null>(null);
  let commitMessageOpen = $state(false);
  let commitMessageTooltipOpen = $state(false);
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
  const visibleCommitBody = $derived(
    commitBody !== null && commitBody.trim().length > 0 ? commitBody.trim() : null,
  );

  // Normalized to `{ found, files }` for both target kinds: a worktree read
  // always "found" (its absence is handled upstream by reconciliation), a commit
  // read carries the backend's found flag.
  function loadFiles(
    t: DiffTarget,
  ): Promise<{ found: boolean; body: string | null; files: ChangedFile[] }> {
    return t.kind === "uncommitted"
      ? changedFiles(t.worktreePath).then((files) => ({ found: true, body: null, files }))
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
      commitBody = null;
      commitMessageOpen = false;
      commitMessageTooltipOpen = false;
    }
    filesError = null;
    void loadFiles(t)
      .then((result) => {
        if (token !== filesToken || revision !== refreshRevision) return;
        filesKey = key;
        commitFound = result.found;
        commitBody = result.found ? (result.body ?? null) : null;
        files = result.files;
        selectedFile = result.files.some((file) => file.path === previousFile)
          ? previousFile
          : (result.files[0]?.path ?? null);
      })
      .catch((e: unknown) => {
        if (token !== filesToken || revision !== refreshRevision) return;
        filesKey = key;
        files = [];
        commitBody = null;
        commitMessageOpen = false;
        commitMessageTooltipOpen = false;
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

  $effect(() => {
    if (visibleCommitBody === null) commitMessageOpen = false;
  });

  $effect(() => {
    if (commitMessageOpen) commitMessageTooltipOpen = false;
  });

  // Drop the file-row hover highlight right after a keyboard move so the mouse-
  // hovered row doesn't stay lit next to the keyboard selection; pointer movement
  // (handled below) clears the suppression and hover returns.
  const hoverBg = $derived(hoverableClass("hover:bg-raised"));

  // The action-icons reveal (and the room the row text makes for it) is keyed on
  // the real mouse `:hover` via `group-hover`, so it must be suppressed alongside
  // the background during keyboard nav — otherwise the icons linger under the
  // cursor on the row the keyboard just left. `group-focus-within` stays so
  // keyboard focus still reveals them.
  const iconsHoverReveal = $derived(
    hoverableClass("group-hover:pointer-events-auto group-hover:opacity-100"),
  );
  const padFocus = $derived(
    target.kind === "uncommitted" ? "group-focus-within:pr-[5.75rem]" : "group-focus-within:pr-10",
  );
  const padHoverReveal = $derived(
    hoverableClass(
      target.kind === "uncommitted" ? "group-hover:pr-[5.75rem]" : "group-hover:pr-10",
    ),
  );

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

  function worktreeFilePath(worktreePath: string, filePath: string): string {
    return `${worktreePath.replace(/\/+$/, "")}/${filePath.replace(/^\/+/, "")}`;
  }

  function canOpenFileInEditor(file: ChangedFile): boolean {
    return target.kind === "uncommitted" && file.change !== "deleted";
  }

  // Only a worktree path is revealable in Finder; a commit has no folder.
  function revealTarget(): void {
    if (target.kind !== "uncommitted") return;
    const path = target.worktreePath;
    void revealInFinder(path).catch((e: unknown) => {
      console.error("[switchboard] reveal worktree failed", e);
    });
  }

  function onExternalActionError(message: string, error: unknown): void {
    externalActionError = error instanceof Error ? error.message : String(error);
    console.error(message, error);
  }

  function openFileDifftool(file: ChangedFile): Promise<void> {
    externalActionError = null;
    return target.kind === "uncommitted"
      ? openWorktreeFileDifftool(target.worktreePath, file.path, file.change)
      : openCommitFileDifftool(target.repoRoot, target.oid, file.path);
  }

  function copyFilePath(file: ChangedFile): Promise<void> {
    if (target.kind !== "uncommitted") return Promise.resolve();
    externalActionError = null;
    return copyText(worktreeFilePath(target.worktreePath, file.path));
  }

  function openFileInEditor(file: ChangedFile): Promise<void> {
    if (target.kind !== "uncommitted" || !canOpenFileInEditor(file)) return Promise.resolve();
    externalActionError = null;
    return openInEditor(worktreeFilePath(target.worktreePath, file.path));
  }

  function startFileResize(event: PointerEvent): void {
    resizingFiles = true;
    event.preventDefault();
  }

  function onWindowPointerMove(event: PointerEvent): void {
    if (hoverSuppressed.value) hoverSuppressed.value = false;
    resizeFiles(event);
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

  let filesListEl = $state<HTMLUListElement | null>(null);

  function moveFileSelection(delta: number): void {
    if (files === null) return;
    const idx = files.findIndex((f) => f.path === selectedFile);
    const next = nextIndex(files.length, idx, delta);
    if (next === null) return;
    selectedFile = files[next]!.path;
    // `data-selected="true"` is unique within the file list, so the scroll target
    // needs no test-hook selector.
    void tick().then(() => {
      filesListEl?.querySelector('[data-selected="true"]')?.scrollIntoView({ block: "nearest" });
    });
  }

  // Arrow up/down navigate the changed-files list when it's the focused
  // selection (the user last clicked a file); the commit pane handles them
  // otherwise. The two panes key off the shared `navFocus`, so only one acts.
  function onFileNavKeydown(event: KeyboardEvent): void {
    if (navFocus.pane !== "files") return;
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    // Yield to an open overlay or an event a closer handler already consumed.
    if (event.defaultPrevented || palette.open) return;
    if (isEditableShortcutTarget(event.target)) return;
    event.preventDefault();
    hoverSuppressed.value = true;
    moveFileSelection(event.key === "ArrowDown" ? 1 : -1);
  }

  $effect(() => {
    window.addEventListener("keydown", onFileNavKeydown);
    return () => window.removeEventListener("keydown", onFileNavKeydown);
  });
</script>

<div class="flex min-h-0 flex-1 flex-col overflow-hidden" data-testid="diff-panel">
  <!-- Header -->
  <div class="border-border/60 bg-raised flex min-h-11 items-center gap-3 border-b px-3 py-2">
    <div class="min-w-0 flex-1">
      <div class="flex min-w-0 items-center gap-1.5">
        <div class="text-fg truncate text-sm leading-5 font-semibold" data-testid="detail-title">
          {target.title}
        </div>
        {#if visibleCommitBody !== null}
          <Tooltip
            bind:open={commitMessageTooltipOpen}
            label="Show commit message"
            side="bottom"
            disabled={commitMessageOpen}
            ignoreNonKeyboardFocus
          >
            {#snippet trigger(props)}
              <button
                {...props}
                type="button"
                class={cn(ICON_BUTTON_CLASS, "hover:bg-border/60 h-5 w-5 shrink-0")}
                aria-label="Show commit message"
                data-testid="commit-message-open"
                onclick={() => {
                  commitMessageTooltipOpen = false;
                  commitMessageOpen = true;
                }}
              >
                <MessageSquareText size={13} strokeWidth={1.8} aria-hidden="true" />
              </button>
            {/snippet}
          </Tooltip>
        {/if}
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

  {#if visibleCommitBody !== null}
    <Dialog bind:open={commitMessageOpen} title="Commit message" contentClass="max-w-2xl">
      <div class="mb-3 min-w-0">
        <div class="text-fg truncate text-sm leading-5 font-semibold">{target.title}</div>
        {#if target.kind === "commit"}
          <div class="text-muted truncate font-mono text-[11px] leading-4">{target.subtitle}</div>
        {/if}
      </div>
      <div class="max-h-[60vh] overflow-y-auto" data-testid="commit-message-body">
        <Markdown text={visibleCommitBody} />
      </div>
    </Dialog>
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
        <ul
          bind:this={filesListEl}
          class="h-[calc(100%-2rem)] overflow-y-auto py-1"
          data-testid="changed-files"
        >
          {#each files as file (file.path)}
            {@const badge = changeBadge(file.change)}
            {@const directory = directoryLabel(file.path)}
            {@const isSelected = file.path === selectedFile}
            <li>
              <!-- `data-selected` drives the action-icons' hover color via the
                   shared `SELECTED_ROW_ICON_HOVER` `group-data-` variant: the
                   icons hover gray by default and white (`bg-raised`) on a
                   selected (blue) row so they read against the blue. CSS (not a
                   JS class) because the buttons live inside Tooltip `{#snippet}`s,
                   which don't re-render when `selectedFile` changes. -->
              <div
                data-selected={isSelected}
                class={cn(
                  "group relative flex w-full items-stretch rounded-none text-xs transition-colors",
                  isSelected ? "bg-selected text-fg" : cn("text-muted", hoverBg),
                )}
              >
                <!-- The padding lives on the button (not the row) so the click
                     target fills the whole hover area — otherwise the top/bottom
                     (and left) padding band highlights but swallows the click. -->
                <button
                  type="button"
                  class={cn(
                    "flex min-w-0 flex-1 items-start gap-2 px-2 py-1.5 pr-2 text-left transition-[padding]",
                    padFocus,
                    padHoverReveal,
                  )}
                  data-testid="changed-file"
                  data-selected={file.path === selectedFile}
                  onclick={() => {
                    selectedFile = file.path;
                    navFocus.pane = "files";
                  }}
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
                <div
                  class={cn(
                    "pointer-events-none absolute top-1/2 right-2 flex -translate-y-1/2 items-center gap-0.5 opacity-0 transition-opacity group-focus-within:pointer-events-auto group-focus-within:opacity-100",
                    iconsHoverReveal,
                  )}
                >
                  {#if target.kind === "uncommitted"}
                    <Tooltip label="Copy path" side="top">
                      {#snippet trigger(props)}
                        <AsyncIconButton
                          {...props}
                          class={cn(
                            ICON_BUTTON_CLASS,
                            "h-6 w-6 shrink-0",
                            "hover:bg-border/60",
                            SELECTED_ROW_ICON_HOVER,
                          )}
                          label={`Copy path for ${file.path}`}
                          testid="changed-file-copy-path"
                          action={() => copyFilePath(file)}
                          onError={(error) =>
                            onExternalActionError("[switchboard] copy file path failed", error)}
                        >
                          <Copy size={14} strokeWidth={1.8} aria-hidden="true" />
                        </AsyncIconButton>
                      {/snippet}
                    </Tooltip>
                    {#if canOpenFileInEditor(file)}
                      <Tooltip label="Open in editor" side="top">
                        {#snippet trigger(props)}
                          <AsyncIconButton
                            {...props}
                            class={cn(
                              ICON_BUTTON_CLASS,
                              "h-6 w-6 shrink-0",
                              "hover:bg-border/60",
                              SELECTED_ROW_ICON_HOVER,
                            )}
                            label={`Open ${file.path} in editor`}
                            testid="changed-file-editor"
                            completeAfterMs={700}
                            action={() => openFileInEditor(file)}
                            onError={(error) =>
                              onExternalActionError(
                                "[switchboard] open file in editor failed",
                                error,
                              )}
                          >
                            <Code2 size={14} strokeWidth={1.8} aria-hidden="true" />
                          </AsyncIconButton>
                        {/snippet}
                      </Tooltip>
                    {/if}
                  {/if}
                  <Tooltip label="Open in difftool" side="top">
                    {#snippet trigger(props)}
                      <AsyncIconButton
                        {...props}
                        class={cn(
                          ICON_BUTTON_CLASS,
                          "h-6 w-6 shrink-0",
                          "hover:bg-border/60",
                          SELECTED_ROW_ICON_HOVER,
                        )}
                        label={`Open ${file.path} in difftool`}
                        testid="changed-file-difftool"
                        completeAfterMs={700}
                        action={() => openFileDifftool(file)}
                        onError={(error) =>
                          onExternalActionError("[switchboard] git difftool failed", error)}
                      >
                        <ExternalLink size={14} strokeWidth={1.8} aria-hidden="true" />
                      </AsyncIconButton>
                    {/snippet}
                  </Tooltip>
                </div>
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

<svelte:window onpointermove={onWindowPointerMove} onpointerup={endFileResize} />
