<script lang="ts">
  /// A ring drawn around a round child (an agent's harness icon). Working: a
  /// running-color arc turning once every 2s on a faint track, drawn as a full
  /// circle when it stops. Waiting: a muted dashed ring turning once every 6s,
  /// slower so the two stay distinct. Both stop under `prefers-reduced-motion`.
  /// Absolutely positioned 3px outside the child, so it never changes layout
  /// when it appears. Decorative: the surrounding surface carries the label.
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  type Props = {
    state: "idle" | "working" | "waiting";
    children: Snippet;
    class?: string;
  };

  let { state, children, class: className }: Props = $props();
</script>

<span class={cn("relative inline-flex shrink-0 rounded-full", className)}>
  {@render children()}
  {#if state === "working"}
    <span
      class="border-status-processing/20 border-t-status-processing motion-reduce:border-status-processing pointer-events-none absolute -inset-[3px] animate-spin rounded-full border-2 [animation-duration:2s] motion-reduce:animate-none"
      aria-hidden="true"
      data-testid="activity-ring"
    ></span>
  {:else if state === "waiting"}
    <!-- An SVG circle, not a dashed border: WebKit leaves one uneven gap in a
         round dashed border. `pathLength` 16 with a 1-1 dash array gives exactly
         8 even dashes. The span takes the insets and the SVG fills it, because
         older WebKit does not stretch a replaced element between insets. The
         viewBox fits a 32px icon (a 38px box); other sizes scale the stroke. -->
    <span
      class="pointer-events-none absolute -inset-[3px] animate-spin [animation-duration:6s] motion-reduce:animate-none"
      aria-hidden="true"
      data-testid="waiting-ring"
    >
      <svg class="text-muted block h-full w-full" viewBox="0 0 38 38" fill="none">
        <circle
          cx="19"
          cy="19"
          r="18"
          stroke="currentColor"
          stroke-width="2"
          pathLength="16"
          stroke-dasharray="1 1"
        />
      </svg>
    </span>
  {/if}
</span>
