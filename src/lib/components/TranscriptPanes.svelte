<script lang="ts">
  /// The transcript pane row: 1..N side-by-side `UnifiedTranscript` instances
  /// showing assigned subsets of the project's roster, with resizable gutters and pane
  /// targeting. With a single pane (the default), every piece of pane chrome —
  /// header, gutters, coverage border, Cmd overlay, targeting gestures — is
  /// inert, so the no-split state renders exactly like the pre-pane UI: with
  /// one pane there is nothing to disambiguate, and the chrome would be pure
  /// noise in the most common state.
  ///
  /// Targeting is a lens over the compose recipient set (`recipientSelection`):
  /// gestures *write* `targetRecipients` (the lock-aware user-targeting path);
  /// the coverage border *derives* from the selection ∩ membership. Nothing
  /// here stores a targeted pane — a stored target could drift from the real
  /// recipient set and lie.
  import { Check, Maximize2, Minimize2, MoreHorizontal, Pencil, X } from "@lucide/svelte";
  import type { AgentRecord, ConversationItem, ProjectId } from "$lib/types";
  import UnifiedTranscript from "$lib/components/UnifiedTranscript.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";
  import { cn } from "$lib/utils";
  import { shortcut } from "$lib/platform";
  import { agentIsWorking, runtimes } from "$lib/state/index.svelte";
  import {
    MIN_PANE_WIDTH_PX,
    closePane,
    layoutFor,
    maximizePane,
    minimizePane,
    renamePane,
    restoreMaximizedPane,
    setFractions,
    setPaneRowWidth,
    showAllInPane,
    unassignedAgentIds,
    unassignAgentFromPane,
    moveAgentToPane,
    type TranscriptPane,
  } from "$lib/state/transcriptPanes.svelte";
  import { selectionFor, targetRecipients } from "$lib/state/recipientSelection.svelte";

  let {
    projectId,
    agents,
    overlay = [],
    loadStatus = "complete",
    loadError,
    onRetryLoad,
  }: {
    projectId: ProjectId;
    agents: AgentRecord[];
    overlay?: ConversationItem[];
    loadStatus?: "pending" | "loading" | "complete" | "failed";
    loadError?: string;
    onRetryLoad?: () => void;
  } = $props();

  const rosterIds = $derived(agents.map((a) => a.id));
  const layout = $derived(layoutFor(projectId, rosterIds));
  const multiPane = $derived(layout.panes.length > 1);
  const selection = $derived(selectionFor(projectId));
  const unassignedIds = $derived(unassignedAgentIds(projectId, rosterIds));
  const paneChrome = $derived(multiPane || unassignedIds.length > 0);
  const minimizedIds = $derived(new Set(layout.minimized));
  const maximizedPane = $derived(
    layout.maximized === null
      ? null
      : (layout.panes.find((pane) => pane.id === layout.maximized) ?? null),
  );
  const renderPanes = $derived.by(() => {
    const panes =
      maximizedPane !== null
        ? [maximizedPane]
        : layout.panes.filter((pane) => !minimizedIds.has(pane.id));
    return panes.map((pane) => ({
      pane,
      originalIndex: layout.panes.findIndex((p) => p.id === pane.id),
    }));
  });

  /// A pane's visible roster, in roster order (membership decides *where* an
  /// agent appears; the pane's hidden set decides *whether*). Roster order
  /// keeps pane columns consistent with the sidebar and fan-out columns.
  function paneAgents(pane: TranscriptPane): AgentRecord[] {
    return agents.filter((a) => pane.members.includes(a.id) && !pane.hidden.includes(a.id));
  }

  function paneMemberAgents(pane: TranscriptPane): AgentRecord[] {
    return agents.filter((a) => pane.members.includes(a.id));
  }

  function paneIsActive(pane: TranscriptPane): boolean {
    return pane.members.some((id) => agentIsWorking(runtimes[id]));
  }

  /// Tri-state recipient coverage: how much of this pane the current draft
  /// targets. Derived from the recipient set every render, so the border can
  /// never disagree with who actually receives the send.
  function coverage(pane: TranscriptPane): "full" | "partial" | "none" {
    if (pane.members.length === 0) return "none";
    const selected = new Set(selection);
    const count = pane.members.filter((id) => selected.has(id)).length;
    if (count === 0) return "none";
    return count === pane.members.length ? "full" : "partial";
  }

  /// Replace the recipient set with the pane's members — the meaning of every
  /// targeting gesture (header click, Cmd+click, `@panename`, Cmd+Alt+N).
  /// An empty pane is not a send target anywhere: targeting it could only
  /// clear the recipient set, silently. Goes through `targetRecipients` so the
  /// prompt-render targeting freeze applies (see recipientSelection).
  function targetPane(pane: TranscriptPane): void {
    if (pane.members.length === 0) return;
    targetRecipients(projectId, [...pane.members]);
  }

  function hasOnlyMeta(event: MouseEvent | KeyboardEvent): boolean {
    const meta = event.metaKey || (event instanceof KeyboardEvent && event.key === "Meta");
    return meta && !event.altKey && !event.shiftKey && !event.ctrlKey;
  }

  /// Cmd+click anywhere in a pane targets it (multi-pane only). Plain clicks
  /// never re-target — reading (scroll, select, copy) must stay safe while a
  /// draft is half-typed elsewhere. Plain ⌘ only, never with Ctrl/Alt/Shift:
  /// on macOS Ctrl+click is the system context-menu gesture, and modified Cmd
  /// chords already mean other global actions like pane numbering.
  function onPaneClick(pane: TranscriptPane, event: MouseEvent): void {
    if (!multiPane) return;
    if (!hasOnlyMeta(event)) return;
    event.preventDefault();
    event.stopPropagation();
    targetPane(pane);
  }

  // ── Cmd-held target overlay ─────────────────────────────────────────────────
  // Holding plain Cmd previews the Cmd+click commit on the hovered pane. The
  // armed state clears on keyup AND on window blur: if the app loses focus
  // while Cmd is held (Cmd+Tab away), the keyup never arrives and the overlay
  // would stick.
  let cmdOnlyHeld = $state(false);
  let hoveredPaneId = $state<string | null>(null);

  function onWindowKeydown(event: KeyboardEvent): void {
    cmdOnlyHeld = hasOnlyMeta(event);
  }
  function onWindowKeyup(event: KeyboardEvent): void {
    cmdOnlyHeld = event.key === "Meta" ? false : hasOnlyMeta(event);
  }
  function onWindowBlur(): void {
    cmdOnlyHeld = false;
  }

  // ── Gutter resize ───────────────────────────────────────────────────────────
  // Generalized from GitView's detail-panel resize: pointer-down on gutter i
  // adjusts panes i and i+1, both clamped to MIN_PANE_WIDTH_PX against the live
  // row width. Drafted locally during the drag; committed (and persisted) once
  // on pointer-up.
  let rowEl = $state<HTMLDivElement | null>(null);
  let rowWidth = $state(0);
  let resizing = $state<{
    leftIndex: number;
    rightIndex: number;
    visibleSum: number;
    pairSum: number;
    leftStart: number;
  } | null>(null);
  let draftFractions = $state<number[] | null>(null);

  const effectiveFractions = $derived(draftFractions ?? layout.fractions);
  const renderFractions = $derived(displayedFractions(renderPanes, effectiveFractions));

  $effect(() => {
    setPaneRowWidth(rowWidth);
  });

  function displayedFractions(
    items: typeof renderPanes,
    fractions: typeof effectiveFractions,
  ): number[] {
    if (maximizedPane !== null) return items.map(() => 1);
    const sum = items.reduce((acc, item) => acc + (fractions[item.originalIndex] ?? 0), 0);
    if (sum <= 0) return items.map(() => 1 / items.length);
    return items.map((item) => (fractions[item.originalIndex] ?? 0) / sum);
  }

  function startResize(visibleIndex: number, event: PointerEvent): void {
    const fractions = layout.fractions;
    const items = renderPanes;
    const left = items[visibleIndex];
    const right = items[visibleIndex + 1];
    if (left === undefined || right === undefined) return;
    const display = displayedFractions(items, fractions);
    const visibleSum = items.reduce((acc, item) => acc + (fractions[item.originalIndex] ?? 0), 0);
    resizing = {
      leftIndex: left.originalIndex,
      rightIndex: right.originalIndex,
      visibleSum,
      pairSum: (display[visibleIndex] ?? 0) + (display[visibleIndex + 1] ?? 0),
      leftStart: display.slice(0, visibleIndex).reduce((acc, f) => acc + f, 0),
    };
    draftFractions = [...fractions];
    event.preventDefault();
  }

  function resizeMove(event: PointerEvent): void {
    if (resizing === null || rowEl === null) return;
    const rect = rowEl.getBoundingClientRect();
    if (rect.width <= 0) return;
    const { leftIndex, rightIndex, visibleSum, pairSum, leftStart } = resizing;
    const minF = MIN_PANE_WIDTH_PX / rect.width;
    const boundary = (event.clientX - rect.left) / rect.width;
    let left = boundary - leftStart;
    const lo = minF;
    const hi = pairSum - minF;
    // Row too narrow for both panes at min width → hold the pair at an even
    // split rather than inverting the clamp range.
    left = hi < lo ? pairSum / 2 : Math.min(hi, Math.max(lo, left));
    const next = [...(draftFractions ?? layout.fractions)];
    next[leftIndex] = left * visibleSum;
    next[rightIndex] = (pairSum - left) * visibleSum;
    draftFractions = next;
  }

  function endResize(): void {
    if (resizing !== null && draftFractions !== null) {
      setFractions(projectId, rosterIds, draftFractions);
    }
    resizing = null;
    draftFractions = null;
  }

  // ── Inline pane rename ──────────────────────────────────────────────────────
  // Mirrors the sidebar's agent-rename pattern: explicit edit affordance opens
  // an inline input; Enter/check commits, Escape/blur cancels. The header
  // text itself is the *target* gesture, so the two affordances never collide.
  let renamingPaneId = $state<string | null>(null);
  let renameDraft = $state("");

  function startRename(pane: TranscriptPane): void {
    renamingPaneId = pane.id;
    renameDraft = pane.name;
  }

  function commitRename(): void {
    if (renamingPaneId !== null) renamePane(projectId, rosterIds, renamingPaneId, renameDraft);
    renamingPaneId = null;
  }

  function cancelRename(): void {
    renamingPaneId = null;
  }

  function onRenameKeydown(event: KeyboardEvent): void {
    if (event.key === "Enter") {
      event.preventDefault();
      commitRename();
    } else if (event.key === "Escape") {
      event.preventDefault();
      cancelRename();
    }
  }

  function focusSelect(node: HTMLInputElement): void {
    requestAnimationFrame(() => {
      node.focus();
      node.select();
    });
  }

  // One border, one meaning: accent = "this pane's members receive the
  // draft"; the faded variant = "some of them do"; nothing = none. Rendered as
  // an absolutely-positioned overlay (like the Cmd-held overlay), NOT as a
  // ring on the pane element itself: an inset ring is a box-shadow painted in
  // the pane's own background layer, beneath its opaque children (header +
  // transcript backgrounds), so it would be drawn but never visible. The
  // overlay also never shifts layout when coverage changes.
  const COVERAGE_RING: Record<"full" | "partial", string> = {
    full: "ring-accent",
    partial: "ring-accent/35",
  };
