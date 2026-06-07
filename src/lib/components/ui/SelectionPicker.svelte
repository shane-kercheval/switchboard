<script lang="ts">
  import type { HTMLAttributes } from "svelte/elements";
  import Select from "$lib/components/ui/Select.svelte";
  import SegmentedSelect from "$lib/components/ui/SegmentedSelect.svelte";

  type Option = { label: string; value: string };
  type Props = HTMLAttributes<HTMLDivElement> & {
    value?: string;
    options: Option[];
    disabled?: boolean;
    testid?: string;
    ariaLabel: string;
    presentation?: "auto" | "segmented";
  };

  let {
    value = $bindable(""),
    options,
    disabled = false,
    testid,
    ariaLabel,
    presentation = "auto",
    ...rest
  }: Props = $props();

  const usesSegmented = $derived(presentation === "segmented" || options.length <= 4);
</script>

{#if usesSegmented}
  <SegmentedSelect bind:value {options} {disabled} {testid} {ariaLabel} {...rest} />
{:else}
  <Select
    bind:value
    {options}
    {disabled}
    data-testid={testid}
    data-value={value}
    aria-label={ariaLabel}
  />
{/if}
