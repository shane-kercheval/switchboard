<script lang="ts">
  import { Clock3, LocateFixed, Pin } from "@lucide/svelte";
  import { tick } from "svelte";
  import { SvelteMap, SvelteSet } from "svelte/reactivity";
  import { ICON_BUTTON_ON_PANEL_CLASS } from "$lib/components/ui/iconButton";
  import type { AgentRecord, ConversationItem, ProjectId } from "$lib/types";
  import { transcripts, type Turn } from "$lib/state/index.svelte";
  import { buildUnifiedRows, copyTextOf, type UnifiedRow } from "$lib/state/unified";
  import { navigatorEntryForRow, type NavigatorEntry } from "$lib/transcriptIndex";
  import {
    identityKeys,
    messageIdentityForRow,
    type PinnableMessageIdentity,
  } from "$lib/messageIdentity";
  import {
    loadMessagePins,
    pinsScrollTopFor,
    isPinCollapsed,
    pinsLoaded,
    pinsFor,
    pinsUnavailableReason,
    setPinsCollapsed,
    setPinsScrollTop,
    setStoredPinPinned,
    togglePinCollapsed,
  } from "$lib/state/messagePins.svelte";
  import {
    buildJumpPaneIndex,
    canResolveJumpFromIndex,
    jumpToRow,
  } from "$lib/state/transcriptJump.svelte";
  import {
    layout,
    PINS_SIDEBAR_DEFAULT_WIDTH,
    rightSidebarMaxWidth,
    SIDEBAR_MIN_WIDTH,
  } from "$lib/layout.svelte";
  import { cn, compareIsoTimestampsDescending, relativeTime } from "$lib/utils";
  import { HARNESS_COLOR } from "$lib/harnessDisplay";
  import { agentCopy } from "$lib/agentCopy.svelte";
  import AgentMessageBody from "$lib/components/AgentMessageBody.svelte";
  import UserMessageBody from "$lib/components/UserMessageBody.svelte";
  import CopyButton from "$lib/components/ui/CopyButton.svelte";
  import ExpandCollapseIcon from "$lib/components/ui/ExpandCollapseIcon.svelte";
  import ResizeHandle from "$lib/components/ui/ResizeHandle.svelte";
  import SidebarPanel from "$lib/components/ui/SidebarPanel.svelte";
  import SidebarSection from "$lib/components/ui/SidebarSection.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import {
    SEGMENTED_MAIN_CONTAINER_CLASS,
    SEGMENTED_MAIN_ITEM_ACTIVE_CLASS,
    SEGMENTED_MAIN_ITEM_CLASS,
    SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
  } from "$lib/components/ui/segmentedControl";

  let {
    projectId,
    agents,
    overlay = [],
  }: {
    projectId: ProjectId;
    agents: AgentRecord[];
    overlay?: ConversationItem[];
  } = $props();

  let draftWidth = $state<number | null>(null);
  let scrollElement = $state<HTMLDivElement>();
  const pinsSortMode = $derived(layout.pinsSortModeFor(projectId));
  const rosterIds = $derived(agents.map((agent) => agent.id));
  const jumpPaneIndex = $derived(buildJumpPaneIndex(projectId, rosterIds));
  const agentById = $derived(new Map(agents.map((agent) => [agent.id, agent])));
  const agentNames = $derived(new Map(agents.map((agent) => [agent.id, agent.name])));
  const agentHarnesses = $derived(new Map(agents.map((agent) => [agent.id, agent.harness])));
  const rows = $derived.by(() => {
    const turns: Turn[] = [];
    for (const agent of agents) turns.push(...(transcripts[agent.id] ?? []));
    return buildUnifiedRows(turns, overlay, new Set(rosterIds));
  });
  const messageRows = $derived.by(() =>
    rows
      .filter(
        (row): row is Extract<UnifiedRow, { kind: "user" | "agent" }> =>
          row.kind === "user" || row.kind === "agent",
      )
      .map((row) => ({
        row,
        identity: messageIdentityForRow(
          row,
          row.kind === "agent" ? agentHarnesses.get(row.turn.agent_id) : undefined,
        ),
      })),
  );
  const continuationsByParent = $derived.by(() => {
    const index = new SvelteMap<string, Extract<UnifiedRow, { kind: "agent" }>[]>();
    for (const item of messageRows) {
      const row = item.row;
      if (row.kind !== "agent" || row.turn.continuation_of === undefined) continue;
      const key = `${row.turn.agent_id}\u0000${row.turn.continuation_of}`;
      const continuations = index.get(key) ?? [];
      continuations.push(row);
      index.set(key, continuations);
    }
    return index;
  });

  function withContinuationContent(
    root: Extract<UnifiedRow, { kind: "agent" }>,
  ): Extract<UnifiedRow, { kind: "agent" }> {
    if (root.turn.hydration_key === undefined) return root;
    const items = [...root.turn.items];
    const visited = new SvelteSet([root.turn.hydration_key]);
    let tail = root;
    while (tail.turn.hydration_key !== undefined) {
      const continuations =
        continuationsByParent.get(`${root.turn.agent_id}\u0000${tail.turn.hydration_key}`) ?? [];
      if (continuations.length !== 1) break;
      const next = continuations[0]!;
      if (next.turn.hydration_key === undefined || visited.has(next.turn.hydration_key)) break;
      visited.add(next.turn.hydration_key);
      items.push(...next.turn.items);
      tail = next;
    }
    if (tail === root) return root;
    return {
      ...root,
      turn: {
        ...root.turn,
        items,
        status: tail.turn.status,
        ended_at: tail.turn.ended_at ?? root.turn.ended_at,
        model: tail.turn.model ?? root.turn.model,
        effort: tail.turn.effort ?? root.turn.effort,
      },
    };
  }

  const pinnedItems = $derived.by(() => {
    const pins = [...pinsFor(projectId)];
    const wanted = new Set(pins.map((pin) => pin.key));
    const resolved = new SvelteMap<
      string,
      {
        row: Extract<UnifiedRow, { kind: "user" | "agent" }>;
        identity: PinnableMessageIdentity;
      } | null
    >();
    for (const item of messageRows) {
      if (item.identity.kind !== "pinnable") continue;
      for (const key of identityKeys(item.identity)) {
        if (!wanted.has(key)) continue;
        const displayRow = item.row.kind === "agent" ? withContinuationContent(item.row) : item.row;
        resolved.set(key, resolved.has(key) ? null : { row: displayRow, identity: item.identity });
      }
    }
    const items = pins.map((pin) => {
      const match = resolved.get(pin.key);
      if (match == null) return { pin, entry: undefined, row: undefined };
      return {
        pin,
        row: match.row,
        entry: navigatorEntryForRow(match.row, agentNames, agentHarnesses),
      };
    });
    return items.sort((a, b) => {
      if (pinsSortMode === "message_at") {
        if (a.entry === undefined && b.entry !== undefined) return 1;
        if (a.entry !== undefined && b.entry === undefined) return -1;
        if (a.entry !== undefined && b.entry !== undefined) {
          const byMessageTime = compareIsoTimestampsDescending(a.entry.at, b.entry.at);
          if (byMessageTime !== 0) return byMessageTime;
        }
      }
      const byPinnedTime = compareIsoTimestampsDescending(a.pin.pinned_at, b.pin.pinned_at);
      return byPinnedTime !== 0 ? byPinnedTime : a.pin.key.localeCompare(b.pin.key);
    });
  });
  const collapsiblePinKeys = $derived(
    pinnedItems
      .filter((item) => item.row?.kind === "user" || item.row?.kind === "agent")
      .map((item) => item.pin.key),
  );
  const showPinsSort = $derived(
    pinnedItems.length > 1 && pinnedItems.some((item) => item.entry !== undefined),
  );
  const allPinsCollapsed = $derived(
    collapsiblePinKeys.length > 0 &&
      collapsiblePinKeys.every((key) => isPinCollapsed(projectId, key)),
  );

  $effect(() => {
    void loadMessagePins(projectId);
  });

  $effect(() => {
    const element = scrollElement;
    const currentProjectId = projectId;
    if (element === undefined || !pinsLoaded(currentProjectId)) return;
    const scrollTop = pinsScrollTopFor(currentProjectId);
    void tick().then(() => {
      if (scrollElement === element && projectId === currentProjectId) {
        element.scrollTop = scrollTop;
      }
    });
  });

  function rememberScroll(scrollTop: number): void {
    if (pinsLoaded(projectId)) setPinsScrollTop(projectId, scrollTop);
  }

  function canJump(entry: NavigatorEntry): boolean {
    return canResolveJumpFromIndex(jumpPaneIndex, entry.agentIds);
  }

  function jump(entry: NavigatorEntry): void {
    jumpToRow(projectId, rosterIds, entry.agentIds, entry.rowKey);
  }

  function copyableText(row: UnifiedRow): string {
    if (row.kind === "user") return row.text;
    if (row.kind === "agent") return copyTextOf(row.turn, agentCopy.mode);
    return "";
  }

  function agentBorderColor(row: Extract<UnifiedRow, { kind: "agent" }>): string {
    const harness = agentById.get(row.turn.agent_id)?.harness;
    return harness === undefined ? "var(--border)" : HARNESS_COLOR[harness];
  }
