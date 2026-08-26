<script lang="ts">
  import type { HTMLAttributes } from "svelte/elements";
  import { cn } from "$lib/utils";
  import {
    SEGMENTED_CONTAINER_CLASS,
    SEGMENTED_ITEM_ACTIVE_CLASS,
    SEGMENTED_ITEM_CLASS,
  } from "$lib/components/ui/segmentedControl";

  type Option = { label: string; value: string };

  type Props = HTMLAttributes<HTMLDivElement> & {
    value?: string;
    options: Option[];
    disabled?: boolean;
    testid?: string;
    ariaLabel: string;
  };

  let {
    class: className,
    style: styleAttr,
    value = $bindable(""),
    options,
    disabled = false,
    testid,
    ariaLabel,
    ...rest
  }: Props = $props();

  let hoveredValue = $state<string | null>(null);

  function choose(next: string): void {
    if (disabled) return;
    value = next;
  }

  function optionTestId(v: string): string {
    return v === "" ? "no-override" : v;
  }

  // Always one row: one equal-width column per option. A set with more than
  // five options steps down to a smaller font + tighter padding so it still
  // fits on one line instead of wrapping to a second row, which reads as a
  // broken pill. Codex's eight effort levels are the only set that trips it.
  //
  // Deliberately count-only, *not* label-length aware. Antigravity's five model
  // pills briefly needed a length trigger too — "Gemini 3.7 Flash" clipped —
  // but that was at the old `max-w-lg` dialog width. Widening the add-agent and
  // model-settings dialogs to 612px removed the need: measured in WebKit, those
  // five fit with zero overflow at every inner width down to 500px, and only
  // clip at 472px. Shrinking their text now would be paying a permanent
  // legibility cost for a window narrower than ~564px, where the app is barely
  // usable anyway. `segmented-fit.browser.test.ts` holds the line.
  const columnCount = $derived(Math.max(1, options.length));
  const gridStyle = $derived(`grid-template-columns: repeat(${columnCount}, minmax(0, 1fr));`);
  const compact = $derived(options.length > 5);
</script>

<div
  role="radiogroup"
  aria-label={ariaLabel}
  aria-disabled={disabled}
  data-testid={testid}
  data-value={value}
  style={styleAttr === undefined ? gridStyle : `${gridStyle} ${styleAttr}`}
  class={cn(
    SEGMENTED_CONTAINER_CLASS,
    "grid w-full",
    compact && "gap-0.5",
    disabled && "opacity-60",
    className,
  )}
  onpointerleave={() => (hoveredValue = null)}
  onpointercancel={() => (hoveredValue = null)}
  {...rest}
>
  {#each options as option (option.value)}
    {@const selected = value === option.value}
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      {disabled}
      data-testid={testid ? `${testid}-option-${optionTestId(option.value)}` : undefined}
      class={cn(
        SEGMENTED_ITEM_CLASS,
        "flex min-w-0 items-center justify-center truncate text-center",
        compact && "px-1 text-[11px]",
        selected ? SEGMENTED_ITEM_ACTIVE_CLASS : "text-muted",
        !selected && !disabled && hoveredValue === option.value && "bg-control-hover text-fg",
        disabled && "cursor-not-allowed",
      )}
      onpointerenter={() => (hoveredValue = option.value)}
      onclick={() => choose(option.value)}
    >
      {option.label}
    </button>
  {/each}
</div>
