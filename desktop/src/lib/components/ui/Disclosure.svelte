<script lang="ts">
  import type { Snippet } from "svelte";
  import { ChevronRight } from "@lucide/svelte";
  import { cn } from "$lib/utils";

  let { id, label, children }: { id: string; label: string; children: Snippet } = $props();
  let expanded = $state(false);
</script>

<div>
  <button
    type="button"
    class="text-muted hover:text-fg focus-visible:ring-focus flex w-full items-center gap-1.5 rounded-sm py-1 text-left text-xs font-medium outline-none focus-visible:ring-1"
    aria-expanded={expanded}
    aria-controls={id}
    data-testid={`${id}-toggle`}
    onclick={() => (expanded = !expanded)}
  >
    <ChevronRight
      size={14}
      strokeWidth={1.8}
      aria-hidden="true"
      class={cn("shrink-0 transition-transform", expanded && "rotate-90")}
    />
    {label}
  </button>
  <div {id} hidden={!expanded} class="text-muted space-y-3 pt-2 text-xs leading-relaxed">
    {@render children()}
  </div>
</div>
