<script lang="ts">
  /// The three-pane horizontal layout (left | center | right), encoding the
  /// flex + overflow behavior once. Thin composition: each pane is a snippet,
  /// the right pane optional. App-level chrome (banners, footer, modals) stays
  /// in the consumer — this owns only the pane row.
  import type { Snippet } from "svelte";

  type Props = {
    left: Snippet;
    center: Snippet;
    right?: Snippet;
    centerTestid?: string;
  };

  let { left, center, right, centerTestid }: Props = $props();
</script>

<div class="flex flex-1 overflow-hidden">
  {@render left()}
  <div class="bg-raised flex flex-1 flex-col overflow-hidden" data-testid={centerTestid}>
    {@render center()}
  </div>
  {#if right}
    {@render right()}
  {/if}
</div>
