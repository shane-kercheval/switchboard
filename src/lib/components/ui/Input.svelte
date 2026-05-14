<script lang="ts">
  import type { HTMLInputAttributes } from "svelte/elements";
  import { cn } from "$lib/utils";

  type Props = HTMLInputAttributes & {
    value?: string;
  };

  let { class: className, value = $bindable(""), ...rest }: Props = $props();
</script>

<input
  bind:value
  class={cn(
    "h-10 w-full rounded-md border border-neutral-300 bg-white px-3 text-sm",
    "text-neutral-900 placeholder:text-neutral-400",
    "focus-visible:ring-2 focus-visible:ring-neutral-400 focus-visible:outline-none",
    "disabled:cursor-not-allowed disabled:bg-neutral-50 disabled:opacity-50",
    className,
  )}
  {...rest}
/>
