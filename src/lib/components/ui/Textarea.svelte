<script lang="ts">
  import type { HTMLTextareaAttributes } from "svelte/elements";
  import { cn } from "$lib/utils";

  type Props = HTMLTextareaAttributes & {
    value?: string;
    ref?: HTMLTextAreaElement;
  };

  let { class: className, value = $bindable(""), ref = $bindable(), ...rest }: Props = $props();
</script>

<textarea
  bind:this={ref}
  bind:value
  class={cn(
    "border-border bg-raised w-full resize-none rounded-md border px-3 py-2 text-sm",
    "text-fg placeholder:text-muted",
    "focus-visible:ring-accent focus-visible:ring-2 focus-visible:outline-none",
    "disabled:bg-panel disabled:cursor-not-allowed disabled:opacity-50",
    className,
  )}
  {...rest}
></textarea>
