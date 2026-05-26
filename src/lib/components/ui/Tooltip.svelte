<script lang="ts">
  /// Custom dark tooltip (label + optional keyboard shortcut) wrapping `bits-ui`
  /// Tooltip, so callers get hover/focus open, delay, dismissal, and ARIA for
  /// free instead of the bare native `title`. The trigger delegates to the
  /// caller's own element via the `trigger` snippet (spread `props` onto it), so
  /// there's no wrapper element and no nested button.
  import type { Snippet } from "svelte";
  import { Tooltip as Bits, Portal } from "bits-ui";

  type Props = {
    label: string;
    /// Optional shortcut hint shown under the label (e.g. "⌘↵").
    shortcut?: string;
    side?: "top" | "bottom" | "left" | "right";
    /// Receives the bits-ui trigger props — spread them onto your element.
    trigger: Snippet<[Record<string, unknown>]>;
  };

  let { label, shortcut, side = "top", trigger }: Props = $props();
</script>

<Bits.Provider delayDuration={500}>
  <Bits.Root>
    <Bits.Trigger>
      {#snippet child({ props })}
        {@render trigger(props)}
      {/snippet}
    </Bits.Trigger>
    <Portal>
      <Bits.Content
        {side}
        sideOffset={6}
        data-testid="tooltip-content"
        class="bg-primary text-primary-fg z-50 rounded-lg px-2.5 py-1.5 shadow-[0_10px_28px_rgba(0,0,0,0.20)]"
      >
        <Bits.Arrow class="fill-primary" />
        <div class="text-[13px] font-medium">{label}</div>
        {#if shortcut}
          <div class="text-primary-fg/70 mt-1 font-mono text-[13px]">{shortcut}</div>
        {/if}
      </Bits.Content>
    </Portal>
  </Bits.Root>
</Bits.Provider>
