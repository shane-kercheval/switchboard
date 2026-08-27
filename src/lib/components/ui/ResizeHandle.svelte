<script lang="ts">
  /// The one drag mechanic behind every vertical resize handle: pointer-down
  /// arms the drag, window pointer-moves turn horizontal travel into a clamped
  /// value, pointer-up commits exactly once, double-click resets to the
  /// consumer's default. Arrow keys give the same adjustment to keyboard users
  /// (the WAI-ARIA window-splitter pattern): each press drafts a step, and the
  /// commit fires once on key release — the pointer contract, transposed.
  /// Consumers own the *geometry mapping* — what the value means (a pixel
  /// width, a pane's share converted from fractions) and how a draft renders —
  /// so pixel consumers stay pixel and fraction consumers stay fraction; only
  /// the interaction is shared.
  ///
  /// `value` and `max` are thunks because both are live geometry: the drag
  /// starts from the *measured* current size (which may come from a CSS
  /// default, not a stored number) and clamps against a container that can
  /// differ per drag. The start value is itself clamped to [min, max()]: a
  /// stored width can exceed the live bound its consumer renders under (CSS
  /// max-width caps it on screen), and an adjustment must start from what the
  /// user sees, never from the invisible stored value. Move/up listeners live
  /// on the window, not the handle — the pointer routinely outruns a 4px strip
  /// mid-drag, and an armed-drag guard makes the window listeners inert the
  /// rest of the time. `pointercancel` and window blur finalize exactly like
  /// pointer-up (commit what's on screen): without that, an interrupted drag
  /// stays armed and the next bare pointer motion resizes with no button held.
  import { cn } from "$lib/utils";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";

  type Props = {
    /// The committed value at drag start, in the consumer's unit (px).
    value: () => number;
    min: number;
    /// Live upper bound, re-read on every move.
    max: () => number;
    /// Which edge of the sized element the handle sits on: dragging away from
    /// the element grows it, so a `start`-edge handle inverts the axis.
    edge?: "start" | "end";
    label: string;
    testid?: string;
    class?: string;
    /// Fires on every move with the clamped value — drive the live layout.
    onDraft?: (value: number) => void;
    /// Fires once per adjustment: on pointer-up/cancel/blur (if the pointer
    /// moved) or on arrow-key release.
    onCommit: (value: number) => void;
    onReset?: () => void;
  };

  let {
    value,
    min,
    max,
    edge = "end",
    label,
    testid,
    class: className,
    onDraft,
    onCommit,
    onReset,
  }: Props = $props();

  const KEYBOARD_STEP_PX = 16;

  let drag: { startX: number; startValue: number; last: number; moved: boolean } | null = null;
  let resizeTooltipSuppressed = $state(false);
  /// Armed by the first arrow press, finalized on key release / blur — the
  /// keyboard counterpart of `drag`, so a held key repeats drafts but persists
  /// once.
  let keyboardDraft: { last: number } | null = null;

  // aria-valuenow/max mirror the thunks, refreshed on focus and on each step —
  // thunk reads aren't reactively trackable, so events drive the attributes.
  let ariaNow = $state<number | null>(null);
  let ariaMax = $state<number | null>(null);

  function clampValue(v: number, lo: number, hi: number): number {
    // Range inverted (container too small for both minimums) → hold the
    // midpoint rather than snapping to either end.
    return hi < lo ? (lo + hi) / 2 : Math.min(hi, Math.max(lo, v));
  }

  function startValue(): number {
    return clampValue(value(), min, max());
  }

  function refreshAria(current?: number): void {
    ariaNow = Math.round(current ?? startValue());
    ariaMax = Math.round(max());
  }

  function manageKeyboardFocus(node: HTMLElement): { destroy: () => void } {
    // Native listeners compose with Tooltip's delegated focus handlers; Svelte
    // attributes here would replace them after `{...props}`.
    const onFocus = (): void => refreshAria();
    node.addEventListener("focus", onFocus);
    node.addEventListener("blur", finalizeKeyboard);
    return {
      destroy(): void {
        node.removeEventListener("focus", onFocus);
        node.removeEventListener("blur", finalizeKeyboard);
      },
    };
  }

  function onPointerDown(event: PointerEvent): void {
    const start = startValue();
    resizeTooltipSuppressed = true;
    drag = { startX: event.clientX, startValue: start, last: start, moved: false };
    event.preventDefault();
  }

  function onWindowPointerMove(event: PointerEvent): void {
    if (drag === null) return;
    const sign = edge === "start" ? -1 : 1;
    const next = clampValue(drag.startValue + sign * (event.clientX - drag.startX), min, max());
    drag = { ...drag, last: next, moved: true };
    onDraft?.(next);
  }

  function finalizeDrag(): void {
    if (drag === null) return;
    if (drag.moved) onCommit(drag.last);
    drag = null;
    if (keyboardDraft === null) resizeTooltipSuppressed = false;
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const sign = edge === "start" ? -1 : 1;
    const direction = event.key === "ArrowRight" ? 1 : -1;
    const current = keyboardDraft?.last ?? startValue();
    const next = clampValue(current + sign * direction * KEYBOARD_STEP_PX, min, max());
    keyboardDraft = { last: next };
    resizeTooltipSuppressed = true;
    refreshAria(next);
    onDraft?.(next);
  }

  function finalizeKeyboard(): void {
    if (keyboardDraft === null) return;
    onCommit(keyboardDraft.last);
    keyboardDraft = null;
    if (drag === null) resizeTooltipSuppressed = false;
  }

  function onKeyup(event: KeyboardEvent): void {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    finalizeKeyboard();
  }

  function onWindowBlur(): void {
    finalizeDrag();
    finalizeKeyboard();
  }
</script>

<svelte:window
  onpointermove={onWindowPointerMove}
  onpointerup={finalizeDrag}
  onpointercancel={finalizeDrag}
  onblur={onWindowBlur}
/>

<!-- Keep the tooltip inside the sized pane; top placement on a full-height
     handle pushes it against the window edge instead of beside the divider. -->
<Tooltip
  side={edge === "start" ? "right" : "left"}
  reopen="fresh-hover"
  suppressed={resizeTooltipSuppressed}
  ignoreNonKeyboardFocus={false}
>
  {#snippet trigger(props)}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div
      {...props}
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuenow={ariaNow}
      aria-valuemin={Math.round(min)}
      aria-valuemax={ariaMax}
      tabindex="0"
      data-testid={testid}
      use:manageKeyboardFocus
      class={cn(
        "focus-visible:ring-focus shrink-0 cursor-col-resize touch-none focus-visible:ring-1 focus-visible:outline-none",
        className,
      )}
      onpointerdown={onPointerDown}
      ondblclick={onReset}
      onkeydown={onKeydown}
      onkeyup={onKeyup}
    ></div>
  {/snippet}
  <!-- The trigger already exposes this identity as its accessible name. -->
  <div class="text-[13px] font-medium" aria-hidden="true">{label}</div>
  <div class="text-primary-fg/70 mt-1 text-[12px]">
    Drag · <span class="font-mono">← →</span> · double-click to reset
  </div>
</Tooltip>
