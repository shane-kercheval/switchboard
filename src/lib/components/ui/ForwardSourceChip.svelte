<script lang="ts">
  import { cn } from "$lib/utils";
  import type { ForwardReadiness, ForwardSource } from "$lib/state/heldForwards.svelte";

  // A forward-source chip — the agent whose latest output will be forwarded.
  // Shared by the compose bar and the prompt/workflow composers' per-field
  // forwarding so every surface looks identical. Agents are the first-class unit:
  // a pane is expanded to one chip per member agent at pick time, so a chip always
  // stands for a single agent. `onRemove` drops it.
  //
  // `readiness` is what this source will contribute at dispatch, and only `empty`
  // is a warning — that source is *skipped* from the send. A boolean cannot express
  // this: a `pending` agent (one still generating) is about to contribute normally,
  // and flagging it as a failure told the user the opposite of what would happen.
  let {
    source,
    readiness = "ready",
    disabled = false,
    onRemove,
  }: {
    source: ForwardSource;
    readiness?: ForwardReadiness;
    disabled?: boolean;
    onRemove: () => void;
  } = $props();
</script>

<span
  class={cn(
    "inline-flex max-w-[14rem] items-center gap-1.5 rounded-full border py-px pr-1 pl-2 text-xs",
    readiness === "empty"
      ? "border-status-failed/40 bg-status-failed-soft/40 text-status-failed"
      : readiness === "pending"
        ? "border-status-processing/40 bg-status-processing-soft/40 text-status-processing"
        : "border-border bg-panel text-fg",
  )}
  data-testid={`forward-source-chip-${source.name}`}
  data-readiness={readiness}
>
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
  <span class="truncate" title={source.name}>{source.name}</span>
  <!-- Each caption names the consequence, not the agent's state. An empty source is
       dropped from the composed body (and if every source is empty the send doesn't
       happen at all); a pending one makes the send wait. -->
  {#if readiness === "empty"}
    <span
      class="shrink-0 italic"
      title="This agent has no completed output, so it will be left out of the forward"
      >will be skipped</span
    >
  {:else if readiness === "pending"}
    <span class="shrink-0 italic" title="Sending will wait for this agent's turn to finish"
      >still generating</span
    >
  {/if}
  <button
    type="button"
    class="text-muted hover:text-status-failed hover:border-status-failed hover:bg-status-failed-soft/70 flex h-4 w-4 shrink-0 items-center justify-center rounded-full border border-transparent transition-colors disabled:cursor-not-allowed disabled:opacity-50"
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
