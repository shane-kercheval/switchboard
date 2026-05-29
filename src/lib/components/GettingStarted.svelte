<script lang="ts">
  import HarnessStatusList from "./HarnessStatusList.svelte";
</script>

<div data-testid="getting-started" class="mt-5">
  <h3 class="text-fg text-sm font-semibold">Supported CLIs</h3>
  <p class="text-muted mt-1 text-sm leading-relaxed">
    Install and sign in to the CLIs you want to use.
  </p>
  <div class="mt-3">
    <HarnessStatusList />
  </div>
</div>
