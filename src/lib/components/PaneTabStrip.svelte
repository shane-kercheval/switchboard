<script lang="ts">
  import { CircleCheck } from "@lucide/svelte";
  import { Portal } from "bits-ui";
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
    started: boolean;
    pointerX: number;
    pointerY: number;
  } | null>(null);

  const dropBeforeId = $derived.by(() => {
    const drag = dragState;
    if (drag === null || !drag.started || drag.targetIndex === drag.startIndex) return null;
    return drag.startOrder.filter((id) => id !== drag.paneId)[drag.targetIndex] ?? null;
  });
  const dropAtEnd = $derived(
    dragState !== null &&
      dragState.started &&
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
      started: false,
      pointerX: event.clientX,
      pointerY: event.clientY,
    };
    const onMove = (e: PointerEvent): void => {
      if (e.pointerId !== pointerId || cancelled) return;
      const drag = dragState;
      if (drag === null) return;
      if (projectId !== drag.projectId || !sameOrder(drag.startOrder)) {
        cancelled = true;
        dragState = null;
        return;
      }
      if (!drag.started) {
        if (Math.hypot(e.clientX - drag.startX, e.clientY - drag.startY) < DRAG_SLOP_PX) return;
        drag.started = true;
      }
      drag.pointerX = e.clientX;
      drag.pointerY = e.clientY;
      const rect = stripEl.getBoundingClientRect();
      if (e.clientX < rect.left + 28) stripEl.scrollLeft -= 10;
      else if (e.clientX > rect.right - 28) stripEl.scrollLeft += 10;
      const midpoints: number[] = [];
      for (const chip of stripEl.querySelectorAll<HTMLElement>("[data-pane-id]")) {
        if (chip.dataset.paneId === paneId) continue;
        const chipRect = chip.getBoundingClientRect();
        midpoints.push(chipRect.left + chipRect.width / 2);
      }
      drag.targetIndex = dropIndexForPointer(midpoints, e.clientX);
    };
    const cleanup = (): void => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
      window.removeEventListener("keydown", onKey, { capture: true });
      window.removeEventListener("blur", onBlur);
    };
    const onUp = (e: PointerEvent): void => {
      if (e.pointerId !== pointerId) return;
      const drag = dragState;
      cleanup();
      dragState = null;
      if (cancelled || drag?.started) swallowNextClick();
      if (
        cancelled ||
        drag === null ||
        !drag.started ||
        projectId !== drag.projectId ||
        !sameOrder(drag.startOrder)
      )
        return;
      const rect = stripEl.getBoundingClientRect();
      if (e.clientY < rect.top - 8 || e.clientY > rect.bottom + 8) return;
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
    };
    const onBlur = (): void => {
      cleanup();
      dragState = null;
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onCancel);
    window.addEventListener("keydown", onKey, { capture: true });
    window.addEventListener("blur", onBlur);
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
  class="pane-tab-strip flex min-w-0 shrink items-center gap-1 overflow-x-auto"
  data-testid="app-pane-tab-strip"
>
  {#each entries as { pane, state } (pane.id)}
    {@const active = paneIsActive(pane)}
    {@const completed = paneIsCompleted(pane)}
    {@const presentation = presentationFor(pane, state, active, completed)}
    <div class="relative shrink-0">
      {#if dropBeforeId === pane.id}
        <span
          class={cn(
            "bg-focus pointer-events-none absolute top-0 z-20 h-full w-0.5 rounded-full",
            entries[0]?.pane.id === pane.id ? "left-0.5" : "-left-0.5",
          )}
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
          class="bg-focus pointer-events-none absolute top-0 right-0.5 z-20 h-full w-0.5 rounded-full"
          data-testid="pane-drop-indicator"
        ></span>
      {/if}
    </div>
  {/each}
</div>

{#if dragState?.started}
  <Portal>
    <div
      class={cn(
        "text-fg pointer-events-none fixed z-50 max-w-36 truncate rounded-full border px-2 py-1 text-xs font-medium shadow-lg",
        dragState.paneState === "visible" ? "border-accent/60 bg-raised" : "border-border bg-panel",
      )}
      style:left={`${Math.max(8, Math.min(dragState.pointerX + 12, window.innerWidth - 160))}px`}
      style:top={`${Math.max(8, dragState.pointerY + 12)}px`}
      data-testid="pane-drag-preview"
    >
      {dragState.paneName}
    </div>
  </Portal>
{/if}
