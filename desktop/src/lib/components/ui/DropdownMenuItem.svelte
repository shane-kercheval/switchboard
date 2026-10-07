<script lang="ts">
  /// A single item inside `DropdownMenu`. `onSelect` fires on click or
  /// keyboard activation; bits-ui closes the menu afterwards unless
  /// `closeOnSelect` is false (e.g. an item that reveals an inline confirm and
  /// needs the menu to stay open). Extra data attributes are forwarded to the
  /// underlying item.
  import type { Snippet } from "svelte";
  import { DropdownMenu as Bits } from "bits-ui";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { MENU_ITEM_CLASS } from "$lib/components/ui/menuStyles";
  import { cn } from "$lib/utils";

  type Props = {
    onSelect?: () => void;
    disabled?: boolean;
    closeOnSelect?: boolean;
    tooltip?: string;
    /// Multi-line tooltip body, for an item whose explanation doesn't fit the
    /// one-line `tooltip`. Takes precedence when both are set, so a caller can
    /// keep `tooltip` as the plain-string fallback. Sharing one snippet with
    /// another affordance for the same action is the point: two surfaces that
    /// trigger one thing shouldn't describe it differently.
    tooltipContent?: Snippet;
    class?: string;
    children: Snippet;
    [key: `data-${string}`]: string | undefined;
  };

  let {
    onSelect,
    disabled = false,
    closeOnSelect = true,
    tooltip,
    tooltipContent,
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
    class={cn(MENU_ITEM_CLASS, className)}
    {...rest}
  >
    {@render children()}
  </Bits.Item>
{/snippet}

{#if tooltipContent}
  <Tooltip side="left">
    {#snippet trigger(props)}{@render item(props)}{/snippet}
    {@render tooltipContent()}
  </Tooltip>
{:else if tooltip}
  <Tooltip label={tooltip} side="left">
    {#snippet trigger(props)}{@render item(props)}{/snippet}
  </Tooltip>
{:else}
  {@render item()}
{/if}
