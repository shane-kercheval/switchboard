<script lang="ts">
  /// Custom dark tooltip wrapping `bits-ui` Tooltip, so callers get hover/focus
  /// open, delay, dismissal, and ARIA for free instead of the bare native
  /// `title`. The trigger delegates to the caller's own element via the
  /// `trigger` snippet (spread `props` onto it), so there's no wrapper element
  /// and no nested button.
  ///
  /// **Two content modes** (discriminated union — TS rejects passing neither
  /// or both, so a caller can't accidentally render an empty tooltip):
  /// - **Label mode**: `label="..."` renders the single-line bold title.
  ///   Optional `shortcut="⌘↵"` line shown beneath it.
  /// - **Children mode**: the default slot owns the entire content area —
  ///   the caller styles its own layout (rows, lists, etc.). `shortcut` is
  ///   ignored in this mode (the caller's children own the visual stack).
  import type { Snippet } from "svelte";
  import { Tooltip as Bits, Portal } from "bits-ui";

  type Common = {
    side?: "top" | "bottom" | "left" | "right";
    delayDuration?: number;
    skipDelayDuration?: number;
    disableHoverableContent?: boolean;
    /// Receives the bits-ui trigger props — spread them onto your element.
    trigger: Snippet<[Record<string, unknown>]>;
  };
  type LabelProps = Common & {
    label: string;
    /// Keyboard-shortcut hint shown beneath the label (label mode only).
    shortcut?: string;
    children?: never;
  };
  type ChildrenProps = Common & {
    children: Snippet;
    label?: never;
    shortcut?: never;
  };
  type Props = LabelProps | ChildrenProps;

  let {
    side = "top",
    delayDuration = 500,
    skipDelayDuration = 300,
    disableHoverableContent = false,
    trigger,
    ...rest
  }: Props = $props();
</script>

<Bits.Provider {delayDuration} {skipDelayDuration}>
  <Bits.Root {disableHoverableContent}>
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
        {#if rest.children}
          {@render rest.children()}
        {:else}
          <div class="text-[13px] font-medium">{rest.label}</div>
          {#if rest.shortcut}
            <div class="text-primary-fg/70 mt-1 font-mono text-[13px]">{rest.shortcut}</div>
          {/if}
        {/if}
      </Bits.Content>
    </Portal>
  </Bits.Root>
</Bits.Provider>
