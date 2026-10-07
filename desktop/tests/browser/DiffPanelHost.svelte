<script lang="ts">
  import DiffPanel from "$lib/components/DiffPanel.svelte";
  import type { DiffTarget } from "$lib/state/gitView.svelte";

  // DiffPanel fills its parent; give it a definite size so the changed-files list
  // and its rows have real geometry to measure.
  let { target }: { target: DiffTarget } = $props();
</script>

<div style="height: 500px; width: 600px; display: flex;">
  <DiffPanel {target} onClose={() => {}} />
</div>
