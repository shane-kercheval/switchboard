<script lang="ts">
  import { cn } from "$lib/utils";
  import { Ban, LoaderCircle } from "@lucide/svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
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
  //
  // A non-`ready` state shows as **colour + a trailing icon**, not inline text: the
  // chip is width-constrained (several sit inline, and the name already truncates),
  // so a phrase like "still generating" blew the chip out. The icon carries a
  // zero-delay tooltip with the full explanation for pointer users; assistive tech
  // gets the same consequence from a visually-hidden (`sr-only`) text node in the
  // chip — an `aria-label` on the icon's role-less span is announced unreliably in
  // WebKit/VoiceOver (the app's runtime), so the consequence is real DOM text.
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

  // `null` for `ready` — no icon, no tooltip, neutral chip. `tooltip` is the
  // pointer explanation; `srText` is the same consequence phrased to read
  // naturally after the agent name for a screen reader ("bob, will be skipped…").
  const stateHint = $derived(
    readiness === "empty"
      ? {
          tooltip: "This agent has no completed output, so it will be left out of the forward",
          srText: "will be skipped from the forward",
        }
      : readiness === "pending"
        ? {
            tooltip: "Sending will wait for this agent's turn to finish",
            srText: "still generating; sending will wait for its turn",
          }
        : null,
  );
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
  {#if stateHint !== null}
    <span class="sr-only">, {stateHint.srText}</span>
    <Tooltip label={stateHint.tooltip} delayDuration={0}>
      {#snippet trigger(props)}
        <span
          {...props}
          class="flex h-3.5 w-3.5 shrink-0 items-center justify-center"
          data-testid={`forward-source-state-${source.name}`}
          data-state-readiness={readiness}
          aria-hidden="true"
        >
          {#if readiness === "empty"}
            <Ban class="h-3.5 w-3.5" aria-hidden="true" />
          {:else}
            <LoaderCircle class="h-3.5 w-3.5 animate-spin" aria-hidden="true" />
          {/if}
        </span>
      {/snippet}
    </Tooltip>
  {/if}
  <button
    type="button"
    class="text-muted hover:text-status-failed hover:border-status-failed hover:bg-status-failed-soft/70 flex h-4 w-4 shrink-0 items-center justify-center rounded-full border border-transparent transition-colors disabled:cursor-not-allowed disabled:opacity-50"
    data-testid={`forward-source-remove-${source.name}`}
    aria-label={`Remove forward source ${source.name}`}
    {disabled}
    onmousedown={(e) => e.preventDefault()}
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
