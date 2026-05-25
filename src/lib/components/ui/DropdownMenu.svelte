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
  import { cn } from "$lib/utils";

  type Props = {
    open?: boolean;
    trigger: Snippet;
    children: Snippet;
    triggerClass?: string;
    triggerLabel?: string;
    triggerTestid?: string;
    contentClass?: string;
    contentTestid?: string;
    align?: "start" | "center" | "end";
  };

  let {
    open = $bindable(false),
    trigger,
    children,
    triggerClass,
    triggerLabel,
    triggerTestid,
    contentClass,
    contentTestid,
    align = "end",
  }: Props = $props();
</script>

<Bits.Root bind:open>
  <Bits.Trigger class={triggerClass} aria-label={triggerLabel} data-testid={triggerTestid}>
    {@render trigger()}
  </Bits.Trigger>
  <Bits.Portal>
    <Bits.Content
      {align}
      sideOffset={4}
      data-testid={contentTestid}
      class={cn(
        "border-border bg-raised z-50 min-w-44 overflow-hidden rounded-md border py-1 text-sm shadow-lg",
        contentClass,
      )}
    >
      {@render children()}
    </Bits.Content>
  </Bits.Portal>
</Bits.Root>
