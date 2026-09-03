<script lang="ts">
  import type { HTMLSelectAttributes } from "svelte/elements";
  import { cn } from "$lib/utils";

  type SelectOption = { label: string; value: string; disabled?: boolean };
  type Props = HTMLSelectAttributes & {
    value?: string;
    options: SelectOption[];
    placeholder?: string;
  };

  let {
    class: className,
    value = $bindable(""),
    options,
    placeholder,
    onchange,
    ...rest
  }: Props = $props();
</script>

<select
  bind:value
  {onchange}
  class={cn(
    "border-border bg-raised h-7 w-full cursor-pointer rounded-md border px-2 text-sm",
    "text-fg",
    "focus-visible:ring-focus focus-visible:ring-1 focus-visible:outline-none",
    "disabled:bg-panel disabled:cursor-not-allowed disabled:opacity-50",
    className,
  )}
  {...rest}
>
  {#if placeholder !== undefined}<option value="" disabled>{placeholder}</option>{/if}
  {#each options as option (option.value)}
    <option value={option.value} disabled={option.disabled}>{option.label}</option>
  {/each}
</select>
