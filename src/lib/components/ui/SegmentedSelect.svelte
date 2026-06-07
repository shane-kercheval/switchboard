<script lang="ts">
  import type { HTMLAttributes } from "svelte/elements";
  import { cn } from "$lib/utils";
  import {
    SEGMENTED_CONTAINER_CLASS,
    SEGMENTED_ITEM_ACTIVE_CLASS,
    SEGMENTED_ITEM_CLASS,
    SEGMENTED_ITEM_INACTIVE_CLASS,
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

  function choose(next: string): void {
    if (disabled) return;
    value = next;
  }

  function optionTestId(v: string): string {
    return v === "" ? "no-override" : v;
  }

  const columnCount = $derived(
    Math.max(1, options.length > 6 ? Math.ceil(options.length / 2) : options.length),
  );
  const gridStyle = $derived(`grid-template-columns: repeat(${columnCount}, minmax(0, 1fr));`);
</script>

<div
  role="radiogroup"
  aria-label={ariaLabel}
  aria-disabled={disabled}
  data-testid={testid}
  data-value={value}
  style={styleAttr === undefined ? gridStyle : `${gridStyle} ${styleAttr}`}
  class={cn(SEGMENTED_CONTAINER_CLASS, "grid w-full", disabled && "opacity-60", className)}
  {...rest}
>
  {#each options as option (option.value)}
    {@const selected = value === option.value}
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      {disabled}
      title={option.label}
      data-testid={testid ? `${testid}-option-${optionTestId(option.value)}` : undefined}
      class={cn(
        SEGMENTED_ITEM_CLASS,
        "flex min-w-0 items-center justify-center truncate text-center",
        selected ? SEGMENTED_ITEM_ACTIVE_CLASS : SEGMENTED_ITEM_INACTIVE_CLASS,
        disabled && "cursor-not-allowed",
      )}
      onclick={() => choose(option.value)}
    >
      {option.label}
    </button>
  {/each}
</div>
