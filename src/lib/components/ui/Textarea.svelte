<script lang="ts">
  import type { HTMLTextareaAttributes } from "svelte/elements";
  import { cn } from "$lib/utils";

  type Props = HTMLTextareaAttributes & {
    value?: string;
  };

  let { class: className, value = $bindable(""), ...rest }: Props = $props();
</script>

<textarea
  bind:value
  class={cn(
    "w-full resize-none rounded-md border border-neutral-300 bg-white px-3 py-2 text-sm",
    "text-neutral-900 placeholder:text-neutral-400",
    "focus-visible:ring-2 focus-visible:ring-neutral-400 focus-visible:outline-none",
    "disabled:cursor-not-allowed disabled:bg-neutral-50 disabled:opacity-50",
    className,
  )}
  {...rest}
></textarea>
