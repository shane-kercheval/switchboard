<script lang="ts">
  /// Test-only fixture: composes DropdownMenu + DropdownMenuItem so a `.test.ts`
  /// can exercise the wrapper (snippet children can't be expressed inline in a
  /// createRawSnippet HTML string). Not a test file itself — the leading `_`
  /// and `.svelte` extension keep it out of the Vitest `*.test` glob.
  import DropdownMenu from "./DropdownMenu.svelte";
  import DropdownMenuItem from "./DropdownMenuItem.svelte";

  let { onSelect }: { onSelect: () => void } = $props();
</script>

<DropdownMenu triggerTestid="dd-trigger" contentTestid="dd-content" triggerLabel="open menu">
  {#snippet trigger()}
    +
  {/snippet}
  <DropdownMenuItem {onSelect} data-testid="dd-item-a">Item A</DropdownMenuItem>
  <DropdownMenuItem data-testid="dd-item-b">Item B</DropdownMenuItem>
</DropdownMenu>
