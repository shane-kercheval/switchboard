<script lang="ts">
  import { cn } from "$lib/utils";

  /// Controlled on/off toggle (`role="switch"`). The parent owns the state and
  /// flips it in `onclick`; this renders the track/knob and the disabled
  /// treatment so those have exactly one definition.
  type Props = {
    checked: boolean;
    disabled?: boolean;
    ariaLabel: string;
    testid?: string;
    onclick: () => void;
  };

  let { checked, disabled = false, ariaLabel, testid, onclick }: Props = $props();

  /// Guarded rather than relying on the `disabled` attribute alone, matching
  /// `SegmentedSelect`: the attribute stops a real pointer click, but not a
  /// programmatic dispatch, and a caller that swaps to `aria-disabled` for
  /// focusability would silently lose the protection entirely.
  function activate(): void {
    if (disabled) return;
    onclick();
  }
</script>

<button
  type="button"
  role="switch"
  {disabled}
  aria-checked={checked}
  aria-label={ariaLabel}
  data-testid={testid}
  class={cn(
    "relative mt-0.5 inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors outline-none",
    disabled ? "cursor-not-allowed opacity-50" : "cursor-pointer",
    checked ? "bg-accent" : "bg-active",
  )}
  onclick={activate}
>
  <span
    class={cn(
      "bg-raised inline-block h-4 w-4 transform rounded-full transition-transform",
      checked ? "translate-x-4" : "translate-x-0.5",
    )}
  ></span>
</button>
