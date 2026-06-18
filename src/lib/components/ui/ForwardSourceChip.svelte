<script lang="ts">
  import { cn } from "$lib/utils";
  import type { ForwardSource } from "$lib/state/heldForwards.svelte";

  // A forward-source chip — the agent (or pane) whose latest output will be
  // forwarded. Shared by the compose bar and the prompt composer's per-argument
  // forwarding so the two surfaces look identical. A pane forwards each member's
  // output but shows as one named chip (a leading pane glyph distinguishes it);
  // `empty` flags a source with no completed output yet (for a pane, every member
  // empty); `onRemove` drops it.
  let {
    source,
    empty = false,
    disabled = false,
    onRemove,
  }: {
    source: ForwardSource;
    empty?: boolean;
    disabled?: boolean;
    onRemove: () => void;
  } = $props();
</script>

<span
  class={cn(
    "inline-flex max-w-[14rem] items-center gap-1.5 rounded-full border py-px pr-1 pl-2 text-xs",
    empty
      ? "border-status-failed/40 bg-status-failed-soft/40 text-status-failed"
      : "border-border bg-panel text-fg",
  )}
  data-testid={`forward-source-chip-${source.name}`}
  data-kind={source.kind}
  data-empty={empty}
>
  {#if source.kind === "pane"}
    <!-- Pane glyph (matches the picker's pane rows): one chip stands for all its
         members. -->
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.8"
      stroke-linecap="round"
      stroke-linejoin="round"
      class="h-3 w-3 shrink-0"
      aria-hidden="true"
    >
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <path d="M12 4v16" />
    </svg>
  {:else}
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      class="h-3 w-3 shrink-0"
      aria-hidden="true"
    >
      <polyline points="15 17 20 12 15 7" />
      <path d="M4 18v-2a4 4 0 0 1 4-4h12" />
    </svg>
  {/if}
  <span class="truncate" title={source.name}>{source.name}</span>
  {#if empty}
    <span
      class="shrink-0 italic"
      title={source.kind === "pane"
        ? "No agent in this pane has completed output to forward"
        : "This agent has no completed output to forward"}>no output</span
    >
  {/if}
  <button
    type="button"
    class="text-muted hover:text-fg hover:bg-raised flex h-4 w-4 shrink-0 items-center justify-center rounded-full transition-colors disabled:cursor-not-allowed disabled:opacity-50"
    data-testid={`forward-source-remove-${source.name}`}
    aria-label={`Remove forward source ${source.name}`}
    {disabled}
    onclick={onRemove}
  >
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      class="h-3 w-3"
      aria-hidden="true"
    >
      <path d="m6 6 12 12M18 6 6 18" />
    </svg>
  </button>
</span>
