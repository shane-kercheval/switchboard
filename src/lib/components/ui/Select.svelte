<script lang="ts">
  import type { HTMLSelectAttributes } from "svelte/elements";
  import { cn } from "$lib/utils";

  type Option = { label: string; value: string };
  type Props = HTMLSelectAttributes & {
    value?: string;
    options: Option[];
  };

  let { class: className, value = $bindable(""), options, ...rest }: Props = $props();
</script>

<select
  bind:value
  class={cn(
    "border-border bg-raised h-7 w-full cursor-pointer rounded-md border px-2 text-sm",
    "text-fg",
    "focus-visible:ring-accent focus-visible:ring-2 focus-visible:outline-none",
    "disabled:bg-panel disabled:cursor-not-allowed disabled:opacity-50",
    className,
  )}
  {...rest}
>
  {#each options as option (option.value)}
    <option value={option.value}>{option.label}</option>
  {/each}
</select>
