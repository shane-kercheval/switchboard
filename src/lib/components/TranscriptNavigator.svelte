<script lang="ts">
  /// The transcript navigator: a centered overlay (the command-palette idiom —
  /// a focus-trapped modal over a dimmed, blurred transcript) listing every
  /// message in the conversation with type-to-filter, a role filter, a sort
  /// toggle, and a live preview panel to the list's right. Clicking or ↵ jumps
  /// the owning pane's transcript to that message (`jumpToRow` handles pane
  /// reveal + window re-pin). Entries derive from the row model, never the DOM
  /// — the transcript is render-windowed. Opened by the header button, ⌘F, or
  /// the command palette (all via `navigatorState`).
  import { tick } from "svelte";
  import { ArrowDownWideNarrow, ArrowUpWideNarrow, TableOfContents } from "@lucide/svelte";
  import { cn, relativeTime } from "$lib/utils";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";
  import Dialog from "$lib/components/ui/Dialog.svelte";
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
  import { jumpToRow, navigatorState, resolveJumpPane } from "$lib/state/transcriptJump.svelte";
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
  let highlighted = $state(0);
  /// The entry whose full text shows in the preview panel. Keyboard moves set
  /// it immediately (the preview follows the highlight); hover sets it through
  /// a short debounce so running the cursor down the list doesn't flash panels.
  let previewKey = $state<string | null>(null);
  let hoverTimer: ReturnType<typeof setTimeout> | null = null;
  let searchEl = $state<HTMLInputElement | null>(null);
  let listEl = $state<HTMLElement | null>(null);

  const open = $derived(navigatorState.open);
  const rosterIds = $derived(agents.map((a) => a.id));

  /// Index only while open — the derivation walks every turn, and a closed
  /// navigator shouldn't pay it on each streamed chunk.
  const entries = $derived.by(() => {
    if (!open) return [];
    const turns: Turn[] = [];
    for (const agent of agents) {
      for (const turn of transcripts[agent.id] ?? []) turns.push(turn);
    }
    const rows = buildUnifiedRows(turns, overlay, new Set(rosterIds));
    return buildNavigatorEntries(rows, new Map(agents.map((a) => [a.id, a.name])));
  });
  const filtered = $derived.by(() => {
    const matched = filterEntries(entries, query, role as NavigatorRoleFilter);
    // The index is chronological (oldest first); descending shows newest first.
    return descending ? [...matched].reverse() : matched;
  });

  /// Entries whose message renders in no visible pane (agent unassigned or
  /// eye-hidden) can't be jumped to; they render disabled with a tooltip.
  function jumpTarget(entry: NavigatorEntry): string | null {
    return resolveJumpPane(projectId, rosterIds, entry.agentIds);
  }

  const PREVIEW_CAP = 1500;

  function previewProse(entry: NavigatorEntry): { text: string; truncated: boolean } {
    const source = entry.prose.trim() === "" ? entry.preview : entry.prose;
    if (source.length <= PREVIEW_CAP) return { text: source, truncated: false };
    return { text: source.slice(0, PREVIEW_CAP), truncated: true };
  }

  const previewEntry = $derived(
    previewKey === null ? undefined : filtered.find((e) => e.rowKey === previewKey),
  );

  function onOpen(): void {
    query = "";
    role = "all";
    descending = true;
    highlighted = 0;
    previewKey = null;
    void tick().then(() => searchEl?.focus());
  }

  function close(): void {
    navigatorState.open = false;
    if (hoverTimer !== null) clearTimeout(hoverTimer);
  }

  function setHighlighted(index: number): void {
    if (filtered.length === 0) return;
    highlighted = Math.max(0, Math.min(filtered.length - 1, index));
    previewKey = filtered[highlighted]?.rowKey ?? null;
    void tick().then(() => {
      listEl
        ?.querySelector(`[data-testid="navigator-entry"]:nth-child(${highlighted + 1})`)
        ?.scrollIntoView({ block: "nearest" });
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
    if (jumpTarget(entry) === null) return;
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
    highlighted = 0;
  });

  /// Toggle top/bottom fade masks by scroll position, so a scrollable region
  /// signals there's more above/below before the user tries to scroll. The
  /// mask is CSS (`data-fade-*` → `[mask-image]`); the action keeps the flags
  /// current on scroll and on content resize.
  function scrollFade(node: HTMLElement): { destroy: () => void } {
    function update(): void {
      const top = node.scrollTop > 4;
      const bottom = node.scrollTop + node.clientHeight < node.scrollHeight - 4;
      node.toggleAttribute("data-fade-top", top);
      node.toggleAttribute("data-fade-bottom", bottom);
    }
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

<Tooltip label="Find messages (⌘F)" side="bottom">
  {#snippet trigger(props)}
    <button
      {...props}
      type="button"
      class={cn(ICON_BUTTON_CLASS, "hover:bg-panel shrink-0")}
      aria-label="Find messages"
      aria-expanded={open}
      data-testid="transcript-navigator-toggle"
      data-tauri-no-drag
      onclick={() => (navigatorState.open = true)}
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
        />
      </div>
      <Tooltip label={descending ? "Newest first" : "Oldest first"} side="bottom">
        {#snippet trigger(props)}
          <button
            {...props}
            type="button"
            class={cn(ICON_BUTTON_CLASS, "hover:bg-panel shrink-0")}
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
      <span class="text-muted ml-auto shrink-0 text-[11px]" data-testid="navigator-count">
        {filtered.length}
        {filtered.length === 1 ? "message" : "messages"}
      </span>
    </div>

    <div class="mt-2 flex h-[70vh] gap-3">
      <div
        bind:this={listEl}
        use:scrollFade
        class="navigator-fade w-2/5 shrink-0 overflow-y-auto pr-1"
        role="listbox"
        aria-label="Messages"
        data-testid="navigator-list"
      >
        {#each filtered as entry, index (entry.rowKey)}
          {@const disabled = jumpTarget(entry) === null}
          {#snippet entryButton(extraProps: Record<string, unknown> = {})}
            <button
              {...extraProps}
              type="button"
              class={cn(
                "block w-full rounded-md px-2.5 py-1.5 text-left outline-none select-none",
                disabled ? "cursor-default opacity-40" : "cursor-pointer",
                index === highlighted && "bg-hover",
              )}
              role="option"
              aria-selected={index === highlighted}
              aria-disabled={disabled}
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
          {#if disabled}
            <Tooltip label={`${entry.attribution} isn't visible in any pane`} side="right">
              {#snippet trigger(props)}
                {@render entryButton(props)}
              {/snippet}
            </Tooltip>
          {:else}
            {@render entryButton()}
          {/if}
        {/each}
        {#if filtered.length === 0}
          <div class="text-muted px-2.5 py-3 text-sm select-none" data-testid="navigator-empty">
            {entries.length === 0 ? "No messages yet." : "No matches."}
          </div>
        {/if}
      </div>

      <div
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
          {#if preview.text === ""}
            <p class="text-muted text-xs">No text content.</p>
          {:else}
            <div class="navigator-preview-prose text-sm">
              <Markdown text={preview.text} />
            </div>
          {/if}
          {#if preview.truncated}
            <p class="text-muted mt-2 text-[11px]" data-testid="navigator-preview-truncated">
              Preview truncated — jump to read the full message.
            </p>
          {/if}
        {:else}
          <p class="text-muted/70 mt-1 text-xs select-none">
            Hover or arrow-key a message to preview it here.
          </p>
        {/if}
      </div>
    </div>
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
