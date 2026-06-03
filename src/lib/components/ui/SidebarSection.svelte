<script lang="ts">
  /// A titled section inside a `SidebarPanel`: an uppercase header label with
  /// an optional `action` (e.g. an add button), above a scrollable body. Fills
  /// the panel's remaining height so the body scrolls independently.
  import type { Snippet } from "svelte";

  type Props = {
    title: string;
    action?: Snippet;
    /// Optional content pinned between the header and the scrollable body (e.g.
    /// a filter/search input that should stay put while the list scrolls).
    subheader?: Snippet;
    children: Snippet;
  };

  let { title, action, subheader, children }: Props = $props();
</script>

<div class="flex min-h-0 flex-1 flex-col">
  <div
    class="text-muted flex h-8 shrink-0 items-center justify-between px-3 text-[11px] leading-none font-semibold tracking-wide uppercase"
  >
    <span>{title}</span>
    {#if action}
      {@render action()}
    {/if}
  </div>
  {#if subheader}
    <div class="shrink-0">
      {@render subheader()}
    </div>
  {/if}
  <div class="flex-1 overflow-y-auto">
    {@render children()}
  </div>
</div>
