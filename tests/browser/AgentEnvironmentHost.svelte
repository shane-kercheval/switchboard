<script lang="ts">
  /// Browser-test host: the Environment row inside a card-width column, so its
  /// single-line environment trigger can be measured against real text metrics. The
  /// padding mirrors the sidebar card's (`px-2.5` either side) so the measured
  /// content width is the width the row actually gets, not the raw sidebar
  /// width.
  import AgentEnvironment from "$lib/components/AgentEnvironment.svelte";
  import type { SessionInventory } from "$lib/types";

  type Props = { width: number; inventory: SessionInventory };
  let { width, inventory }: Props = $props();
</script>

<div style="width: {width}px;" class="bg-panel">
  <div class="px-2.5">
    <AgentEnvironment {inventory} />
  </div>
</div>
