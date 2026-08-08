<script lang="ts">
  /// The transcript navigator: a centered overlay (the command-palette idiom —
  /// a focus-trapped modal over a dimmed, blurred transcript) listing every
  /// message in the conversation with type-to-filter, a role filter, a sort
  /// toggle, and a live preview panel to the list's right. Clicking or ↵ jumps
  /// the owning pane's transcript to that message (`jumpToRow` handles pane
  /// reveal + window re-pin). Entries derive from the row model, never the DOM
  /// — the transcript is render-windowed. Opened by the header button, ⌘F, or
  /// the command palette (all via `navigatorState`).
  import { onDestroy, tick, untrack } from "svelte";
  import { ArrowDownWideNarrow, ArrowUpWideNarrow, Pin, TableOfContents } from "@lucide/svelte";
  import { cn, relativeTime } from "$lib/utils";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import SegmentedSelect from "$lib/components/ui/SegmentedSelect.svelte";
  import Markdown from "$lib/components/ui/Markdown.svelte";
  import { transcripts, type Turn } from "$lib/state/index.svelte";
  import { buildUnifiedRows } from "$lib/state/unified";
  import {
    buildNavigatorEntries,
    filterEntries,
    type NavigatorEntry,
    type NavigatorRoleFilter,
  } from "$lib/transcriptIndex";
  import {
    buildJumpPaneIndex,
    canResolveJumpFromIndex,
    jumpToRow,
    navigatorState,
  } from "$lib/state/transcriptJump.svelte";
  import {
    isMessagePinned,
    loadMessagePins,
    pinsLoaded,
    pinsUnavailableReason,
    toggleMessagePin,
  } from "$lib/state/messagePins.svelte";
  import type { AgentRecord, ConversationItem, ProjectId } from "$lib/types";

  let {
    projectId,
    agents,
    overlay = [],
  }: {
    projectId: ProjectId;
    agents: AgentRecord[];
    overlay?: ConversationItem[];
  } = $props();

  let query = $state("");
  let role = $state<string>("all");
  /// Newest-first by default: the common use is "jump to something recent," so
  /// the most recent message should be at the top rather than after a scroll.
  let descending = $state(true);
  let pinnedOnly = $state(false);
  let highlighted = $state(0);
  let findTooltipOpen = $state(false);
  let findTooltipSuppressed = $state(false);
  /// The entry whose full text shows in the preview panel. Keyboard moves set
  /// it immediately (the preview follows the highlight); hover sets it through
  /// a short debounce so running the cursor down the list doesn't flash panels.
  let previewKey = $state<string | null>(null);
  let hoverTimer: ReturnType<typeof setTimeout> | null = null;
  let searchEl = $state<HTMLInputElement | null>(null);
  let listEl = $state<HTMLElement | null>(null);
  let previewEl = $state<HTMLElement | null>(null);
  let entries = $state<NavigatorEntry[]>([]);
  let indexStatus = $state<"idle" | "loading" | "ready">("idle");
  let indexedProjectId = $state<ProjectId | null>(null);
  let indexFrame: number | null = null;
  let indexTimer: ReturnType<typeof setTimeout> | null = null;
  let indexGeneration = 0;
  let latestIndexSource: IndexSource | null = null;
  let lastIndexBuildAt = 0;

  const open = $derived(navigatorState.open);
  const rosterIds = $derived(agents.map((a) => a.id));
  const pinsReady = $derived(pinsLoaded(projectId));
  const jumpPaneIndex = $derived(buildJumpPaneIndex(projectId, rosterIds));

  $effect(() => {
    if (!open) return;
    if (findTooltipOpen) findTooltipSuppressed = true;
    findTooltipOpen = false;
  });

  type IndexSource = {
    projectId: ProjectId;
    agents: (Pick<AgentRecord, "id" | "name" | "harness"> & { turns: Turn[] })[];
    overlay: ConversationItem[];
  };

  function cancelScheduledIndex(): void {
    indexGeneration += 1;
    if (indexFrame !== null) cancelAnimationFrame(indexFrame);
    if (indexTimer !== null) clearTimeout(indexTimer);
    indexFrame = null;
    indexTimer = null;
  }

  function resetIndex(): void {
    cancelScheduledIndex();
    entries = [];
    indexedProjectId = null;
    indexStatus = "idle";
    latestIndexSource = null;
    lastIndexBuildAt = 0;
  }

  function buildLatestIndex(generation: number): void {
    indexTimer = null;
    if (generation !== indexGeneration || !navigatorState.open) return;
    const source = latestIndexSource;
    if (source === null || projectId !== source.projectId) return;
    const turns: Turn[] = [];
    for (const agent of source.agents) {
      for (const turn of agent.turns) turns.push(turn);
    }
    const agentIds = source.agents.map((agent) => agent.id);
    const rows = buildUnifiedRows(turns, source.overlay, new Set(agentIds));
    const nextEntries = buildNavigatorEntries(
      rows,
      new Map(source.agents.map((agent) => [agent.id, agent.name])),
      new Map(source.agents.map((agent) => [agent.id, agent.harness])),
    );
    if (generation !== indexGeneration || !navigatorState.open || projectId !== source.projectId) {
      return;
    }
    entries = nextEntries;
    indexedProjectId = source.projectId;
    indexStatus = "ready";
    lastIndexBuildAt = performance.now();
  }

  const INDEX_REFRESH_INTERVAL_MS = 250;

  /// Keep at most one build pending. Streaming updates replace `latestIndexSource`
  /// instead of postponing that build; once ready, refresh at a bounded rate with
  /// a trailing build that consumes the newest source.
  function scheduleIndex(source: IndexSource): void {
    latestIndexSource = source;
    if (indexedProjectId !== source.projectId) {
      cancelScheduledIndex();
      entries = [];
      indexStatus = "loading";
      indexedProjectId = source.projectId;
      lastIndexBuildAt = 0;
    }
    if (indexFrame !== null || indexTimer !== null) return;
    const generation = indexGeneration;
    const enqueue = (): void => {
      indexFrame = null;
      const delay =
        indexStatus === "ready"
          ? Math.max(0, lastIndexBuildAt + INDEX_REFRESH_INTERVAL_MS - performance.now())
          : 0;
      indexTimer = setTimeout(() => buildLatestIndex(generation), delay);
    };
    if (typeof requestAnimationFrame === "function") {
      indexFrame = requestAnimationFrame(enqueue);
    } else {
      enqueue();
    }
  }

  $effect(() => {
    if (!open) {
      untrack(resetIndex);
      return;
    }
    const source: IndexSource = {
      projectId,
      agents: agents.map(({ id, name, harness }) => ({
        id,
        name,
        harness,
        turns: transcripts[id] ?? [],
      })),
      overlay,
    };
    // Hydration replaces the overlay array. Length also catches a defensive
    // same-reference append without walking the full history while closed.
    void overlay.length;
    untrack(() => scheduleIndex(source));
  });

  onDestroy(cancelScheduledIndex);

  const filtered = $derived.by(() => {
    const matched = filterEntries(entries, query, role as NavigatorRoleFilter).filter(
      (entry) =>
        !pinnedOnly ||
        (entry.messageIdentity.kind === "pinnable" &&
          isMessagePinned(projectId, entry.messageIdentity)),
    );
    // The index is chronological (oldest first); descending shows newest first.
    return descending ? [...matched].reverse() : matched;
  });

  /// Entries whose message renders in no visible pane (agent unassigned or
  /// eye-hidden) can't be jumped to; they render disabled with a tooltip.
  function canJump(entry: NavigatorEntry): boolean {
    return canResolveJumpFromIndex(jumpPaneIndex, entry.agentIds);
  }

  function previewProse(entry: NavigatorEntry): string {
    return entry.prose.trim() === "" ? entry.preview : entry.prose;
  }

  const previewEntry = $derived(
    previewKey === null ? undefined : filtered.find((e) => e.rowKey === previewKey),
  );

  $effect(() => {
    const key = previewKey;
    const node = previewEl;
    if (key === null || node === null) return;
    void tick().then(() => {
      if (previewKey !== key || previewEl !== node) return;
      node.scrollTop = 0;
      updateScrollFade(node);
    });
  });

  function onOpen(): void {
    query = "";
    role = "all";
    descending = true;
    pinnedOnly = false;
    highlighted = 0;
    previewKey = null;
    void tick().then(() => searchEl?.focus());
    void loadMessagePins(projectId);
  }

  function close(): void {
    navigatorState.open = false;
    findTooltipOpen = false;
    if (hoverTimer !== null) clearTimeout(hoverTimer);
  }

  function manageFindTooltipSuppression(node: HTMLElement): { destroy: () => void } {
    const release = () => {
      if (!open) findTooltipSuppressed = false;
    };
    node.addEventListener("pointerenter", release);
    node.addEventListener("pointerleave", release);
    return {
      destroy: () => {
        node.removeEventListener("pointerenter", release);
        node.removeEventListener("pointerleave", release);
      },
    };
  }

  function setHighlighted(index: number): void {
    if (filtered.length === 0) return;
    highlighted = Math.max(0, Math.min(filtered.length - 1, index));
    previewKey = filtered[highlighted]?.rowKey ?? null;
    void tick().then(() => {
      const node = listEl;
      if (node === null) return;
      const row = node.querySelector<HTMLElement>(`[data-navigator-index="${highlighted}"]`);
      if (row === null) return;
      const listRect = node.getBoundingClientRect();
      const rowRect = row.getBoundingClientRect();
      if (rowRect.top < listRect.top) node.scrollTop -= listRect.top - rowRect.top;
      else if (rowRect.bottom > listRect.bottom) node.scrollTop += rowRect.bottom - listRect.bottom;
    });
  }

  function hoverEntry(index: number): void {
    highlighted = index;
    if (hoverTimer !== null) clearTimeout(hoverTimer);
    const key = filtered[index]?.rowKey ?? null;
    hoverTimer = setTimeout(() => {
      previewKey = key;
    }, 90);
  }

  function jumpTo(entry: NavigatorEntry): void {
    if (!canJump(entry)) return;
    jumpToRow(projectId, rosterIds, entry.agentIds, entry.rowKey);
    close();
  }

  function onSearchKeydown(event: KeyboardEvent): void {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setHighlighted(highlighted + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setHighlighted(highlighted - 1);
    } else if (event.key === "Enter") {
      const entry = filtered[highlighted];
      if (entry !== undefined) {
        event.preventDefault();
        jumpTo(entry);
      }
    }
    // Escape is handled by the Dialog (focus-trapped) — no branch here.
  }

  // Filter/sort changes invalidate the highlight (the list under it moved).
  $effect(() => {
    void query;
    void role;
    void descending;
    void pinnedOnly;
    highlighted = 0;
    void tick().then(() => {
      if (listEl !== null) listEl.scrollTop = 0;
    });
  });

  /// Toggle top/bottom fade masks by scroll position, so a scrollable region
  /// signals there's more above/below before the user tries to scroll. The
  /// mask is CSS (`data-fade-*` → `[mask-image]`); the action keeps the flags
  /// current on scroll and on content resize.
  function updateScrollFade(node: HTMLElement): void {
    const top = node.scrollTop > 4;
    const bottom = node.scrollTop + node.clientHeight < node.scrollHeight - 4;
    node.toggleAttribute("data-fade-top", top);
    node.toggleAttribute("data-fade-bottom", bottom);
  }

  function scrollFade(node: HTMLElement): { destroy: () => void } {
    const update = () => updateScrollFade(node);
    node.addEventListener("scroll", update, { passive: true });
    const ro = new ResizeObserver(update);
    ro.observe(node);
    update();
    return {
      destroy() {
        node.removeEventListener("scroll", update);
        ro.disconnect();
      },
    };
  }

  const ROLE_OPTIONS = [
    { label: "All", value: "all" },
    { label: "You", value: "user" },
    { label: "Agents", value: "agent" },
  ];
