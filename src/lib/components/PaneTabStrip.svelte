<script lang="ts">
  import { CircleCheck } from "@lucide/svelte";
  import { Portal } from "bits-ui";
  import { onDestroy } from "svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { DRAG_SLOP_PX, dropIndexForPointer } from "$lib/agentReorder";
  import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";
  import type { ProjectId } from "$lib/types";
  import { cn } from "$lib/utils";
  import type { HeaderPaneEntry, HeaderPaneState } from "./PaneTabStrip.types";

  let {
    entries,
    projectId,
    paneIsActive,
    paneIsCompleted,
    onSelectVisible,
    onOpenHidden,
    onReorder,
  }: {
    entries: HeaderPaneEntry[];
    projectId: ProjectId;
    paneIsActive: (pane: TranscriptPane) => boolean;
    paneIsCompleted: (pane: TranscriptPane) => boolean;
    onSelectVisible: (pane: TranscriptPane) => void;
    onOpenHidden: (pane: TranscriptPane) => void;
    onReorder: (projectId: ProjectId, paneId: string, toIndex: number) => void;
  } = $props();

  let stripEl: HTMLDivElement;
  let activeCleanup: (() => void) | null = null;
  let dragState = $state<{
    paneId: string;
    paneName: string;
    paneState: HeaderPaneState;
    projectId: ProjectId;
    pointerId: number;
    startX: number;
    startY: number;
    startOrder: string[];
    startIndex: number;
    targetIndex: number;
    inDropZone: boolean;
    started: boolean;
    pointerX: number;
    pointerY: number;
  } | null>(null);

  const dropBeforeId = $derived.by(() => {
    const drag = dragState;
    if (drag === null || !drag.started || !drag.inDropZone || drag.targetIndex === drag.startIndex)
      return null;
    return drag.startOrder.filter((id) => id !== drag.paneId)[drag.targetIndex] ?? null;
  });
  const dropAtEnd = $derived(
    dragState !== null &&
      dragState.started &&
      dragState.inDropZone &&
      dragState.targetIndex !== dragState.startIndex &&
      dropBeforeId === null,
  );

  function sameOrder(order: string[]): boolean {
    return (
      entries.length === order.length && entries.every((entry, i) => entry.pane.id === order[i])
    );
  }

  function swallowNextClick(): void {
    const swallow = (event: MouseEvent): void => {
      event.preventDefault();
      event.stopPropagation();
    };
    window.addEventListener("click", swallow, { capture: true });
    setTimeout(() => window.removeEventListener("click", swallow, { capture: true }), 0);
  }

  function inDropZone(clientY: number): boolean {
    const rect = stripEl.getBoundingClientRect();
    return clientY >= rect.top - 8 && clientY <= rect.bottom + 8;
  }

  function cancelStaleDrag(): void {
    activeCleanup?.();
    dragState = null;
  }

  onDestroy(() => activeCleanup?.());

  function beginDrag(paneId: string, event: PointerEvent): void {
    if (entries.length < 2 || event.button !== 0 || dragState !== null) return;
    event.preventDefault();
    const startOrder = entries.map((entry) => entry.pane.id);
    const source = entries.find((entry) => entry.pane.id === paneId);
    if (source === undefined) return;
    const pointerId = event.pointerId;
    let cancelled = false;
    dragState = {
      paneId,
      paneName: source.pane.name,
      paneState: source.state,
      projectId,
      pointerId,
      startX: event.clientX,
      startY: event.clientY,
      startOrder,
      startIndex: startOrder.indexOf(paneId),
      targetIndex: startOrder.indexOf(paneId),
      inDropZone: true,
      started: false,
      pointerX: event.clientX,
      pointerY: event.clientY,
    };
    let scrollFrame: number | null = null;
    const updateTarget = (drag: NonNullable<typeof dragState>): void => {
      const midpoints: number[] = [];
      for (const chip of stripEl.querySelectorAll<HTMLElement>("[data-pane-id]")) {
        if (chip.dataset.paneId === paneId) continue;
        const chipRect = chip.getBoundingClientRect();
        midpoints.push(chipRect.left + chipRect.width / 2);
      }
      drag.targetIndex = dropIndexForPointer(midpoints, drag.pointerX);
    };
    const stopScroll = (): void => {
      if (scrollFrame !== null) cancelAnimationFrame(scrollFrame);
      scrollFrame = null;
    };
    const cancelIfStale = (drag: NonNullable<typeof dragState>): boolean => {
      if (projectId === drag.projectId && sameOrder(drag.startOrder)) return false;
      cancelled = true;
      dragState = null;
      stopScroll();
      return true;
    };
    const scrollAtEdge = (): void => {
      scrollFrame = null;
      const drag = dragState;
      if (cancelled || drag === null || !drag.started || !drag.inDropZone) return;
      if (cancelIfStale(drag)) return;
      const rect = stripEl.getBoundingClientRect();
      const delta = drag.pointerX < rect.left + 28 ? -10 : drag.pointerX > rect.right - 28 ? 10 : 0;
      if (delta === 0) return;
      const previous = stripEl.scrollLeft;
      stripEl.scrollLeft += delta;
      if (stripEl.scrollLeft === previous) return;
      updateTarget(drag);
      scrollFrame = requestAnimationFrame(scrollAtEdge);
    };
    const onMove = (e: PointerEvent): void => {
      if (e.pointerId !== pointerId) return;
      if ((e.buttons & 1) === 0) {
        cancelStaleDrag();
        return;
      }
      if (cancelled) return;
      const drag = dragState;
      if (drag === null) return;
      if (cancelIfStale(drag)) return;
      if (!drag.started) {
        if (Math.hypot(e.clientX - drag.startX, e.clientY - drag.startY) < DRAG_SLOP_PX) return;
        drag.started = true;
      }
      drag.pointerX = e.clientX;
      drag.pointerY = e.clientY;
      drag.inDropZone = inDropZone(e.clientY);
      updateTarget(drag);
      if (drag.inDropZone && scrollFrame === null) scrollAtEdge();
      else if (!drag.inDropZone) stopScroll();
    };
    const cleanup = (): void => {
      stopScroll();
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerdown", cancelStaleDrag, { capture: true });
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
      window.removeEventListener("keydown", onKey, { capture: true });
      window.removeEventListener("blur", onBlur);
      document.removeEventListener("visibilitychange", onVisibilityChange);
      activeCleanup = null;
    };
    const onUp = (e: PointerEvent): void => {
      if (e.pointerId !== pointerId) return;
      const primaryRelease = e.button === 0 && e.buttons === 0;
      const drag = dragState;
      cleanup();
      dragState = null;
      if (cancelled || drag?.started) swallowNextClick();
      if (
        cancelled ||
        !primaryRelease ||
        drag === null ||
        !drag.started ||
        projectId !== drag.projectId ||
        !sameOrder(drag.startOrder)
      )
        return;
      if (!inDropZone(e.clientY)) return;
      if (drag.targetIndex !== drag.startIndex) onReorder(drag.projectId, paneId, drag.targetIndex);
    };
    const onCancel = (e: PointerEvent): void => {
      if (e.pointerId !== pointerId) return;
      cleanup();
      dragState = null;
    };
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== "Escape" || cancelled || dragState?.started !== true) return;
      e.preventDefault();
      cancelled = true;
      dragState = null;
      stopScroll();
    };
    const onBlur = (): void => {
      cleanup();
      dragState = null;
    };
    const onVisibilityChange = (): void => {
      if (document.visibilityState === "hidden") cancelStaleDrag();
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerdown", cancelStaleDrag, { capture: true });
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onCancel);
    window.addEventListener("keydown", onKey, { capture: true });
    window.addEventListener("blur", onBlur);
    document.addEventListener("visibilitychange", onVisibilityChange);
    activeCleanup = cleanup;
  }

  function chipDrag(node: HTMLElement, paneId: string): { destroy: () => void } {
    const onPointerDown = (event: PointerEvent): void => beginDrag(paneId, event);
    node.addEventListener("pointerdown", onPointerDown);
    return { destroy: () => node.removeEventListener("pointerdown", onPointerDown) };
  }

  function presentationFor(
    pane: TranscriptPane,
    state: HeaderPaneState,
    active: boolean,
    completed: boolean,
  ): {
    visible: boolean;
    selectable: boolean;
    label: string;
    title: string;
    actionDescription: string;
  } {
    const visible = state === "visible";
    const selectable = visible && pane.members.length > 0;
    const stateDescription =
      state === "visible"
        ? "visible"
        : state === "minimized"
          ? "minimized"
          : "hidden behind the maximized pane";
    const activityDescription = active
      ? "Agents are working."
      : !visible && completed
        ? "Agents finished while hidden."
        : null;
    const actionDescription = selectable
      ? "Click to select."
      : visible
        ? "No agents assigned; selection unavailable."
        : "Click to open.";
    return {
      visible,
      selectable,
      label: [`${pane.name} — ${stateDescription}.`, activityDescription, actionDescription]
        .filter((part) => part !== null)
        .join(" "),
      title: [`${pane.name} — ${stateDescription}.`, activityDescription]
        .filter((part) => part !== null)
        .join(" "),
      actionDescription,
    };
  }
