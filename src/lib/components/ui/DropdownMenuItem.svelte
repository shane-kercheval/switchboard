<script lang="ts">
  /// A single item inside `DropdownMenu`. `onSelect` fires on click or
  /// keyboard activation; bits-ui closes the menu afterwards unless
  /// `closeOnSelect` is false (e.g. an item that reveals an inline confirm and
  /// needs the menu to stay open). Extra attributes (`data-testid`, `title`)
  /// are forwarded to the underlying item.
  import type { Snippet } from "svelte";
  import { DropdownMenu as Bits } from "bits-ui";
  import { cn } from "$lib/utils";

  type Props = {
    onSelect?: () => void;
    disabled?: boolean;
    closeOnSelect?: boolean;
    title?: string;
    class?: string;
    children: Snippet;
    [key: `data-${string}`]: string | undefined;
  };

  let {
    onSelect,
    disabled = false,
    closeOnSelect = true,
    title,
    class: className,
    children,
    ...rest
  }: Props = $props();
</script>

<Bits.Item
  {onSelect}
  {disabled}
  {closeOnSelect}
  {title}
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
