<script lang="ts">
  /// Test-only fixture: composes Tooltip so `Tooltip.test.ts` can exercise
  /// both content modes (label-only and children-slot). Not a test file
  /// itself — the leading `_` and `.svelte` extension keep it out of
  /// Vitest's `*.test` glob. Mirrors `_DropdownMenuHarness.svelte`.
  import Tooltip from "./Tooltip.svelte";

  /// `mode` picks which variant the harness renders. Defaults to label
  /// for the common case.
  let { mode = "label" as "label" | "children" }: { mode?: "label" | "children" } = $props();
</script>

{#if mode === "label"}
  <Tooltip label="hello label" shortcut="⌘K">
    {#snippet trigger(props)}
      <button {...props} type="button" data-testid="tt-trigger">trigger</button>
    {/snippet}
  </Tooltip>
{:else}
  <Tooltip>
    {#snippet trigger(props)}
      <button {...props} type="button" data-testid="tt-trigger">trigger</button>
    {/snippet}
    <ul data-testid="tt-rich-content">
      <li>row one</li>
      <li>row two</li>
    </ul>
  </Tooltip>
{/if}
