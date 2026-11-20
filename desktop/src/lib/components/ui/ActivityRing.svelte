<script lang="ts">
  /// A ring drawn around a round child (an agent's harness icon) while
  /// `active`: a running-color arc turning once every 2s on a faint track.
  /// Absolutely positioned 3px outside the child, so it never changes layout
  /// when it appears. Under `prefers-reduced-motion` it stops and shows as a
  /// full circle. Decorative: the surrounding surface carries the label.
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  type Props = {
    active: boolean;
    children: Snippet;
    class?: string;
  };

  let { active, children, class: className }: Props = $props();
</script>

<span class={cn("relative inline-flex shrink-0 rounded-full", className)}>
  {@render children()}
  {#if active}
    <span
      class="border-status-processing/20 border-t-status-processing motion-reduce:border-status-processing pointer-events-none absolute -inset-[3px] animate-spin rounded-full border-2 [animation-duration:2s] motion-reduce:animate-none"
      aria-hidden="true"
      data-testid="activity-ring"
    ></span>
  {/if}
</span>
