<script lang="ts">
  import type { HTMLInputAttributes } from "svelte/elements";
  import { cn } from "$lib/utils";

  type Props = HTMLInputAttributes & {
    value?: string;
  };

  let { class: className, value = $bindable(""), ...rest }: Props = $props();
</script>

<!-- Single-line inputs are names / search / identifiers, not prose, so macOS
     autocorrect + auto-capitalization fight the user (e.g. mangling a typed
     prompt name). Disabled by default; `...rest` lets a prose field opt back in. -->
<input
  bind:value
  autocorrect="off"
  autocapitalize="off"
  spellcheck="false"
  class={cn(
    "border-border bg-raised h-7 w-full rounded-full border px-2.5 text-sm",
    "text-fg placeholder:text-muted",
    "focus-visible:ring-accent focus-visible:ring-2 focus-visible:outline-none",
    "disabled:bg-panel disabled:cursor-not-allowed disabled:opacity-50",
    className,
  )}
  {...rest}
/>
