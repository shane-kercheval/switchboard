<script lang="ts">
  /// A single item inside `DropdownMenu`. `onSelect` fires on click or
  /// keyboard activation; bits-ui closes the menu afterwards. Extra attributes
  /// (e.g. `data-testid`) are forwarded to the underlying item.
  import type { Snippet } from "svelte";
  import { DropdownMenu as Bits } from "bits-ui";
  import { cn } from "$lib/utils";

  type Props = {
    onSelect?: () => void;
    disabled?: boolean;
    class?: string;
    children: Snippet;
    [key: `data-${string}`]: string | undefined;
  };

  let { onSelect, disabled = false, class: className, children, ...rest }: Props = $props();
</script>

<Bits.Item
  {onSelect}
  {disabled}
  class={cn(
    "text-fg flex w-full items-center rounded-md px-2.5 py-1.5 text-left leading-5 outline-none select-none",
    "data-highlighted:bg-panel/80 cursor-pointer",
    "data-disabled:text-muted/50 data-disabled:cursor-not-allowed",
    className,
  )}
  {...rest}
>
  {@render children()}
</Bits.Item>
