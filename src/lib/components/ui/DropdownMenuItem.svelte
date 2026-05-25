<script lang="ts">
  /// A single item inside `DropdownMenu`. `onSelect` fires on click or
  /// keyboard activation; bits-ui closes the menu afterwards. Extra attributes
  /// (e.g. `data-testid`) are forwarded to the underlying item.
  import type { Snippet } from "svelte";
  import { DropdownMenu as Bits } from "bits-ui";
  import { cn } from "$lib/utils";

  type Props = {
    onSelect?: () => void;
    class?: string;
    children: Snippet;
    [key: `data-${string}`]: string | undefined;
  };

  let { onSelect, class: className, children, ...rest }: Props = $props();
</script>

<Bits.Item
  {onSelect}
  class={cn(
    "text-fg flex w-full cursor-pointer items-center rounded-md px-2.5 py-1.5 text-left leading-5 outline-none select-none",
    "data-highlighted:bg-panel/80",
    className,
  )}
  {...rest}
>
  {@render children()}
</Bits.Item>
