<script lang="ts">
  /// Thin wrapper around `bits-ui` DropdownMenu — single import surface so
  /// callers get focus management, keyboard navigation, escape/click-outside
  /// dismissal, and ARIA semantics for free instead of hand-rolling a menu.
  ///
  /// Caller supplies a `trigger` snippet (the clickable affordance) and
  /// `children` (the menu items — use `DropdownMenuItem`). `open` is two-way
  /// bindable for callers that need to drive it.
  import type { Snippet } from "svelte";
  import { DropdownMenu as Bits } from "bits-ui";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { cn } from "$lib/utils";

  type Props = {
    open?: boolean;
    trigger: Snippet;
    children: Snippet;
    triggerClass?: string;
    triggerLabel?: string;
    triggerTestid?: string;
    triggerTabindex?: number;
    tooltipLabel?: string;
    tooltipSide?: "top" | "bottom" | "left" | "right";
    contentClass?: string;
    contentTestid?: string;
    align?: "start" | "center" | "end";
    onOpenChange?: (open: boolean) => void;
  };

  let {
    open = $bindable(false),
    trigger: renderTrigger,
    children,
    triggerClass,
    triggerLabel,
    triggerTestid,
    triggerTabindex,
    tooltipLabel,
    tooltipSide = "top",
    contentClass,
    contentTestid,
    align = "end",
    onOpenChange,
  }: Props = $props();

  let previousOpen = $state(open);

  $effect(() => {
    if (open === previousOpen) return;
    previousOpen = open;
    onOpenChange?.(open);
  });
</script>

<Bits.Root bind:open>
  {#if tooltipLabel}
    <Tooltip label={tooltipLabel} side={tooltipSide}>
      {#snippet trigger(props)}
        <Bits.Trigger
          {...props}
          class={triggerClass}
          aria-label={triggerLabel}
          data-testid={triggerTestid}
          tabindex={triggerTabindex}
        >
          {@render renderTrigger()}
        </Bits.Trigger>
      {/snippet}
    </Tooltip>
  {:else}
    <Bits.Trigger
      class={triggerClass}
      aria-label={triggerLabel}
      data-testid={triggerTestid}
      tabindex={triggerTabindex}
    >
      {@render renderTrigger()}
    </Bits.Trigger>
  {/if}
  <Bits.Portal>
    <Bits.Content
      {align}
      sideOffset={4}
      data-testid={contentTestid}
      class={cn(
        "border-border/90 bg-raised z-50 min-w-44 rounded-lg border p-1 text-[13px] shadow-[0_10px_28px_rgba(0,0,0,0.10)] outline-none focus:outline-none",
        contentClass,
      )}
    >
      {@render children()}
    </Bits.Content>
  </Bits.Portal>
</Bits.Root>