</script>

<div
  bind:this={stripEl}
  class="pane-tab-strip flex min-w-0 shrink items-center gap-1 overflow-x-auto px-1"
  data-testid="app-pane-tab-strip"
>
  {#each entries as { pane, state } (pane.id)}
    {@const active = paneIsActive(pane)}
    {@const completed = paneIsCompleted(pane)}
    {@const presentation = presentationFor(pane, state, active, completed)}
    <div class="relative shrink-0">
      {#if dropBeforeId === pane.id}
        <span
          class="bg-focus pointer-events-none absolute top-0 -left-[3px] z-20 h-full w-0.5 rounded-full"
          data-testid="pane-drop-indicator"
        ></span>
      {/if}
      <!-- The tooltip is where the spinner/✓ semantics are taught: the
         indicator is seen far more often than any empty-state prose. -->
      <Tooltip side="bottom" suppressed={dragState?.started === true} reopen="fresh-hover">
        {#snippet trigger(props)}
          {#snippet contents()}
            {#if active}
              <span
                class="inline-flex shrink-0 items-center justify-center"
                role="status"
                aria-label={`${pane.name} has running agents`}
                data-testid="app-pane-tab-activity"
              >
                <Spinner class="h-3.5 w-3.5" />
              </span>
            {:else if completed}
              <span
                class="text-accent inline-flex shrink-0 items-center justify-center"
                role="status"
                aria-label={`${pane.name} activity ended`}
                data-testid="app-pane-tab-completed"
              >
                <CircleCheck size={14} strokeWidth={1.8} aria-hidden="true" />
              </span>
            {/if}
            <span class="truncate font-medium">{pane.name}</span>
          {/snippet}
          {#if presentation.visible}
            <button
              {...props}
              use:chipDrag={pane.id}
              type="button"
              class={cn(
                "border-accent/60 bg-raised text-fg inline-flex h-6.5 max-w-36 shrink-0 items-center gap-1.5 rounded-full border px-2 text-xs",
                "hover:bg-control-hover hover:border-accent",
                dragState?.started && dragState.paneId === pane.id
                  ? "cursor-grabbing opacity-60"
                  : "cursor-grab",
              )}
              aria-label={presentation.label}
              aria-disabled={presentation.selectable ? undefined : "true"}
              data-testid="app-pane-tab"
              data-pane-id={pane.id}
              data-pane-state={state}
              onclick={presentation.selectable ? () => onSelectVisible(pane) : undefined}
            >
              {@render contents()}
            </button>
          {:else}
            <button
              {...props}
              use:chipDrag={pane.id}
              type="button"
              class={cn(
                "border-border bg-panel text-muted hover:bg-raised hover:text-fg inline-flex h-6.5 max-w-36 shrink-0 items-center gap-1.5 rounded-full border px-2 text-xs",
                dragState?.started && dragState.paneId === pane.id
                  ? "cursor-grabbing opacity-60"
                  : "cursor-grab",
              )}
              aria-label={presentation.label}
              data-testid="app-pane-tab"
              data-pane-id={pane.id}
              data-pane-state={state}
              onclick={() => onOpenHidden(pane)}
            >
              {@render contents()}
            </button>
          {/if}
        {/snippet}
        <div class="max-w-64">
          <div class="text-[13px] font-medium">{presentation.title}</div>
          <div
            class="text-primary-fg/70 border-primary-fg/20 mt-2 border-t pt-2 text-[12px]"
            data-testid="pane-chip-tooltip-action"
          >
            {presentation.actionDescription} Drag to rearrange.
          </div>
        </div>
      </Tooltip>
      {#if dropAtEnd && entries[entries.length - 1]?.pane.id === pane.id}
        <span
          class="bg-focus pointer-events-none absolute top-0 -right-[3px] z-20 h-full w-0.5 rounded-full"
          data-testid="pane-drop-indicator"
        ></span>
      {/if}
    </div>
  {/each}
</div>

{#if dragState?.started}
  <Portal>
    <div class="fixed inset-0 z-40 cursor-grabbing" data-testid="pane-drag-cursor-layer"></div>
    <div
      class={cn(
        "text-fg pointer-events-none fixed z-50 max-w-36 -translate-x-1/2 truncate rounded-full border px-2 py-1 text-xs font-medium shadow-lg",
        dragState.paneState === "visible" ? "border-accent/60 bg-raised" : "border-border bg-panel",
      )}
      style:left={`${Math.max(72, Math.min(dragState.pointerX, window.innerWidth - 72))}px`}
      style:top={`${Math.max(8, dragState.pointerY + 12)}px`}
      data-testid="pane-drag-preview"
    >
      {dragState.paneName}
    </div>
  </Portal>
{/if}
