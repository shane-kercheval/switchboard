<script lang="ts">
  /// A single item inside `DropdownMenu`. `onSelect` fires on click or
  /// keyboard activation; bits-ui closes the menu afterwards unless
  /// `closeOnSelect` is false (e.g. an item that reveals an inline confirm and
  /// needs the menu to stay open). Extra data attributes are forwarded to the
  /// underlying item.
  import type { Snippet } from "svelte";
  import { DropdownMenu as Bits } from "bits-ui";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { cn } from "$lib/utils";

  type Props = {
    onSelect?: () => void;
    disabled?: boolean;
    closeOnSelect?: boolean;
    tooltip?: string;
    class?: string;
    children: Snippet;
    /// Disclosure state for an item that expands in place (used with
    /// `closeOnSelect={false}`), so a screen reader reports it as expandable
    /// rather than as a plain action.
    "aria-expanded"?: boolean;
    [key: `data-${string}`]: string | undefined;
  };

  let {
    onSelect,
    disabled = false,
    closeOnSelect = true,
    tooltip,
    class: className,
    children,
    ...rest
  }: Props = $props();
</script>

{#snippet item(props: Record<string, unknown> = {})}
  <Bits.Item
    {...props}
    {onSelect}
    {disabled}
    {closeOnSelect}
    class={cn(
      "text-fg flex w-full items-center rounded-md px-2.5 py-1.5 text-left leading-5 outline-none select-none",
      "data-highlighted:bg-hover cursor-pointer",
      "data-disabled:text-muted/50 data-disabled:cursor-not-allowed",
      className,
    )}
    {...rest}
  >
    {@render children()}
  </Bits.Item>
{/snippet}

{#if tooltip}
  <Tooltip label={tooltip} side="left">
    {#snippet trigger(props)}{@render item(props)}{/snippet}
  </Tooltip>
{:else}
  {@render item()}
{/if}
