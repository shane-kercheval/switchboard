<script lang="ts">
  /// A dropdown with far more items than fit the viewport — the shape the forward
  /// picker reaches once it lists every agent, every pane, and every other
  /// project. Exists to prove the shared menu contains its own overflow rather
  /// than running off-screen, which only a real engine can measure.
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";

  const items = Array.from({ length: 60 }, (_, i) => `item-${i}`);
</script>

<div style="height: 100vh; display: flex; align-items: flex-start;">
  <DropdownMenu open triggerTestid="overflow-trigger" contentTestid="overflow-content">
    {#snippet trigger()}open{/snippet}
    {#each items as item (item)}
      <DropdownMenuItem onSelect={() => undefined} data-testid={`row-${item}`}>
        {item}
      </DropdownMenuItem>
    {/each}
  </DropdownMenu>
</div>
