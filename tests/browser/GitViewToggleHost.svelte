<script lang="ts">
  import GitView from "$lib/components/GitView.svelte";

  let visible = $state(true);
</script>

<button type="button" data-testid="toggle-git-view" onclick={() => (visible = !visible)}>
  Toggle Git view
</button>
<div style="display: flex; width: 100vw; height: 600px; overflow: hidden;">
  {#if visible}
    <GitView />
  {/if}
</div>
