<script lang="ts">
  /// A dialog whose body is deliberately taller than any viewport, so the
  /// height cap in `Dialog.svelte` is exercised rather than assumed. The
  /// filler is inline-styled rather than Tailwind-classed so the test reads as
  /// "content is 4000px tall" without a class lookup.
  import Dialog from "$lib/components/ui/Dialog.svelte";

  let open = $state(true);
</script>

<Dialog bind:open title="Tall dialog">
  <div data-testid="tall-body" style="height: 4000px;">
    <button type="button" data-testid="body-first">first</button>
    <div style="height: 3900px;"></div>
    <button type="button" data-testid="body-last">last</button>
  </div>
</Dialog>
