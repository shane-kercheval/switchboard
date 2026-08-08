<script lang="ts">
  import { CircleCheck } from "@lucide/svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";
  import { cn } from "$lib/utils";
  import type { HeaderPaneEntry, HeaderPaneState } from "./PaneTabStrip.types";

  let {
    entries,
    paneIsActive,
    paneIsCompleted,
    onSelectVisible,
    onOpenHidden,
  }: {
    entries: HeaderPaneEntry[];
    paneIsActive: (pane: TranscriptPane) => boolean;
    paneIsCompleted: (pane: TranscriptPane) => boolean;
    onSelectVisible: (pane: TranscriptPane) => void;
    onOpenHidden: (pane: TranscriptPane) => void;
  } = $props();

  function presentationFor(
    pane: TranscriptPane,
    state: HeaderPaneState,
    active: boolean,
    completed: boolean,
  ): {
    visible: boolean;
    selectable: boolean;
    label: string;
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
    };
  }
</script>

<div
  class="pane-tab-strip flex min-w-0 shrink items-center gap-1 overflow-x-auto"
  data-testid="app-pane-tab-strip"
>
  {#each entries as { pane, state } (pane.id)}
    {@const active = paneIsActive(pane)}
    {@const completed = paneIsCompleted(pane)}
    {@const presentation = presentationFor(pane, state, active, completed)}
    <!-- The tooltip is where the spinner/✓ semantics are taught: the
         indicator is seen far more often than any empty-state prose. -->
    <Tooltip
      label={presentation.label}
      side="bottom"
      reopen={presentation.selectable || !presentation.visible ? "fresh-hover" : "default"}
    >
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
            type="button"
            class={cn(
              "border-accent/60 bg-raised text-fg inline-flex h-6.5 max-w-36 shrink-0 items-center gap-1.5 rounded-full border px-2 text-xs",
              presentation.selectable
                ? "hover:bg-control-hover hover:border-accent"
                : "cursor-default",
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
            type="button"
            class="border-border bg-panel text-muted hover:bg-raised hover:text-fg inline-flex h-6.5 max-w-36 shrink-0 items-center gap-1.5 rounded-full border px-2 text-xs"
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
    </Tooltip>
  {/each}
</div>
