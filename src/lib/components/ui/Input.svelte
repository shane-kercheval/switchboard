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
    "border-border bg-raised h-9 w-full rounded-full border px-2.5 text-sm",
    "text-fg placeholder:text-muted",
    "focus-visible:ring-accent focus-visible:ring-2 focus-visible:outline-none",
    "disabled:bg-panel disabled:cursor-not-allowed disabled:opacity-50",
    className,
  )}
  {...rest}
/>
