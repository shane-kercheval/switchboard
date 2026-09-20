<script lang="ts">
  import ForwardSourceChip from "$lib/components/ui/ForwardSourceChip.svelte";
  import type { ForwardReadiness } from "$lib/state/heldForwards.svelte";

  // One forward-source chip in a chosen readiness, for specs about its state
  // indicator. The label is arbitrary text of arbitrary width, which is the
  // point: it puts the indicator on a fractional x offset, as the real chip does.
  let { name, readiness }: { name: string; readiness: ForwardReadiness } = $props();
</script>

<div style="padding: 20px">
  <ForwardSourceChip
    source={{ id: name, name }}
    {readiness}
    onRemove={() => {}}
    currentProjectId="p-1"
  />
</div>
