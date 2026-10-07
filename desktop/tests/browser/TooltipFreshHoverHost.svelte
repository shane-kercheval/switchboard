<script lang="ts">
  import Tooltip from "$lib/components/ui/Tooltip.svelte";

  let count = $state(0);
</script>

<button type="button" data-testid="tooltip-before">before</button>
<Tooltip label={`State control ${count}`} reopen="fresh-hover">
  {#snippet trigger(props)}
    <button
      {...props}
      type="button"
      data-testid="tooltip-fresh-trigger"
      onclick={() => (count += 1)}
    >
      state control
    </button>
  {/snippet}
</Tooltip>
<button type="button" data-testid="tooltip-after">after</button>
<div data-testid="tooltip-parking">parking</div>