</script>

<svelte:window
  onpointermove={resizeMove}
  onpointerup={endResize}
  onkeydown={onWindowKeydown}
  onkeyup={onWindowKeyup}
  onblur={onWindowBlur}
/>

<div
  class="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden"
  data-testid="transcript-panes-shell"
>
  <div
    bind:this={rowEl}
    bind:clientWidth={rowWidth}
    class="flex min-h-0 min-w-0 flex-1 overflow-hidden"
    data-testid="transcript-panes"
  >
    {#each renderPanes as item, i (item.pane.id)}
      {@const pane = item.pane}
      {@const visible = paneAgents(pane)}
      {@const cov = multiPane ? coverage(pane) : "none"}
      {@const active = paneIsActive(pane)}
      {#if i > 0 && maximizedPane === null}
        <div
          role="separator"
          aria-orientation="vertical"
          aria-label="Resize panes"
          data-testid={`pane-gutter-${i}`}
          class="bg-border/80 hover:bg-accent/70 w-1 shrink-0 cursor-col-resize transition-colors"
          onpointerdown={(event) => startResize(i - 1, event)}
        ></div>
      {/if}
      <!-- Cmd+click targets the pane; plain clicks pass through untouched, so a
         click-to-read can never re-aim a half-typed draft. Keyboard targeting
         exists via Cmd+Alt+1..N. -->
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <section
        class="relative flex min-w-0 flex-col overflow-hidden"
        style:flex={`${renderFractions[i] ?? 1} 1 0%`}
        style:min-width={multiPane && maximizedPane === null ? `${MIN_PANE_WIDTH_PX}px` : undefined}
        data-testid="transcript-pane"
        data-pane-id={pane.id}
        data-coverage={multiPane && maximizedPane === null ? cov : undefined}
        data-maximized={maximizedPane?.id === pane.id}
        onclick={(event) => onPaneClick(pane, event)}
        onpointerenter={() => (hoveredPaneId = pane.id)}
        onpointerleave={() => (hoveredPaneId = hoveredPaneId === pane.id ? null : hoveredPaneId)}
      >
        {#if paneChrome}
          <header
            class="border-border/80 bg-raised flex h-8 shrink-0 items-center gap-1 border-b px-2"
            data-testid="pane-header"
          >
            {#if renamingPaneId === pane.id}
              <input
                use:focusSelect
                bind:value={renameDraft}
                class="text-fg border-border bg-panel focus-visible:ring-accent h-6 min-w-0 flex-1 rounded border px-1.5 text-xs font-semibold focus-visible:ring-1 focus-visible:outline-none"
                aria-label="Pane name"
                data-testid="pane-rename-input"
                onkeydown={onRenameKeydown}
                onblur={cancelRename}
              />
              <Tooltip label="Save pane name">
                {#snippet trigger(props)}
                  <button
                    {...props}
                    type="button"
                    class={cn(ICON_BUTTON_CLASS, "hover:bg-border/60 shrink-0")}
                    aria-label="Save pane name"
                    data-testid="pane-rename-save"
                    onmousedown={(event) => event.preventDefault()}
                    onclick={commitRename}
                  >
                    <Check size={14} strokeWidth={2} aria-hidden="true" />
                  </button>
                {/snippet}
              </Tooltip>
            {:else}
              {#if pane.members.length === 0}
                <!-- An empty pane is not a send target — a "Send to" affordance
                   here could only clear the recipient set. Plain name; the
                   pane body explains how to populate it. -->
                <span
                  class="text-muted flex h-6 min-w-0 flex-1 items-center px-1.5 text-xs font-semibold"
                  data-testid="pane-name"
                >
                  {pane.name}
                </span>
              {:else if multiPane}
                <Tooltip
                  label={`Send to ${pane.name}`}
                  shortcut={item.originalIndex < 9
                    ? shortcut("mod", "alt", String(item.originalIndex + 1))
                    : undefined}
                >
                  {#snippet trigger(props)}
                    <button
                      {...props}
                      type="button"
                      class="hover:bg-panel flex h-6 min-w-0 items-center rounded px-1.5 text-left"
                      data-testid="pane-target"
                      onclick={() => targetPane(pane)}
                    >
                      <span class="text-fg truncate text-xs font-semibold" data-testid="pane-name">
                        {pane.name}
                      </span>
                    </button>
                  {/snippet}
                </Tooltip>
              {:else}
                <span
                  class="text-fg flex h-6 min-w-0 items-center px-1.5 text-xs font-semibold"
                  data-testid="pane-name"
                >
                  {pane.name}
                </span>
              {/if}
              <div class="flex min-w-0 flex-1 items-center gap-1 overflow-hidden">
                {#each paneMemberAgents(pane) as member (member.id)}
                  <span
                    class="border-border bg-panel text-fg inline-flex h-5 max-w-28 min-w-0 items-center gap-1 rounded-full border px-1.5 text-[11px]"
                    data-testid="pane-member-chip"
                    data-agent-id={member.id}
                  >
                    <HarnessIcon harness={member.harness} size="sm" class="h-3 w-3 shrink-0" />
                    <span class="truncate">{member.name}</span>
                    <button
                      type="button"
                      class="text-muted hover:text-status-failed hover:border-status-failed hover:bg-status-failed-soft/70 -mr-1 inline-flex h-4 w-4 shrink-0 items-center justify-center rounded-full border border-transparent"
                      aria-label={`Remove ${member.name} from ${pane.name}`}
                      data-testid="pane-member-remove"
                      onclick={(event) => {
                        event.stopPropagation();
                        unassignAgentFromPane(projectId, rosterIds, member.id);
                      }}
                    >
                      <X size={10} strokeWidth={2} aria-hidden="true" />
                    </button>
                  </span>
                {/each}
              </div>
              {#if active}
                <span
                  class="text-muted inline-flex h-[26px] w-[26px] shrink-0 items-center justify-center"
                  role="status"
                  aria-label={`${pane.name} has running agents`}
                  data-testid="pane-activity"
                >
                  <Spinner class="h-4 w-4" />
                </span>
              {/if}
              {#if layout.panes.length > 2 && maximizedPane === null && renderPanes.length > 1}
                <Tooltip label={`Minimize ${pane.name}`}>
                  {#snippet trigger(props)}
                    <button
                      {...props}
                      type="button"
                      class={cn(ICON_BUTTON_CLASS, "hover:bg-border/60 shrink-0")}
                      aria-label={`Minimize ${pane.name}`}
                      data-testid="pane-minimize"
                      onclick={(event) => {
                        event.stopPropagation();
                        minimizePane(projectId, rosterIds, pane.id);
                      }}
                    >
                      <Minimize2 size={12} strokeWidth={1.8} aria-hidden="true" />
                    </button>
                  {/snippet}
                </Tooltip>
              {/if}
              {#if multiPane}
                <Tooltip
                  label={maximizedPane?.id === pane.id ? "Restore panes" : `Maximize ${pane.name}`}
                >
                  {#snippet trigger(props)}
                    <button
                      {...props}
                      type="button"
                      class={cn(ICON_BUTTON_CLASS, "hover:bg-border/60 shrink-0")}
                      aria-label={maximizedPane?.id === pane.id
                        ? "Restore panes"
                        : `Maximize ${pane.name}`}
                      data-testid="pane-maximize"
                      onclick={(event) => {
                        event.stopPropagation();
                        if (maximizedPane?.id === pane.id) {
                          restoreMaximizedPane(projectId, rosterIds);
                        } else {
                          maximizePane(projectId, rosterIds, pane.id);
                        }
                      }}
                    >
                      {#if maximizedPane?.id === pane.id}
                        <Minimize2 size={12} strokeWidth={1.8} aria-hidden="true" />
                      {:else}
                        <Maximize2 size={12} strokeWidth={1.8} aria-hidden="true" />
                      {/if}
                    </button>
                  {/snippet}
                </Tooltip>
              {/if}
              <DropdownMenu
                triggerLabel={`Actions for ${pane.name}`}
                triggerTestid="pane-actions"
                triggerClass={cn(ICON_BUTTON_CLASS, "hover:bg-border/60 shrink-0")}
                tooltipLabel={`Actions for ${pane.name}`}
                contentTestid="pane-actions-menu"
              >
                {#snippet trigger()}
                  <MoreHorizontal size={14} strokeWidth={1.8} aria-hidden="true" />
                {/snippet}
                {#each agents as agent (agent.id)}
                  {@const alreadyInPane = pane.members.includes(agent.id)}
                  {@const currentPane = layout.panes.find((p) => p.members.includes(agent.id))}
                  <DropdownMenuItem
                    onSelect={() => moveAgentToPane(projectId, rosterIds, agent.id, pane.id)}
                    disabled={alreadyInPane}
                    class="gap-2"
                    data-testid={`pane-add-agent-${agent.id}`}
                  >
                    <HarnessIcon harness={agent.harness} size="sm" class="h-3.5 w-3.5 shrink-0" />
                    <span class="min-w-0 flex-1 truncate">{agent.name}</span>
                    {#if alreadyInPane}
                      <span class="text-muted text-xs">in pane</span>
                    {:else if currentPane !== undefined}
                      <span class="text-muted text-xs">move</span>
                    {:else}
                      <span class="text-muted text-xs">add</span>
                    {/if}
                  </DropdownMenuItem>
                {/each}
                <DropdownMenuItem
                  onSelect={() => startRename(pane)}
                  class="gap-2"
                  data-testid="pane-rename"
                >
                  <Pencil size={14} strokeWidth={1.8} aria-hidden="true" />
                  Rename pane
                </DropdownMenuItem>
                {#if layout.panes.length > 1}
                  <DropdownMenuItem
                    onSelect={() => closePane(projectId, rosterIds, pane.id)}
                    class="items-start gap-2"
                    data-testid="pane-close"
                  >
                    <X size={14} strokeWidth={1.8} aria-hidden="true" class="mt-0.5 shrink-0" />
                    <span class="flex min-w-0 flex-col">
                      <span>Close pane</span>
                      <span class="text-muted text-xs leading-4">
                        Agents become unassigned and keep working.
                      </span>
                    </span>
                  </DropdownMenuItem>
                {/if}
              </DropdownMenu>
            {/if}
          </header>
        {/if}
        {#if visible.length === 0}
          <div
            class="text-muted flex flex-1 flex-col items-center justify-center gap-2 p-6 text-center text-xs"
            data-testid="pane-empty"
          >
            {#if pane.members.length === 0}
              <p>
                No agents in this pane — add one from the pane header or move one here from the
                agents sidebar.
              </p>
            {:else}
              <p>All agents in this pane are hidden.</p>
              <button
                type="button"
                class="text-accent hover:underline"
                data-testid="pane-show-all"
                onclick={() => showAllInPane(projectId, rosterIds, pane.id)}
              >
                Show all
              </button>
            {/if}
          </div>
        {:else}
          <UnifiedTranscript
            {projectId}
            agents={visible}
            {overlay}
            {loadStatus}
            {loadError}
            {onRetryLoad}
          />
        {/if}
        {#if multiPane && maximizedPane === null && cov !== "none"}
          <div
            class={cn(
              "pointer-events-none absolute inset-0 z-10 ring-2 ring-inset",
              COVERAGE_RING[cov],
            )}
            data-testid="pane-coverage"
          ></div>
        {/if}
        {#if multiPane && maximizedPane === null && cmdOnlyHeld && hoveredPaneId === pane.id && pane.members.length > 0}
          <div
            class="ring-accent pointer-events-none absolute inset-0 z-10 flex items-start justify-center ring-2 ring-inset"
            data-testid="pane-target-overlay"
          >
            <span
              class="bg-accent-soft text-fg mt-10 rounded-full px-2.5 py-0.5 text-xs font-medium shadow"
            >
              Send to {pane.name} — ⌘click
            </span>
          </div>
        {/if}
      </section>
    {/each}
  </div>
</div>
