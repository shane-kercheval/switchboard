<script lang="ts">
  import type { HTMLSelectAttributes } from "svelte/elements";
  import { cn } from "$lib/utils";

  /// Closed enum-select over a `{label, value}[]` list — the curated-dropdown
  /// primitive for model and effort selection. A native `<select>` deliberately:
  /// real keyboard/ARIA/screen-reader
  /// semantics for free, no open-state to manage, and the value space is a fixed
  /// curated set, so there is no free-text branch to support.
  ///
  /// The option shape is defined locally — a `ui/` primitive stays
  /// domain-agnostic, so feature lists (e.g. `agentSelection`'s) are accepted
  /// structurally rather than via an import that points the dependency the
  /// wrong way.
  type Option = { label: string; value: string };
  type Props = HTMLSelectAttributes & {
    value?: string;
    options: Option[];
  };

  let { class: className, value = $bindable(""), options, ...rest }: Props = $props();
</script>

<select
  bind:value
  class={cn(
    "border-border bg-raised h-7 w-full cursor-pointer rounded-md border px-2 text-sm",
    "text-fg",
    "focus-visible:ring-accent focus-visible:ring-2 focus-visible:outline-none",
    "disabled:bg-panel disabled:cursor-not-allowed disabled:opacity-50",
    className,
  )}
  {...rest}
>
  {#each options as option (option.value)}
    <option value={option.value}>{option.label}</option>
  {/each}
</select>
