<script lang="ts">
  /// Host for the SegmentedSelect layout browser test: renders the control at a
  /// fixed pixel width (the real dialog/card inner width) so single-row fit and
  /// label clipping are measurable in WebKit.
  import SegmentedSelect from "$lib/components/ui/SegmentedSelect.svelte";

  type Option = { label: string; value: string };

  let {
    width,
    options,
    value = $bindable(""),
  }: { width: number; options: Option[]; value?: string } = $props();
</script>

<div style={`width:${width}px;`}>
  <SegmentedSelect {options} bind:value testid="fit" ariaLabel="Reasoning effort" />
</div>