</script>

<Tooltip
  bind:open={findTooltipOpen}
  label="Find messages (⌘F)"
  side="bottom"
  disabled={open || findTooltipSuppressed}
  ignoreNonKeyboardFocus
>
  {#snippet trigger(props)}
    <button
      {...props}
      type="button"
      class={cn(ICON_BUTTON_CLASS, "shrink-0")}
      aria-label="Find messages"
      aria-expanded={open}
      data-testid="transcript-navigator-toggle"
      data-tauri-no-drag
      use:manageFindTooltipSuppression
      onclick={() => {
        findTooltipOpen = false;
        findTooltipSuppressed = true;
        navigatorState.open = true;
      }}
    >
      <TableOfContents size={16} aria-hidden="true" />
    </button>
  {/snippet}
</Tooltip>

<Dialog
  open={navigatorState.open}
  title="Messages"
  contentClass="w-[90vw] max-w-[1600px]"
  overlayClass="backdrop-blur-sm"
  onOpenAutoFocus={(event) => {
    event.preventDefault();
    onOpen();
  }}
  onClose={close}
>
  <div data-testid="transcript-navigator">
    <input
      bind:this={searchEl}
      bind:value={query}
      onkeydown={onSearchKeydown}
      type="text"
      autocorrect="off"
      autocapitalize="off"
      spellcheck="false"
      placeholder="Search messages…"
      aria-label="Search messages"
      data-testid="navigator-search"
      class="border-border bg-raised text-fg placeholder:text-muted focus-visible:ring-focus w-full rounded-md border px-2.5 py-1.5 text-sm focus-visible:ring-1 focus-visible:outline-none"
    />

    <div class="mt-2 flex items-center gap-2">
      <div class="w-56">
        <SegmentedSelect
          bind:value={role}
          options={ROLE_OPTIONS}
          ariaLabel="Filter by sender"
          testid="navigator-role"
          class="h-[26px] py-0"
        />
      </div>
      <Tooltip
        label={descending ? "Newest first" : "Oldest first"}
        side="bottom"
        reopen="fresh-hover"
      >
        {#snippet trigger(props)}
          <button
            {...props}
            type="button"
            class={cn(ICON_BUTTON_CLASS, "shrink-0")}
            aria-label={descending ? "Sort: newest first" : "Sort: oldest first"}
            data-testid="navigator-sort"
            onclick={() => (descending = !descending)}
          >
            {#if descending}
              <ArrowDownWideNarrow size={16} aria-hidden="true" />
            {:else}
              <ArrowUpWideNarrow size={16} aria-hidden="true" />
            {/if}
          </button>
        {/snippet}
      </Tooltip>
      <Tooltip
        label={pinnedOnly ? "Show all messages" : "Show pinned only"}
        side="bottom"
        reopen="fresh-hover"
      >
        {#snippet trigger(props)}
          <button
            {...props}
            type="button"
            class={cn(ICON_BUTTON_CLASS, "shrink-0", pinnedOnly && "text-accent bg-hover")}
            aria-label={pinnedOnly ? "Showing pinned messages" : "Show pinned messages only"}
            aria-pressed={pinnedOnly}
            disabled={!pinsReady}
            data-testid="navigator-pinned-filter"
            onclick={() => (pinnedOnly = !pinnedOnly)}
          >
            <Pin size={15} fill={pinnedOnly ? "currentColor" : "none"} aria-hidden="true" />
          </button>
        {/snippet}
      </Tooltip>
      <span class="text-muted ml-auto shrink-0 text-[11px]" data-testid="navigator-count">
        {#if indexStatus === "ready"}
          {filtered.length}
          {filtered.length === 1 ? "message" : "messages"}
        {:else}
          Preparing…
        {/if}
      </span>
    </div>

    {#if indexStatus !== "ready"}
      <div
        class="text-muted mt-2 flex h-[70vh] flex-col items-center justify-center gap-3 text-sm"
        role="status"
        aria-live="polite"
        data-testid="navigator-loading"
      >
        <Spinner class="h-6 w-6" />
        <span>Preparing messages…</span>
      </div>
    {:else}
      <div class="mt-2 flex h-[70vh] gap-3" data-testid="navigator-ready">
        <div
          bind:this={listEl}
          use:scrollFade
          class="navigator-fade w-2/5 shrink-0 overflow-y-auto pr-1"
          role="listbox"
          aria-label="Messages"
          data-testid="navigator-list"
        >
          {#each filtered as entry, index (entry.rowKey)}
            {@const disabled = !canJump(entry)}
            {@const pinnableIdentity =
              entry.messageIdentity.kind === "pinnable" ? entry.messageIdentity : undefined}
            {@const pinned =
              pinnableIdentity !== undefined &&
              pinsReady &&
              isMessagePinned(projectId, pinnableIdentity)}
            {#snippet entryButton(extraProps: Record<string, unknown> = {})}
              <button
                {...extraProps}
                type="button"
                class={cn(
                  "block min-w-0 flex-1 rounded-md px-2.5 py-1.5 text-left outline-none select-none",
                  disabled ? "cursor-default opacity-40" : "cursor-pointer",
                  index === highlighted && "bg-hover",
                )}
                role="option"
                aria-selected={index === highlighted}
                aria-disabled={disabled}
                aria-posinset={index + 1}
                aria-setsize={filtered.length}
                data-testid="navigator-entry"
                data-row-key={entry.rowKey}
                onmousemove={() => hoverEntry(index)}
                onclick={() => jumpTo(entry)}
              >
                <span class="flex items-baseline gap-2">
                  <!-- User and agent attributions share one weight/color so an
                     agent name is as easy to spot at a glance as "You". -->
                  <span class="text-fg shrink-0 text-xs font-medium">{entry.attribution}</span>
                  <span class="text-muted min-w-0 flex-1 truncate text-xs">
                    {entry.preview === "" ? "—" : entry.preview}
                  </span>
                  <span class="text-muted/70 shrink-0 font-mono text-[10px]">
                    {relativeTime(entry.at)}
                  </span>
                </span>
              </button>
            {/snippet}
            <div
              class={cn("flex h-7 items-center rounded-md", index === highlighted && "bg-hover")}
              data-navigator-index={index}
            >
              {#if disabled}
                <Tooltip label={`${entry.attribution} isn't visible in any pane`} side="right">
                  {#snippet trigger(props)}
                    {@render entryButton(props)}
                  {/snippet}
                </Tooltip>
              {:else}
                {@render entryButton()}
              {/if}
              {#if pinnableIdentity !== undefined && pinsReady}
                <Tooltip
                  label={pinned ? "Unpin message" : "Pin message"}
                  side="left"
                  reopen="fresh-hover"
                >
                  {#snippet trigger(props)}
                    <button
                      {...props}
                      type="button"
                      class={cn(
                        "hover:bg-control-hover mr-1 flex h-7 w-7 shrink-0 items-center justify-center rounded-full",
                        pinned ? "text-accent" : "text-muted hover:text-fg",
                      )}
                      aria-label={pinned ? "Unpin message" : "Pin message"}
                      aria-pressed={pinned}
                      data-testid="navigator-entry-pin"
                      onclick={() => toggleMessagePin(projectId, pinnableIdentity)}
                    >
                      <Pin size={14} fill={pinned ? "currentColor" : "none"} aria-hidden="true" />
                    </button>
                  {/snippet}
                </Tooltip>
              {:else}
                {@const unavailableReason =
                  pinnableIdentity !== undefined
                    ? pinsUnavailableReason(projectId)
                    : entry.messageIdentity.kind === "unsupported"
                      ? entry.messageIdentity.reason
                      : "Pin unavailable"}
                <Tooltip label={unavailableReason} side="left">
                  {#snippet trigger(props)}
                    <span
                      {...props}
                      class="text-muted/50 mr-1 flex h-7 w-7 shrink-0 items-center justify-center rounded-full"
                      role="button"
                      aria-label={unavailableReason}
                      aria-disabled="true"
                      tabindex="0"
                      data-testid="navigator-entry-pin-unavailable"
                    >
                      <Pin size={14} aria-hidden="true" />
                    </span>
                  {/snippet}
                </Tooltip>
              {/if}
            </div>
          {/each}
          {#if filtered.length === 0}
            <div class="text-muted px-2.5 py-3 text-sm select-none" data-testid="navigator-empty">
              {entries.length === 0 ? "No messages yet." : "No matches."}
            </div>
          {/if}
        </div>

        <div
          bind:this={previewEl}
          use:scrollFade
          class="navigator-fade bg-panel min-w-0 flex-1 overflow-y-auto rounded-md px-3 py-2"
          data-testid="navigator-preview"
        >
          {#if previewEntry !== undefined}
            {@const preview = previewProse(previewEntry)}
            <div class="text-muted mb-1.5 flex items-baseline justify-between gap-2 text-[11px]">
              <span class="font-medium">{previewEntry.attribution}</span>
              <span class="font-mono">{relativeTime(previewEntry.at)}</span>
            </div>
            {#if preview === ""}
              <p class="text-muted text-xs">No text content.</p>
            {:else}
              <div class="navigator-preview-prose text-sm">
                <Markdown text={preview} />
              </div>
            {/if}
          {:else}
            <p class="text-muted/70 mt-1 text-xs select-none">
              Hover or arrow-key a message to preview it here.
            </p>
          {/if}
        </div>
      </div>
    {/if}
  </div>
</Dialog>

<style>
  /* Fade the scrollable edge that has more content, cued by `data-fade-*`
     (set by the `scrollFade` action). Bottom-only, top-only, or both. */
  .navigator-fade[data-fade-bottom]:not([data-fade-top]) {
    mask-image: linear-gradient(to bottom, black calc(100% - 2.5rem), transparent);
  }
  .navigator-fade[data-fade-top]:not([data-fade-bottom]) {
    mask-image: linear-gradient(to bottom, transparent, black 2.5rem);
  }
  .navigator-fade[data-fade-top][data-fade-bottom] {
    mask-image: linear-gradient(
      to bottom,
      transparent,
      black 2.5rem,
      black calc(100% - 2.5rem),
      transparent
    );
  }
  /* Tighten the markdown preview's default block spacing for a dense panel. */
  .navigator-preview-prose :global(p) {
    margin: 0 0 0.5rem;
  }
</style>
