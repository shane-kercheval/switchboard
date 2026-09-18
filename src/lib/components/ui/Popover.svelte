<script lang="ts">
  /// Click-open detail surface for structured, inspectable content. Tooltips
  /// stay reserved for short hints; a popover may contain sections, scrolling,
  /// and nested controls while retaining keyboard focus and Escape dismissal.
  import type { Snippet } from "svelte";
  import { Popover as Bits } from "bits-ui";
  import { cn } from "$lib/utils";

  type Props = {
    open?: boolean;
    trigger: Snippet;
    children: Snippet;
    side?: "top" | "bottom" | "left" | "right";
    align?: "start" | "center" | "end";
    triggerClass?: string;
    triggerLabel?: string;
    triggerTestid?: string;
    contentClass?: string;
    contentTestid?: string;
  };

  let {
    open = $bindable(false),
    trigger,
    children,
    side = "left",
    align = "start",
    triggerClass,
    triggerLabel,
    triggerTestid,
    contentClass,
    contentTestid,
  }: Props = $props();
</script>

<Bits.Root bind:open>
  <Bits.Trigger class={triggerClass} aria-label={triggerLabel} data-testid={triggerTestid}>
    {@render trigger()}
  </Bits.Trigger>
  <Bits.Portal>
    <Bits.Content
      {side}
      {align}
      sideOffset={8}
      collisionPadding={12}
      data-testid={contentTestid}
      class={cn(
        "border-border/90 bg-raised z-50 max-h-[min(34rem,var(--bits-popover-content-available-height,34rem))] w-[min(24rem,calc(100vw-1.5rem))] overflow-y-auto rounded-lg border p-3 text-[13px] shadow-[0_10px_28px_rgba(0,0,0,0.12)] outline-none",
        contentClass,
      )}
    >
      {@render children()}
    </Bits.Content>
  </Bits.Portal>
</Bits.Root>