</script>

<SidebarPanel
  side="right"
  widthProfile="reading"
  width={draftWidth ?? layout.pinsSidebarWidth}
  testid="pins-sidebar"
>
  <ResizeHandle
    value={() => draftWidth ?? layout.pinsSidebarWidth}
    min={SIDEBAR_MIN_WIDTH}
    max={rightSidebarMaxWidth}
    edge="start"
    label="Resize pins sidebar"
    testid="pins-sidebar-resizer"
    class="hover:bg-focus absolute inset-y-0 left-0 z-10 w-1 transition-colors"
    onDraft={(px) => (draftWidth = px)}
    onCommit={(px) => {
      layout.pinsSidebarWidth = px;
      draftWidth = null;
    }}
    onReset={() => {
      layout.pinsSidebarWidth = PINS_SIDEBAR_DEFAULT_WIDTH;
      draftWidth = null;
    }}
  />
  <SidebarSection
    title="Pins"
    bind:scrollRef={scrollElement}
    scrollTestid="pins-scroll"
    onScroll={rememberScroll}
  >
    {#snippet action()}
      <div class="flex items-center gap-1">
        {#if showPinsSort}
          <div
            class={cn(SEGMENTED_MAIN_CONTAINER_CLASS, "flex")}
            role="radiogroup"
            aria-label="Sort pinned messages"
            data-testid="pins-sort"
          >
            <Tooltip label="Recently pinned" side="bottom" reopen="fresh-hover">
              {#snippet trigger(props)}
                <button
                  {...props}
                  type="button"
                  role="radio"
                  class={cn(
                    SEGMENTED_MAIN_ITEM_CLASS,
                    pinsSortMode === "pinned_at"
                      ? SEGMENTED_MAIN_ITEM_ACTIVE_CLASS
                      : SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
                  )}
                  aria-label="Sort by recently pinned"
                  aria-checked={pinsSortMode === "pinned_at"}
                  data-testid="pins-sort-pinned"
                  onclick={() => layout.setPinsSortMode(projectId, "pinned_at")}
                >
                  <Pin
                    size={13}
                    fill={pinsSortMode === "pinned_at" ? "currentColor" : "none"}
                    aria-hidden="true"
                  />
                </button>
              {/snippet}
            </Tooltip>
            <Tooltip label="Newest messages" side="bottom" reopen="fresh-hover">
              {#snippet trigger(props)}
                <button
                  {...props}
                  type="button"
                  role="radio"
                  class={cn(
                    SEGMENTED_MAIN_ITEM_CLASS,
                    pinsSortMode === "message_at"
                      ? SEGMENTED_MAIN_ITEM_ACTIVE_CLASS
                      : SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
                  )}
                  aria-label="Sort by newest messages"
                  aria-checked={pinsSortMode === "message_at"}
                  data-testid="pins-sort-message"
                  onclick={() => layout.setPinsSortMode(projectId, "message_at")}
                >
                  <Clock3 size={13} aria-hidden="true" />
                </button>
              {/snippet}
            </Tooltip>
          </div>
        {/if}
        {#if collapsiblePinKeys.length > 1}
          <Tooltip
            label={allPinsCollapsed ? "Expand all pinned messages" : "Collapse all pinned messages"}
            side="bottom"
            reopen="fresh-hover"
          >
            {#snippet trigger(props)}
              <button
                {...props}
                type="button"
                class={ICON_BUTTON_ON_PANEL_CLASS}
                aria-label={allPinsCollapsed
                  ? "Expand all pinned messages"
                  : "Collapse all pinned messages"}
                data-testid="pins-toggle-all"
                onclick={() => setPinsCollapsed(projectId, collapsiblePinKeys, !allPinsCollapsed)}
              >
                <ExpandCollapseIcon expanded={!allPinsCollapsed} size={14} />
              </button>
            {/snippet}
          </Tooltip>
        {/if}
      </div>
    {/snippet}
    {#if !pinsLoaded(projectId)}
      <div class="text-muted px-3 py-4 text-sm" data-testid="pins-loading">
        {pinsUnavailableReason(projectId)}
      </div>
    {:else if pinnedItems.length === 0}
      <div
        class="text-muted flex flex-col items-center px-5 py-8 text-center"
        data-testid="pins-empty"
      >
        <div
          class="bg-surface text-accent mb-3 flex h-9 w-9 items-center justify-center rounded-full"
          data-testid="pins-empty-icon"
        >
          <Pin size={17} fill="currentColor" aria-hidden="true" />
        </div>
        <p class="text-fg text-sm font-semibold">Keep important messages close</p>
        <p class="mt-1 max-w-64 text-xs leading-5">
          Pinned messages appear here in full, so you can read them without scrolling through the
          transcript. Pin a message from its footer or from Find Messages.
        </p>
      </div>
    {:else}
      <div class="space-y-2 px-2 pb-2" data-testid="pins-list">
        {#each pinnedItems as item (item.pin.key)}
          <div
            class="border-border/80 bg-raised overflow-hidden rounded-lg border"
            data-testid="pinned-message-card"
            data-message-key={item.pin.key}
          >
            <div
              class="bg-surface border-border/60 flex min-h-9 items-center gap-1 border-b px-1.5"
              data-testid="pinned-message-header"
            >
              {#if item.entry !== undefined}
                {@const collapsible = item.row?.kind === "user" || item.row?.kind === "agent"}
                <Tooltip
                  label={collapsible
                    ? isPinCollapsed(projectId, item.pin.key)
                      ? "Expand message"
                      : "Collapse message"
                    : "Message unavailable"}
                  side="right"
                >
                  {#snippet trigger(props)}
                    <button
                      {...props}
                      type="button"
                      class={cn(
                        "min-w-0 flex-1 px-1.5 py-1 text-left",
                        collapsible ? "cursor-pointer" : "cursor-default opacity-45",
                      )}
                      aria-disabled={!collapsible}
                      aria-expanded={collapsible
                        ? !isPinCollapsed(projectId, item.pin.key)
                        : undefined}
                      data-testid="pinned-message"
                      onclick={() => collapsible && togglePinCollapsed(projectId, item.pin.key)}
                    >
                      <span class="text-fg block truncate text-xs font-semibold">
                        {item.entry.attribution}
                      </span>
                      <span class="text-muted/70 block font-mono text-[10px]">
                        {relativeTime(item.entry.at)}
                      </span>
                    </button>
                  {/snippet}
                </Tooltip>
              {:else}
                <div
                  class="text-muted min-w-0 flex-1 px-1.5 py-2 text-xs"
                  data-testid="pinned-missing"
                >
                  Message unavailable
                </div>
              {/if}
              {#if item.entry !== undefined && canJump(item.entry)}
                <Tooltip label="Go to message" side="bottom">
                  {#snippet trigger(props)}
                    <button
                      {...props}
                      type="button"
                      class="text-muted hover:text-fg hover:bg-active flex h-7 w-7 shrink-0 items-center justify-center rounded-full"
                      aria-label="Go to message"
                      data-testid="pinned-message-locate"
                      onclick={() => jump(item.entry!)}
                    >
                      <LocateFixed size={14} aria-hidden="true" />
                    </button>
                  {/snippet}
                </Tooltip>
              {/if}
              {#if item.row?.kind === "user" || item.row?.kind === "agent"}
                {@const copyable = copyableText(item.row)}
                {#if copyable !== ""}
                  <CopyButton
                    text={copyable}
                    label="Copy pinned message"
                    testid="pinned-message-copy"
                  />
                {/if}
                <Tooltip
                  label={isPinCollapsed(projectId, item.pin.key)
                    ? "Expand message"
                    : "Collapse message"}
                  side="bottom"
                  reopen="fresh-hover"
                >
                  {#snippet trigger(props)}
                    <button
                      {...props}
                      type="button"
                      class="text-muted hover:text-fg hover:bg-active flex h-7 w-7 shrink-0 items-center justify-center rounded-full"
                      aria-label={isPinCollapsed(projectId, item.pin.key)
                        ? "Expand message"
                        : "Collapse message"}
                      aria-expanded={!isPinCollapsed(projectId, item.pin.key)}
                      data-testid="pinned-message-toggle"
                      onclick={() => togglePinCollapsed(projectId, item.pin.key)}
                    >
                      <ExpandCollapseIcon
                        expanded={!isPinCollapsed(projectId, item.pin.key)}
                        size={14}
                      />
                    </button>
                  {/snippet}
                </Tooltip>
              {/if}
              <Tooltip label="Unpin message" side="left">
                {#snippet trigger(props)}
                  <button
                    {...props}
                    type="button"
                    class="text-accent hover:bg-active flex h-7 w-7 shrink-0 items-center justify-center rounded-full"
                    aria-label="Unpin message"
                    data-testid="pinned-message-unpin"
                    onclick={() => setStoredPinPinned(projectId, item.pin.key, false)}
                  >
                    <Pin size={15} fill="currentColor" aria-hidden="true" />
                  </button>
                {/snippet}
              </Tooltip>
            </div>
            {#if item.row?.kind === "user" || item.row?.kind === "agent"}
              {#if isPinCollapsed(projectId, item.pin.key)}
                <p
                  class="text-muted truncate px-3 py-2 text-xs"
                  data-testid="pinned-message-preview"
                >
                  {item.entry?.preview === "" ? "No text content" : item.entry?.preview}
                </p>
              {:else}
                <div class="bg-raised px-3 py-2.5 text-sm" data-testid="pinned-message-body">
                  {#if item.row.kind === "user"}
                    <div class="rounded-lg px-3 py-2">
                      <UserMessageBody row={item.row} />
                    </div>
                  {:else}
                    <div
                      class="space-y-1.5 border-l-[0.5px] pl-3"
                      style:border-left-color={agentBorderColor(item.row)}
                    >
                      <AgentMessageBody
                        turn={item.row.turn}
                        settled={item.row.turn.status !== "streaming"}
                      />
                    </div>
                  {/if}
                </div>
              {/if}
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </SidebarSection>
</SidebarPanel>
