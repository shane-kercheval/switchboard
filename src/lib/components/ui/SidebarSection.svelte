<script lang="ts">
  /// A titled section inside a `SidebarPanel`: an uppercase header label with
  /// an optional `action` (e.g. an add button), above a scrollable body. Fills
  /// the panel's remaining height so the body scrolls independently.
  import type { Snippet } from "svelte";

  type Props = {
    title: string;
    action?: Snippet;
    children: Snippet;
  };

  let { title, action, children }: Props = $props();
</script>

<div class="flex min-h-0 flex-1 flex-col">
  <div
    class="border-border text-muted flex shrink-0 items-center justify-between border-b px-4 py-3 text-xs font-semibold tracking-wide uppercase"
  >
    <span>{title}</span>
    {#if action}
      {@render action()}
    {/if}
  </div>
  <div class="flex-1 overflow-y-auto">
    {@render children()}
  </div>
</div>
