<script lang="ts">
  import { ChevronsDownUp, ChevronsUpDown, type IconProps } from "@lucide/svelte";

  type Props = {
    /// Current content state. The glyph communicates the action clicking will take.
    expanded: boolean;
    size?: IconProps["size"];
    strokeWidth?: IconProps["strokeWidth"];
    class?: string;
  };

  let { expanded, size, strokeWidth, class: className }: Props = $props();
</script>

{#if expanded}
  <ChevronsDownUp
    {size}
    {strokeWidth}
    class={className}
    data-icon-action="collapse"
    aria-hidden="true"
  />
{:else}
  <ChevronsUpDown
    {size}
    {strokeWidth}
    class={className}
    data-icon-action="expand"
    aria-hidden="true"
  />
{/if}
