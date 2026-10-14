<script lang="ts">
  import type { Snippet } from "svelte";
  import { ChevronRight, Info } from "@lucide/svelte";
  import { cn } from "$lib/utils";

  let { id, label, children }: { id: string; label: string; children: Snippet } = $props();
  let expanded = $state(false);
</script>

<div class="border-border bg-panel overflow-hidden rounded-md border">
  <button
    type="button"
    class="text-fg hover:bg-hover focus-visible:ring-focus flex min-h-11 w-full items-center gap-2.5 px-3 py-2.5 text-left text-sm font-medium outline-none focus-visible:ring-1 focus-visible:ring-inset"
    aria-expanded={expanded}
    aria-controls={id}
    data-testid={`${id}-toggle`}
    onclick={() => (expanded = !expanded)}
  >
    <Info size={16} strokeWidth={1.8} aria-hidden="true" class="text-muted shrink-0" />
    <span class="min-w-0 flex-1">{label}</span>
    <ChevronRight
      size={16}
      strokeWidth={1.8}
      aria-hidden="true"
      class={cn("text-muted shrink-0 transition-transform", expanded && "rotate-90")}
    />
  </button>
  <div
    {id}
    hidden={!expanded}
    class="border-border text-muted space-y-3 border-t px-3 py-3 text-xs leading-relaxed"
  >
    {@render children()}
  </div>
</div>
